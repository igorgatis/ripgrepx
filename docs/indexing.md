# Indexing

The index answers one question fast: **"which files could contain a match for this pattern?"** so a
search scans a small candidate set instead of the whole tree. It never decides what matches — ripgrep
does, against the real bytes on disk (see [`querying.md`](querying.md)) — so the index is free to
over-approximate (a few extra candidate files: a little slower) but **never** under-approximate (drop
a file that does match: a lost result). Every decision here is subordinate to that asymmetry.

This is the **trigram inverted index** of Russ Cox's *Regular Expression Matching with a Trigram
Index* and the lineage after it (Google Code Search, `zoekt`, `livegrep`); `rgx` defers all matching
to ripgrep.

## The index: file-granularity trigram inverted index

`src/index.rs`

### Structure

- **Trigram → posting list of file IDs.** A trigram is any 3 consecutive bytes. Each file is
  represented by the *set* of distinct trigrams it contains, inverted into `trigram -> {file IDs}`.
  Posting lists are **roaring bitmaps** (`roaring` crate): file IDs stay sorted (so AND/OR are cheap
  merges — exactly query evaluation), and they compress dense and sparse lists well.
- **File granularity, not line/offset.** We store *which files* contain a trigram, not where:
  ripgrep recomputes exact line numbers when it confirms, so positions would be redundant work and
  far more disk (a positional index runs ~3× corpus; this file-granularity index is ~13–30% of
  corpus — see [Storage](#storage)).
- **File table.** `file ID -> {path, size, mtime, live}`. Drives candidate resolution, freshness
  reconciliation, deletion (tombstone via `live = false`), and the `--find` name lookup.

### Why trigrams

Bigrams are too common (weak pruning); 4-grams explode the key space. Trigram is the sweet spot prior
code-search engines converged on. The byte trigram space is 2²⁴ ≈ 16.7M keys; a source-heavy repo
uses a small fraction (the linux kernel: ~550k distinct trigrams).

## Building it (cold, first use)

Indexing a cold or large repo must never block searching:

- **Streaming + parallel.** The tree walk is pipelined straight into indexing, so reading and indexing
  overlap rather than running as two sequential passes. Each rayon worker reads a file and dedups its
  trigrams with a reused 2²⁴-bit sparse bitset (bit-test, cleared via the distinct list — faster than
  hashing dense 24-bit keys), then merges into 256 mutex-sharded posting maps keyed by `trigram & 255`
  (workers rarely contend a lock). Shared maps keep peak build memory near the final index size (~one
  compressed copy, ≈200 MB for the Linux kernel) — a per-worker-map design would be faster but
  multiplies memory by the worker count, which isn't worth it.
- **Serve immediately.** The daemon binds and answers before the first build finishes; until a region
  is indexed, those queries fall back to a normal scan (correct from the first second, just not yet
  accelerated). A persisted snapshot makes warm starts instant (see [Storage](#storage)).
- **Progress is visible.** A per-worker counter feeds a live build display instead of a silent wait.

The walk reuses ripgrep's own crates so the indexed file set is **exactly** what `rg` would search:
`ignore::WalkBuilder::new(root).build_parallel()` with default settings, which equal `rg`'s defaults
(hidden skipped, all git rules on, `.ignore` on, `parents`, `require_git`) — see
[The ripgrep-parity walk](#the-ripgrep-parity-walk).

Measured cold build (12-core / 24 GB): lucene 7.4k files ~1.5 s, vscode 15.1k ~1.2 s, kubernetes
30.2k ~1.5 s, linux 93.6k ~7.4 s. The walk is a tiny fraction; cost is read + trigram extraction.

### Binary files

Binary content explodes the trigram space (near-random bytes), so files ripgrep would treat as binary
are skipped at index time. The rule is **sound against ripgrep's recursive-search behavior**, measured
directly:

- A NUL **within the initial detection window** ⇒ ripgrep suppresses the whole file in a tree search.
- A NUL **deep in an otherwise-text file** ⇒ ripgrep prints matches *before* the NUL, then stops.

So skipping a file outright is only safe when ripgrep would suppress it entirely. `rgx` skips a file
only if a NUL appears in its first 1 KB (`BINARY_SNIFF_BYTES`) — conservatively inside ripgrep's
larger detection window, so it only ever skips files ripgrep also suppresses. Everything else is
indexed in full; a deep-NUL file's post-NUL trigrams only over-include candidates, which is harmless
(the confirm step reproduces ripgrep's search-until-NUL behavior — see [`querying.md`](querying.md)).

## The ripgrep-parity walk

The candidate walk must yield **exactly** the files `rg` would for the same invocation: confirm
searches that file list directly without re-applying ignore rules, so an extra file becomes a phantom
match and a missing one drops a real match. rgx walks with ripgrep's own `ignore` crate at its
defaults — which already match `rg` (skip hidden files; honor `.gitignore`, `.ignore`,
`.git/info/exclude` and the global gitignore; read parent ignore files; don't follow symlinks) — plus
the one thing the `rg` binary adds on top, the `.rgignore` custom ignore name. This lives in one place
(`index::walk_builder`) so the index walk and the fallback scan can't drift from `rg` or each other.

Two ripgrep rules are worth stating, because they shape any future "share one index across
subdirectories" work (see [`design.md`](design.md) open questions):

- **`.gitignore` is inert without a `.git` (or `.jj`).** In a plain directory a `.gitignore` does
  nothing; the git directory both *activates* the gitignore stack and is the boundary the upward walk
  for parent ignores stops at. A repo's index is therefore naturally rooted at its git root.
- **An explicitly named path is exempt from ignore/hidden rules.** `rg PATTERN build/` searches a
  gitignored `build/`, and `rg` run from inside `node_modules/` searches it — ripgrep applies ignore
  rules only to entries it discovers by *descending*, never to the roots you hand it.

Behavior is pinned to ripgrep's `ignore` 0.4.x; if that crate is bumped, re-verify against `rg`. The
walk is held to that parity by a **differential fuzz suite** that compares rgx's file list against the
real `rg` binary over generated trees (baked fixtures plus randomized ignore layouts) — see
[`CONTRIBUTING.md`](../CONTRIBUTING.md#testing).

## Keeping it fresh (incremental)

`src/index.rs` + `src/server.rs`

- **Watch** with `notify` + `notify-debouncer-full` (300 ms settle window) coalesces a burst of FS
  events (a save-all, a branch switch) into one update.
- On a debounced batch the daemon calls `Index::reconcile`: an ignore-aware re-walk diffs the tree
  against the file table by `(size, mtime)`, re-indexing new/changed files and tombstoning vanished
  ones. (Reconciling by re-walk keeps freshly-created *ignored* files from leaking in; driving updates
  from raw event paths is a possible future optimization for very large trees.)
- A changed file's **current** trigrams are added so no new-trigram query can miss it. Trigrams the
  file no longer contains linger in their posting lists — harmless, because ripgrep is the confirm
  step: a stale posting only yields an extra candidate that ripgrep scans and finds nothing in. A
  periodic/full rebuild reclaims that precision.
- `stat` is taken **before** the read, so a write racing the read can't store a newer mtime over older
  trigrams and be skipped by the next reconcile.

Per-file index cost is ~40–80 µs (read included), so a typical commit (1–100 files) reconciles in
single-digit ms and a ~2,000-file branch switch in ~150 ms.

### `--find` (file/dir lookup)

`rgx --find` is a substring match over the live paths in the file table, returned sorted. (An FST over
paths would add prefix/glob/fuzzy lookup; not needed for the current substring use.)

## Storage

The resident index is the in-RAM structure above (`FxHashMap<trigram, RoaringBitmap>` + file table).
It is persisted to a **single versioned snapshot file** so the daemon warm-starts instantly instead of
re-walking:

- Location: `$RGX_CACHE_DIR/<hash>/index.bin` if set, else the config file's `cache_dir/<hash>/...`,
  else `$XDG_CACHE_HOME/rgx/<hash>/index.bin`, else `~/.cache/rgx/<hash>/index.bin`, keyed by the
  canonicalized root — **never written into the indexed repo**. `$RGX_CACHE_DIR` and `cache_dir`
  relocate only rgx's state (unlike the shared `$XDG_CACHE_HOME`).
- Format: a magic + version header (`RGXIDX01`), the file table, then per-trigram roaring postings.
  Saved atomically (temp file + rename). On load, postings whose file IDs fall outside the table are
  rejected (corrupt/foreign snapshot ⇒ rebuild rather than risk a bad candidate).
- The snapshot is a rebuildable cache: a version mismatch wipes and rebuilds rather than migrating.
- **RAM-only below a build-time threshold.** A cold build is timed; if it finished faster than
  `persist_threshold_ms` (config, default 1 s) the snapshot is skipped entirely and the index lives
  only in RAM, rebuilt on each daemon start. The cutoff is a *build-time* one, not a size one,
  because build cost tracks bytes scanned, not file count (Lucene's 7.4k large files take longer than
  VS Code's 15.1k small ones); at the default it lands around VS Code/Kubernetes scale (~30-40 MB
  index, ~150 MB corpus), so typical project repos stay RAM-only. This also avoids the per-reconcile
  snapshot rewrite for small, actively-edited trees. `persist_threshold_ms = 0` always persists.
  Once a snapshot exists, a warm start keeps persisting; delete the cache to re-evaluate.

Index size (roaring postings, ≈ on-disk snapshot, source-heavy repos): kubernetes ~47 MB, vscode
~42 MB, linux ~198 MB — roughly 13–30% of corpus. A general embedded KV store (e.g. `fjall`) or a
`zoekt`-style segmented mmap format would matter for a much larger corpus or multi-repo scale; at
these sizes the in-RAM index plus snapshot is simpler and queries evaluate in well under a millisecond.

## Self-managing

The indexer starts on first use, updates itself as files change, and exits after an idle period
(default 1 h, `idle_timeout_secs`) to free its RAM — the next search respawns it. A small repo whose
cold build is cheap (under `persist_threshold_ms`, default 1 s) is kept **RAM-only**: no snapshot is
written, and each daemon start rebuilds from scratch. So `--server start`/`--server stop` are for
explicit control, not routine use. Config is read once at startup; restart the daemon to apply edits.

## References

- Russ Cox, *Regular Expression Matching with a Trigram Index* — https://swtch.com/~rsc/regexp/regexp4.html
- zoekt design — https://github.com/sourcegraph/zoekt/blob/main/doc/design.md
- `ignore`, `grep-searcher` — https://docs.rs/ignore , https://docs.rs/grep-searcher
- `roaring` — https://docs.rs/roaring ; `fst` — https://docs.rs/fst
