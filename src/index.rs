//! The in-memory candidate index: trigram -> set of file IDs, plus a file table.
//!
//! This is the "narrow the candidate set" half of rgx. Building, querying, incremental update, and
//! persistence are kept separable from the walk and from ripgrep so each can be tested in isolation.
//!
//! Incremental updates are deliberately simple and lean on the confirm step for correctness: when a
//! file changes we add its *current* trigrams (so no new-trigram query can miss it); trigrams that
//! the file no longer contains linger in their posting lists, which only ever produces an extra
//! candidate that ripgrep then filters out. Deleted files are tombstoned (`live = false`) so they
//! stop being candidates. See `docs/index-and-storage.md` sections 3.1 and 5.

use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;
use roaring::RoaringBitmap;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::query::Query;
use crate::trigram::{self, Trigram};

/// Snapshot magic + format version. Bump to invalidate (load returns an error -> caller rebuilds).
const SNAPSHOT_MAGIC: &[u8; 8] = b"RGXIDX01";

/// Bytes from the start of a file scanned for a NUL to decide "binary from the start". Conservative:
/// if a NUL is in here, ripgrep also treats the file as binary and prints nothing for it in a
/// recursive search, so giving it no postings is sound. Deeper NULs are handled by the confirm
/// step. See `docs/index-and-storage.md` section 3.1.
const BINARY_SNIFF_BYTES: usize = 1024;

/// A file known to the index. `live` is false once the file is deleted (tombstoned).
struct FileEntry {
    path: PathBuf,
    size: u64,
    mtime_ns: u64,
    live: bool,
}

#[derive(Default)]
pub struct Index {
    entries: Vec<FileEntry>,
    path_to_id: FxHashMap<PathBuf, u32>,
    postings: FxHashMap<u32, RoaringBitmap>,
}

impl Index {
    /// Number of live (non-deleted) files known to the index.
    pub fn file_count(&self) -> usize {
        self.entries.iter().filter(|e| e.live).count()
    }

    pub fn trigram_count(&self) -> usize {
        self.postings.len()
    }

    /// Build an index over `root`, honoring the same ignore rules ripgrep uses by default.
    pub fn build(root: impl AsRef<Path>) -> Index {
        let paths = walk_files(root.as_ref());
        Self::from_paths(&paths, &AtomicUsize::new(0))
    }

    /// Build an index from an already-walked path set, bumping `progress` once per file processed so
    /// a watcher can show a climbing count during a cold build.
    pub fn from_paths(paths: &[PathBuf], progress: &AtomicUsize) -> Index {
        let (metas, postings) = index_files(paths, progress);
        let entries: Vec<FileEntry> = paths
            .iter()
            .cloned()
            .zip(metas)
            .map(|(path, (size, mtime_ns))| FileEntry {
                path,
                size,
                mtime_ns,
                live: true,
            })
            .collect();
        let path_to_id = entries
            .iter()
            .enumerate()
            .map(|(id, e)| (e.path.clone(), id as u32))
            .collect();
        Index {
            entries,
            path_to_id,
            postings,
        }
    }

    /// Apply a set of changed and removed paths to the resident index (incremental update).
    pub fn apply_changes(&mut self, changed: &[PathBuf], removed: &[PathBuf]) {
        for path in removed {
            if let Some(&id) = self.path_to_id.get(path) {
                self.entries[id as usize].live = false;
            }
        }
        let mut seen = FxHashSet::default();
        for path in changed {
            // Stat BEFORE read: if the file is rewritten between our read and stat, we must record an
            // mtime no newer than the bytes we indexed, so the next reconcile still sees a change and
            // re-indexes. Stat-after-read could store the new mtime over old trigrams and miss it.
            let (size, mtime_ns) = stat(path);
            let Ok(bytes) = std::fs::read(path) else {
                // Unreadable now (perhaps deleted between event and handling): tombstone if known.
                if let Some(&id) = self.path_to_id.get(path) {
                    self.entries[id as usize].live = false;
                }
                continue;
            };
            let id = self.intern(path, size, mtime_ns);
            if collect_trigrams(&bytes, &mut seen) {
                for &t in &seen {
                    self.postings
                        .entry(trigram::pack(t))
                        .or_default()
                        .insert(id);
                }
            }
        }
    }

