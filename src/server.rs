//! The per-project daemon: holds the index resident in RAM, keeps it fresh, and answers queries over
//! a local IPC endpoint (an AF_UNIX socket on Unix, a loopback-TCP port on Windows — see
//! [`crate::transport`]). Owning that endpoint *is* the single-instance lock — a second daemon that
//! loses the race exits. The daemon serves immediately: a warm start loads the snapshot and answers
//! at once; a cold start answers via a full ripgrep scan (the correct fallback) until the first
//! build finishes. See `docs/indexing.md` and `docs/index-and-storage.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use notify_debouncer_full::new_debouncer;
use notify_debouncer_full::notify::RecursiveMode;

use crate::config::Config;
use crate::confirm::SearchOptions;
use crate::index::{self, Index};
use crate::paths;
use crate::proto::{self, Request};
use crate::transport::{self, Stream};

/// How often a `watch` subscriber repaints when nothing changed (keeps the build-progress count and
/// the snapshot age fresh, and detects client disconnect).
const WATCH_HEARTBEAT: Duration = Duration::from_millis(250);

/// Upper bound on how often the idle reaper wakes to check; short timeouts check at their own pace.
const IDLE_CHECK_MAX: Duration = Duration::from_secs(15);

struct Shared {
    index: RwLock<Index>,
    ready: AtomicBool,
    root: PathBuf,
    snapshot: PathBuf,
    /// Cold-build progress (files processed / total to process); only meaningful while building.
    indexed: AtomicUsize,
    total: AtomicUsize,
    /// Posting-list footprint, cached so `status`/`watch` need not re-walk all postings each render.
    index_bytes: AtomicU64,
    /// A change counter + condvar so `watch` wakes immediately on any transition.
    seq: Mutex<u64>,
    seq_cv: Condvar,
    /// A cold build at least this long earns an on-disk snapshot; below it the index stays RAM-only.
    persist_threshold: Duration,
    /// Whether the resident index is backed by a snapshot. Set once the cold build's duration is
    /// known (warm starts keep the default `true`, since a snapshot already exists).
    persist: AtomicBool,
    /// Exit after this long with no client request, or `None` to stay resident.
    idle_timeout: Option<Duration>,
    /// Last time a request finished (or arrived); drives the idle reaper.
    last_active: Mutex<Instant>,
    /// Requests currently being served; any in-flight request (search, find, status, or a long-lived
    /// watch) keeps the idle reaper from exiting. Held via [`ActiveRequest`] so a panicking handler
    /// can't leak the count.
    in_flight: AtomicUsize,
}

/// Marks a request in flight for its whole lifetime. Drop decrements and stamps `last_active`, so the
/// reaper never exits mid-request and the idle clock resets when the request finishes — panic-safe,
/// since Drop runs even when a handler unwinds.
struct ActiveRequest<'a>(&'a Shared);

impl<'a> ActiveRequest<'a> {
    fn new(shared: &'a Shared) -> Self {
        shared.in_flight.fetch_add(1, Ordering::SeqCst);
        shared.touch();
        ActiveRequest(shared)
    }
}