    /// Reconcile the index against the current tree by `(size, mtime)`: index new/changed files and
    /// tombstone vanished ones. Used at daemon startup to catch changes made while it was down.
    /// Returns the number of files re-indexed plus removed.
    pub fn reconcile(&mut self, root: impl AsRef<Path>) -> usize {
        let walked = walk_files(root.as_ref());
        let walked_set: rustc_hash::FxHashSet<&Path> = walked.iter().map(|p| p.as_path()).collect();
        let mut changed = Vec::new();
        for p in &walked {
            match self.path_to_id.get(p) {
                None => changed.push(p.clone()),
                Some(&id) => {
                    let e = &self.entries[id as usize];
                    let (size, mtime_ns) = stat(p);
                    if !e.live || e.size != size || e.mtime_ns != mtime_ns {
                        changed.push(p.clone());
                    }
                }
            }
        }
        let removed: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|e| e.live && !walked_set.contains(e.path.as_path()))
            .map(|e| e.path.clone())
            .collect();
        let n = changed.len() + removed.len();
        self.apply_changes(&changed, &removed);
        n
    }

    /// Get the id for `path`, creating a live entry if new; refresh metadata and revive if known.
    fn intern(&mut self, path: &Path, size: u64, mtime_ns: u64) -> u32 {
        if let Some(&id) = self.path_to_id.get(path) {
            let e = &mut self.entries[id as usize];
            e.size = size;
            e.mtime_ns = mtime_ns;
            e.live = true;
            return id;
        }
        let id = self.entries.len() as u32;
        self.entries.push(FileEntry {
            path: path.to_path_buf(),
            size,
            mtime_ns,
            live: true,
        });
        self.path_to_id.insert(path.to_path_buf(), id);
        id
    }

    /// Resolve a trigram query to candidate files (a sound superset of the real matches). A fallback
    /// query (no usable trigram) simply makes *every* live file a candidate — there is no separate
    /// "scan the tree" path; ripgrep then confirms over whatever this returns.
    pub fn candidates(&self, query: &Query) -> Vec<&Path> {
        let bitmap = self.eval(query);
        bitmap
            .iter()
            .filter(|&id| self.entries[id as usize].live)
            .map(|id| self.entries[id as usize].path.as_path())
            .collect()
    }

    /// File/dir name lookup (fd/find-style): live paths whose string contains `needle`. Sorted before
    /// truncation so the result is deterministic and `limit` keeps a stable (path-ordered) prefix
    /// rather than an arbitrary subset of entry-insertion order.
    pub fn find(&self, needle: &str, limit: usize) -> Vec<&Path> {
        let mut hits: Vec<&Path> = self
            .entries
            .iter()
            .filter(|e| e.live && e.path.to_string_lossy().contains(needle))
            .map(|e| e.path.as_path())
            .collect();
        hits.sort_unstable();
        hits.truncate(limit);
        hits
    }

    /// Approximate resident size of the posting lists (serialized roaring bytes), for status.
    pub fn memory_bytes(&self) -> u64 {
        self.postings
            .values()
            .map(|b| b.serialized_size() as u64)
            .sum()
    }

    /// Evaluate a query to the set of matching file IDs via roaring set algebra. A fallback
    /// (`Query::All`) yields every file id.
    fn eval(&self, query: &Query) -> RoaringBitmap {
        match query {
            Query::All => self.all_ids(),
            Query::Tri(t) => self
                .postings
                .get(&trigram::pack(*t))
                .cloned()
                .unwrap_or_default(),
            // An empty AND is the identity "match all", not "match nothing" — guard the soundness
            // invariant even though `Query::for_pattern` never builds an empty And/Or today.
            Query::And(qs) => qs
                .iter()
                .map(|q| self.eval(q))
                .reduce(|a, b| a & b)
                .unwrap_or_else(|| self.all_ids()),
            Query::Or(qs) => qs
                .iter()
                .map(|q| self.eval(q))
                .reduce(|a, b| a | b)
                .unwrap_or_default(),
        }
    }

    fn all_ids(&self) -> RoaringBitmap {
        (0..self.entries.len() as u32).collect()
    }

    /// Serialize the index to `path` (atomic via temp file + rename).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let tmp = path.with_extension("tmp");
        let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        w.write_all(SNAPSHOT_MAGIC)?;
        write_u64(&mut w, self.entries.len() as u64)?;
        for e in &self.entries {
            w.write_all(&[e.live as u8])?;
            write_u64(&mut w, e.size)?;
            write_u64(&mut w, e.mtime_ns)?;
            let pb = e.path.as_os_str().as_bytes();
            write_u32(&mut w, pb.len() as u32)?;
            w.write_all(pb)?;
        }
        write_u64(&mut w, self.postings.len() as u64)?;
        let mut buf = Vec::new();
        for (&key, bm) in &self.postings {
            write_u32(&mut w, key)?;
            buf.clear();
            bm.serialize_into(&mut buf)?;
            write_u32(&mut w, buf.len() as u32)?;
            w.write_all(&buf)?;
        }
        w.flush()?;
        drop(w);
        std::fs::rename(&tmp, path).context("rename snapshot into place")?;
        Ok(())
    }

    /// Load an index snapshot from `path`. Errors (incl. version mismatch) signal "rebuild".
    pub fn load(path: impl AsRef<Path>) -> Result<Index> {
        let mut r = std::io::BufReader::new(std::fs::File::open(path)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != SNAPSHOT_MAGIC {
            bail!("snapshot version mismatch");
        }
        let n = read_u64(&mut r)? as usize;
        let mut entries = Vec::with_capacity(n);
        let mut path_to_id = FxHashMap::default();
        for id in 0..n {
            let mut live = [0u8; 1];
            r.read_exact(&mut live)?;
            let size = read_u64(&mut r)?;
            let mtime_ns = read_u64(&mut r)?;
            let plen = read_u32(&mut r)? as usize;
            let mut pb = vec![0u8; plen];
            r.read_exact(&mut pb)?;
            let path = PathBuf::from(std::ffi::OsStr::from_bytes(&pb));
            path_to_id.insert(path.clone(), id as u32);
            entries.push(FileEntry {
                path,
                size,
                mtime_ns,
                live: live[0] != 0,
            });
        }
        let np = read_u64(&mut r)? as usize;
        let n_entries = entries.len() as u32;
        let mut postings = FxHashMap::default();
        for _ in 0..np {
            let key = read_u32(&mut r)?;
            let blen = read_u32(&mut r)? as usize;
            let mut bb = vec![0u8; blen];
            r.read_exact(&mut bb)?;
            let bm = RoaringBitmap::deserialize_from(&bb[..])?;
            // A posting referencing a file id past the entries table means a corrupt or foreign
            // snapshot; reject it so a query can't panic indexing `entries[id]` (caller rebuilds).
            if bm.max().is_some_and(|m| m >= n_entries) {
                bail!("snapshot posting references out-of-range file id");
            }
            postings.insert(key, bm);
        }
        Ok(Index {
            entries,
            path_to_id,
            postings,
        })
    }
}