impl Drop for ActiveRequest<'_> {
    fn drop(&mut self) {
        // Stamp before decrementing so the reaper, on seeing the count hit zero, reads a fresh time.
        self.0.touch();
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Shared {
    /// Read/write the index, recovering a poisoned lock rather than cascading panics across every
    /// handler: the index is a rebuildable over-approximation, so continuing (and letting the next
    /// reconcile repair it) is safer than wedging the daemon if one operation ever panics.
    fn read_index(&self) -> std::sync::RwLockReadGuard<'_, Index> {
        self.index.read().unwrap_or_else(|e| e.into_inner())
    }
    fn write_index(&self) -> std::sync::RwLockWriteGuard<'_, Index> {
        self.index.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Signal a state change (build done, reconcile applied) to wake watchers; refresh the cached
    /// posting footprint from the current index.
    fn bump(&self) {
        self.index_bytes
            .store(self.read_index().memory_bytes(), Ordering::Relaxed);
        *self.seq.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.seq_cv.notify_all();
    }

    /// Reset the idle clock to now.
    fn touch(&self) {
        *self.last_active.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// Mark the index ready to serve and start the idle clock from now, so a daemon that just
    /// finished a long cold build gets a full idle window before the reaper can exit it.
    fn mark_ready(&self) {
        self.touch();
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Persist the index to its snapshot unless this index is RAM-only (`persist` is false).
    fn maybe_save(&self, idx: &Index) {
        if self.persist.load(Ordering::SeqCst) {
            let _ = idx.save(&self.snapshot);
        }
    }

    /// Block until the change counter moves past `last` or the heartbeat elapses; return the latest.
    fn wait_change(&self, last: u64) -> u64 {
        let g = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        let (g, _) = self
            .seq_cv
            .wait_timeout_while(g, WATCH_HEARTBEAT, |s| *s == last)
            .unwrap_or_else(|e| e.into_inner());
        *g
    }
}

/// Run the daemon for `root` in the foreground. Returns once the socket can't be owned (another
/// daemon is already running) or on a fatal error.
pub fn run(root: PathBuf) -> Result<()> {
    let dir = paths::state_dir(&root);
    std::fs::create_dir_all(&dir)?;
    let listener = match transport::bind(&root)? {
        Some(l) => l,
        None => return Ok(()), // another daemon owns this root
    };

    let cfg = Config::get();
    let shared = Arc::new(Shared {
        index: RwLock::new(Index::default()),
        ready: AtomicBool::new(false),
        root: root.clone(),
        snapshot: paths::snapshot_path(&root),
        indexed: AtomicUsize::new(0),
        total: AtomicUsize::new(0),
        index_bytes: AtomicU64::new(0),
        seq: Mutex::new(0),
        seq_cv: Condvar::new(),
        persist_threshold: cfg.persist_threshold(),
        persist: AtomicBool::new(true),
        idle_timeout: cfg.idle_timeout(),
        last_active: Mutex::new(Instant::now()),
        in_flight: AtomicUsize::new(0),
    });

    spawn_indexer(shared.clone());
    spawn_watcher(shared.clone());
    spawn_idle_reaper(shared.clone());

    loop {
        let conn = match transport::accept(&listener) {
            Ok(conn) => conn,
            Err(_) => continue,
        };
        let shared = shared.clone();
        std::thread::spawn(move || {
            // Count the whole connection as in flight (covering the blocking read), so the reaper
            // can't exit out from under a request — including one accepted but not yet parsed.
            let _active = ActiveRequest::new(&shared);
            let _ = handle(conn, &shared);
        });
    }
}

/// Warm-start from the snapshot if present (serve immediately), then build/reconcile in the
/// background so the resident index reflects the real tree.
fn spawn_indexer(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        if let Ok(idx) = Index::load(&shared.snapshot) {
            *shared.write_index() = idx;
            shared.mark_ready();
            shared.bump();
            // catch changes made while the daemon was down
            let mut idx = shared.write_index();
            idx.reconcile(&shared.root);
            shared.maybe_save(&idx);
            drop(idx);
            shared.bump();
        } else {
            // Cold build: publish total, then index reporting per-file progress for watchers.
            let started = Instant::now();
            let paths = index::walk_files(&shared.root);
            shared.total.store(paths.len(), Ordering::Relaxed);
            shared.bump();
            let built = Index::from_paths(&paths, &shared.indexed);
            // A build cheap enough to redo on the next start stays RAM-only: skip the snapshot (and
            // its per-reconcile rewrites), trading a sub-threshold cold rebuild for the disk.
            shared.persist.store(
                started.elapsed() >= shared.persist_threshold,
                Ordering::SeqCst,
            );
            shared.maybe_save(&built);
            *shared.write_index() = built;
            shared.mark_ready();
            shared.bump();
        }
    });
}

/// Watch the tree; on a debounced change burst, reconcile the resident index and persist. The
/// reconcile re-walks ignore-aware, so freshly-created ignored files never leak into results.
fn spawn_watcher(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = match new_debouncer(Duration::from_millis(300), None, move |res| {
            let _ = tx.send(res);
        }) {
            Ok(d) => d,
            Err(_) => return,
        };
        if debouncer
            .watch(&shared.root, RecursiveMode::Recursive)
            .is_err()
        {
            return;
        }
        for res in rx {
            if res.is_err() || !shared.ready.load(Ordering::SeqCst) {
                continue;
            }
            let mut idx = shared.write_index();
            if idx.reconcile(&shared.root) > 0 {
                shared.maybe_save(&idx);
                drop(idx);
                shared.bump();
            }
        }
    });
}

/// Exit the daemon once it has gone `idle_timeout` without a request (freeing its RAM; the next
/// search respawns it). Never exits before the first build is ready or while a request is in flight.
/// No-op when disabled.
fn spawn_idle_reaper(shared: Arc<Shared>) {
    let Some(timeout) = shared.idle_timeout else {
        return;
    };
    let tick = timeout.min(IDLE_CHECK_MAX);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(tick);
            // Don't reap a daemon that is still doing its first build, or one mid-request.
            if !shared.ready.load(Ordering::SeqCst) || shared.in_flight.load(Ordering::SeqCst) > 0 {
                continue;
            }
            let idle = shared
                .last_active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .elapsed();
            if idle >= timeout {
                transport::cleanup(&shared.root);
                std::process::exit(0);
            }
        }
    });
}

fn handle(mut conn: Stream, shared: &Shared) -> Result<()> {
    let req = proto::read_request(&mut conn)?;
    match req {
        // Errors here are essentially "client went away mid-stream"; ignore so we still attempt the
        // stream terminator below — a request that produced N frames then errored should not look
        // different to the client than a clean finish.
        Request::Search { opts, pattern } => {
            let _ = content_search(shared, &pattern, opts, &mut conn);
        }
        Request::Find {
            needle,
            after,
            limit,
        } => {
            let out = find(shared, &needle, after.as_deref(), limit as usize);
            let _ = proto::write_data(&mut conn, &out);
        }
        Request::Status => {
            let _ = proto::write_data(&mut conn, &status(shared));
        }
        Request::Watch => return watch(shared, &mut conn),
        Request::Shutdown => {
            let _ = proto::write_data(&mut conn, b"ok\n");
            let _ = proto::end_stream(&mut conn);
            transport::cleanup(&shared.root);
            std::process::exit(0);
        }
    }
    let _ = proto::end_stream(&mut conn);
    Ok(())
}

/// Stream a fresh status frame on every change (and on a heartbeat), until the client disconnects.
/// The blocking wait holds no index lock, and rendering only touches the (cheap-while-building)
/// resident index, so an attached watcher does not slow indexing.
fn watch(shared: &Shared, conn: &mut Stream) -> Result<()> {
    // The connection's ActiveRequest guard (held in the accept loop) keeps the daemon alive for the
    // whole subscription and resets the idle clock when it ends, so nothing extra is needed here.
    let mut last = 0;
    loop {
        if proto::write_data(conn, &status(shared)).is_err() {
            return Ok(()); // client went away
        }
        last = shared.wait_change(last);
    }
}

/// Stream content-search results straight to the socket so huge result sets aren't buffered.
fn content_search(
    shared: &Shared,
    pattern: &str,
    opts: SearchOptions,
    conn: &mut Stream,
) -> Result<()> {
    if shared.ready.load(Ordering::SeqCst) {
        // Resolve candidates while holding the read lock, then RELEASE it before streaming: ripgrep
        // confirm + blocking socket writes must never run under the index lock, or a slow client
        // would block the watcher's write lock and freeze indexing.
        let paths = crate::candidate_paths(&shared.read_index(), pattern, opts);
        let effective = crate::effective_pattern(pattern, opts);
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        crate::confirm::search_streaming(&effective, &refs, &shared.root, opts, |chunk| {
            proto::write_data(&mut *conn, chunk)
        })
    } else {
        // Cold start only: pipelined full scan until the first build finishes. The sink is shared
        // across walk threads, so guard the socket with a mutex.
        let conn = std::sync::Mutex::new(conn);
        crate::stream_full_scan(&shared.root, pattern, opts, |chunk| {
            if let Ok(mut c) = conn.lock() {
                let _ = proto::write_data(&mut **c, chunk);
            }
        })
    }
}

/// File-name lookup, keyset-paginated. The response leads with a `proto::format_find_header` line so
/// the client reports the true total (not just the page) and can resume via `next_after`.
fn find(shared: &Shared, needle: &str, after: Option<&str>, limit: usize) -> Vec<u8> {
    let (lines, total, start): (Vec<String>, usize, usize) = if shared.ready.load(Ordering::SeqCst)
    {
        let idx = shared.read_index();
        let (hits, total, start) = idx.find(needle, after, limit);
        let lines = hits
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        (lines, total, start)
    } else {
        let mut all: Vec<String> = index::walk_files(&shared.root)
            .iter()
            .filter(|p| p.to_string_lossy().contains(needle))
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        all.sort_unstable();
        let total = all.len();
        let start = after.map_or(0, |a| all.partition_point(|p| p.as_str() <= a));
        let lines = all.into_iter().skip(start).take(limit).collect();
        (lines, total, start)
    };
    // Offer a resume key only when matches genuinely remain past this page (we know the true total
    // and the keyset offset), so following `next_after` never lands on an empty page.
    let next_after = (start + lines.len() < total)
        .then(|| lines.last().map(String::as_str))
        .flatten();
    let mut out = proto::format_find_header(total, start, lines.len(), next_after);
    for l in &lines {
        out.push_str(l);
        out.push('\n');
    }
    out.into_bytes()
}

fn status(shared: &Shared) -> Vec<u8> {
    let idx = shared.read_index();
    let state = if shared.ready.load(Ordering::SeqCst) {
        "ready".to_string()
    } else {
        let done = shared.indexed.load(Ordering::Relaxed) as u64;
        let total = shared.total.load(Ordering::Relaxed) as u64;
        if total > 0 {
            format!(
                "building {} / {} files",
                crate::status::human_count(done),
                crate::status::human_count(total)
            )
        } else {
            "building (scanning tree...)".to_string()
        }
    };
    // RAM-only once a cold build decided not to persist; while building, the decision isn't made yet.
    let ram_only = shared.ready.load(Ordering::SeqCst) && !shared.persist.load(Ordering::SeqCst);
    crate::status::Status {
        root: &shared.root,
        snapshot: &shared.snapshot,
        running: true,
        ram_only,
        state: Some(state),
        files: Some(idx.file_count()),
        trigrams: Some(idx.trigram_count()),
        memory_bytes: Some(shared.index_bytes.load(Ordering::Relaxed)),
    }
    .render()
    .into_bytes()
}