fn write_u32(w: &mut impl Write, v: u32) -> std::io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn write_u64(w: &mut impl Write, v: u64) -> std::io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_u64(r: &mut impl Read) -> std::io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// Collect the files ripgrep would search under `root`, sorted so file IDs are deterministic.
pub fn walk_files(root: &Path) -> Vec<PathBuf> {
    let found = Mutex::new(Vec::<PathBuf>::new());
    WalkBuilder::new(root).build_parallel().run(|| {
        let found = &found;
        Box::new(move |res| {
            if let Ok(entry) = res
                && entry.file_type().is_some_and(|t| t.is_file())
            {
                found.lock().unwrap().push(entry.into_path());
            }
            WalkState::Continue
        })
    });
    let mut paths = found.into_inner().unwrap();
    paths.sort();
    paths
}

/// Read each file once: collect (size, mtime) per file (ordered) and accumulate distinct trigrams
/// into sharded posting lists in parallel. `progress` is bumped once per file (for watchers).
fn index_files(
    paths: &[PathBuf],
    progress: &AtomicUsize,
) -> (Vec<(u64, u64)>, FxHashMap<u32, RoaringBitmap>) {
    const SHARDS: usize = 256;
    const TRIGRAM_WORDS: usize = (1 << 24) / 64; // one bit per possible 24-bit trigram
    let shards: Vec<Mutex<FxHashMap<u32, RoaringBitmap>>> = (0..SHARDS)
        .map(|_| Mutex::new(FxHashMap::default()))
        .collect();

    // Per-worker scratch reused across files: a sparse bitset that dedups a file's trigrams by
    // bit-test (24-bit keys are dense, so this beats hashing), the resulting distinct keys, and the
    // per-shard grouping buffers. The bitset is cleared via the distinct list, so clearing is O(set
    // bits), not O(2^24).
    let init = || {
        (
            vec![0u64; TRIGRAM_WORDS],
            Vec::<u32>::new(),
            vec![Vec::<u32>::new(); SHARDS],
        )
    };
    let metas: Vec<(u64, u64)> = paths
        .par_iter()
        .enumerate()
        .map_init(init, |(bits, distinct, by_shard), (id, path)| {
            progress.fetch_add(1, Ordering::Relaxed);
            let id = id as u32;
            let (size, mtime_ns) = stat(path);
            if let Ok(bytes) = std::fs::read(path)
                && !is_binary_from_start(&bytes)
            {
                distinct.clear();
                trigram::for_each(&bytes, |t| {
                    let key = trigram::pack(t);
                    let (w, b) = ((key >> 6) as usize, key & 63);
                    if bits[w] & (1u64 << b) == 0 {
                        bits[w] |= 1u64 << b;
                        distinct.push(key);
                    }
                });
                by_shard.iter_mut().for_each(Vec::clear);
                for &key in distinct.iter() {
                    by_shard[(key as usize) & (SHARDS - 1)].push(key);
                }
                for (s, keys) in by_shard.iter().enumerate() {
                    if keys.is_empty() {
                        continue;
                    }
                    let mut g = shards[s].lock().unwrap();
                    for &key in keys {
                        g.entry(key).or_default().insert(id);
                    }
                }
                for &key in distinct.iter() {
                    bits[(key >> 6) as usize] &= !(1u64 << (key & 63));
                }
            }
            (size, mtime_ns)
        })
        .collect();

    let mut merged = FxHashMap::default();
    for shard in shards {
        merged.extend(shard.into_inner().unwrap());
    }
    (metas, merged)
}

fn stat(path: &Path) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            (m.len(), mtime)
        }
        Err(_) => (0, 0),
    }
}

/// True if a NUL appears within the conservative sniff window — ripgrep suppresses such files
/// entirely in a recursive search, so giving them no postings is sound (see [`BINARY_SNIFF_BYTES`]).
fn is_binary_from_start(bytes: &[u8]) -> bool {
    memchr::memchr(0, &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)]).is_some()
}

/// Fill `seen` with the distinct trigrams of `bytes`, returning whether the file should be indexed.
/// Binary-from-start files get no postings (the one place this decision is made, shared by the cold
/// build and incremental updates so they can't drift).
fn collect_trigrams(bytes: &[u8], seen: &mut FxHashSet<Trigram>) -> bool {
    if is_binary_from_start(bytes) {
        return false;
    }
    seen.clear();
    trigram::for_each(bytes, |t| {
        seen.insert(t);
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Options;

    fn write(dir: &Path, name: &str, content: &[u8]) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    fn names(c: Vec<&Path>) -> Vec<String> {
        let mut v: Vec<String> = c
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn build_candidates_and_binary_skip() {
        let tmp = std::env::temp_dir().join(format!("rgx_idx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write(&tmp, "a.txt", b"the quick brown fox\nNEEDLE here\n");
        write(&tmp, "b.txt", b"no match in this file at all\n");
        write(&tmp, "c.txt", b"another NEEDLE appears\n");
        write(&tmp, "bin.dat", b"\x00\x00NEEDLE inside binary\x00");

        let idx = Index::build(&tmp);
        assert_eq!(idx.file_count(), 4);
        let q = Query::for_pattern("NEEDLE", Options::default());
        let n = names(idx.candidates(&q));
        assert!(n.contains(&"a.txt".to_string()) && n.contains(&"c.txt".to_string()));
        assert!(!n.contains(&"b.txt".to_string()) && !n.contains(&"bin.dat".to_string()));

        // A fallback query (no usable trigram) makes every live file a candidate, incl. bin.dat.
        let fb = names(idx.candidates(&Query::for_pattern("a.", Options::default())));
        assert_eq!(fb, vec!["a.txt", "b.txt", "bin.dat", "c.txt"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn incremental_add_change_remove() {
        let tmp = std::env::temp_dir().join(format!("rgx_inc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write(&tmp, "a.txt", b"WIDGET alpha\n");
        let mut idx = Index::build(&tmp);
        let q = Query::for_pattern("WIDGET", Options::default());
        assert_eq!(names(idx.candidates(&q)), vec!["a.txt"]);

        // Add a new file containing WIDGET.
        write(&tmp, "b.txt", b"WIDGET beta\n");
        idx.apply_changes(&[tmp.join("b.txt")], &[]);
        let mut got = names(idx.candidates(&q));
        got.sort();
        assert_eq!(got, vec!["a.txt", "b.txt"]);

        // Remove a.txt -> tombstoned, no longer a candidate.
        std::fs::remove_file(tmp.join("a.txt")).unwrap();
        idx.apply_changes(&[], &[tmp.join("a.txt")]);
        assert_eq!(names(idx.candidates(&q)), vec!["b.txt"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn snapshot_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("rgx_snap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write(&tmp, "a.txt", b"SNAPSHOT token here\n");
        write(&tmp, "b.txt", b"other content\n");
        let idx = Index::build(&tmp);
        let snap = tmp.join("index.bin");
        idx.save(&snap).unwrap();
        let loaded = Index::load(&snap).unwrap();
        assert_eq!(loaded.file_count(), idx.file_count());
        let q = Query::for_pattern("SNAPSHOT", Options::default());
        assert_eq!(names(loaded.candidates(&q)), vec!["a.txt"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
