# Index & Storage

How `rgx`'s candidate index is structured, built, kept fresh, and stored. It builds on
[`design.md`](design.md) (the candidate-index-in-front-of-ripgrep model) and
[`indexing.md`](indexing.md) (the freshness contract).

## 1. What the index is for

The index answers one question fast: **"which files could contain a match for this pattern?"** It
never decides what matches — ripgrep does, against the real bytes on disk. So the index is free to
over-approximate (return a few extra candidate files: a little slower) but **never**
under-approximates (drop a file that does match: a lost result). Every decision here is subordinate
to that asymmetry.

This is the **trigram inverted index** of Russ Cox's *Regular Expression Matching with a Trigram
Index* and the lineage after it (Google Code Search, `zoekt`, `livegrep`); `rgx` defers all matching
to ripgrep.

## 2. The index: file-granularity trigram inverted index

### 2.1 Structure (`src/index.rs`)

- **Trigram → posting list of file IDs.** A trigram is any 3 consecutive bytes. Each file is
  represented by the *set* of distinct trigrams it contains, inverted into `trigram -> {file IDs}`.
  Posting lists are **roaring bitmaps** (`roaring` crate): file IDs stay sorted (so AND/OR are cheap
  merges — exactly query evaluation), and they compress dense and sparse lists well.
- **File granularity, not line/offset.** We store *which files* contain a trigram, not where:
  ripgrep recomputes exact line numbers when it confirms, so positions would be redundant work and
  far more disk (a positional index runs ~3× corpus; this file-granularity index is ~13–30% of
  corpus — §6).
- **File table.** `file ID -> {path, size, mtime, live}`. Drives candidate resolution, freshness
  reconciliation, deletion (tombstone via `live = false`), and the `--find` name lookup.

### 2.2 Why trigrams

Bigrams are too common (weak pruning); 4-grams explode the key space. Trigram is the sweet spot prior
code-search engines converged on. The byte trigram space is 2²⁴ ≈ 16.7M keys; a source-heavy repo
uses a small fraction (the linux kernel: ~550k distinct trigrams).

### 2.3 Regex → trigram query (the soundness core, `src/query.rs`)

A regex is turned into a **boolean formula over trigrams — an AND-of-ORs** — that every matching file
is guaranteed to satisfy. The translation uses `regex-syntax`'s literal extraction
(`regex_syntax::hir::literal::{Extractor, Seq}`) in both **prefix** and **suffix** directions; each is
a complete over-approximation (every match begins, resp. ends, with one of the returned literals), so
their conjunction is a sound necessary condition.

The invariant: **the only sound way to add an AND-constraint is from a required literal** (length ≥ 3,
not under `*`/`?`). Anything uncertain degrades to "match all files" (a full scan). A false negative
loses a result; a false positive only costs a scan — so **when in doubt, scan.**

| Regex construct | Trigram query contribution |
| --- | --- |
| literal `foo` (≥3) | AND of its sliding trigrams (`_SUSPEND` → `_su,sus,usp,…`) |
| concatenation `foo.*bar` | AND of each part's required trigrams |
| alternation `a\|b\|c` | OR of every branch; if **any** branch lacks a required literal → match all |
| `a*`, `a?`, `{0,n}` | atom not required → no constraint |
| `a+`, `{1,}` | at least one `a` required |
| `(?i)foo` | the HIR case-folds (incl. exotic folds like `k`→U+212A); OR over folded variants |
| `\bfoo\b`, `^`, `$` | zero-width: still require `foo`; ripgrep confirms the boundary |
| `.`, `\w+`, `[A-Z]+`, patterns < 3 chars | no required trigram → **fall back to full scan** |

A fallback query (no usable trigram) simply makes *every* live file a candidate; there is no separate
code path — ripgrep confirms over whatever `candidates()` returns. The CLI shortcuts a fallback query
straight to an in-process pipelined scan (§5) rather than the daemon, since the index can't narrow it.

**Soundness is verified.** `tests/soundness.rs` fuzzes ~thousands of random patterns (plus `(?i)`,
anchors, alternation, classes) against the real `regex` engine over 100k+ (pattern, text) checks:
**zero missed matches**. `tests/index_soundness.rs` confirms the same at the candidate level over real
files — every file ripgrep matches is in the candidate set. Precision is high in practice (often
85–100% on identifiers); the only precision gap is long `(?i)` literals, where `regex-syntax`
case-folds the whole literal and may give up (correctly falling back to scan).

## 3. Building the index (cold, first use)

Reuse ripgrep's own crates so the indexed file set is **exactly** what `rg` would search:

- **Walk:** `ignore::WalkBuilder::new(root).build_parallel()` with default settings, which equal
  `rg`'s defaults (hidden skipped, all git rules on, `.ignore` on, `parents`, `require_git`).
- **Parallel index:** each rayon worker reads a file and dedups its trigrams with a reused
  2²⁴-bit sparse bitset (bit-test, cleared via the distinct list — faster than hashing dense 24-bit
  keys), accumulates into its **own** posting map (no shared locks on the hot path), and the
  per-worker maps are unioned once at the end. A per-worker progress counter feeds the live build
  display. Tradeoff: the per-worker maps raise *transient* peak memory during a cold build (≈2.4 GB
  for the entire Linux kernel) above the ~200 MB resident index; it's rebuildable and bounded by
  worker count, so capping build threads would cap it on very-many-core machines.
- **Serve immediately:** the daemon binds and answers before the first build finishes; until the
  index is ready, queries run a full in-process scan (correct, just not yet accelerated). A persisted
  snapshot makes warm starts instant (§7).

Measured cold build (12-core / 24 GB): lucene 7.4k files ~1.5 s, vscode 15.1k ~1.2 s, kubernetes
30.2k ~1.5 s, linux 93.6k ~7.4 s. The walk is a tiny fraction; cost is read + trigram extraction.

### 3.1 Binary files

Binary content explodes the trigram space (near-random bytes), so files ripgrep would treat as binary
are skipped at index time. The rule is **sound against ripgrep's recursive-search behavior**, measured
directly:

- A NUL **within the initial detection window** ⇒ ripgrep suppresses the whole file in a tree search.
- A NUL **deep in an otherwise-text file** ⇒ ripgrep prints matches *before* the NUL, then stops.

So skipping a file outright is only safe when ripgrep would suppress it entirely. `rgx` skips a file
only if a NUL appears in its first 1 KB (`BINARY_SNIFF_BYTES`) — conservatively inside ripgrep's
larger detection window, so it only ever skips files ripgrep also suppresses. Everything else is
indexed in full; a deep-NUL file's post-NUL trigrams only over-include candidates, which is harmless
(the confirm step reproduces ripgrep's search-until-NUL behavior — §4).

## 4. The confirm step (`src/confirm.rs`)

Matching is ripgrep's, in-process via the `grep` library crates — **no `rg` binary is invoked or
required.** A `grep::regex::RegexMatcher` + `grep::searcher::Searcher` + `grep::printer::Standard`
produce byte-for-byte `rg` output. The searcher uses `BinaryDetection::quit`, which reproduces
ripgrep's *recursive-traversal* binary semantics (passing files as explicit args to the `rg` binary
would not — it prints `binary file matches` to stdout instead of suppressing). Results stream in
batches so a large result set is never fully buffered.

Two entry points share builders and the binary-detection logic:

- **Accelerated:** confirm over the candidate file set.
- **Fallback / cold start:** a pipelined parallel `ignore` walk feeds per-file search, streaming as
  files are discovered — ripgrep's own model, one process, no socket hop.

## 5. Keeping it fresh (incremental, `src/index.rs` + `src/server.rs`)

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

## 6. Correctness & freshness at query time

- **ripgrep is the confirm step.** Output is byte-for-byte `rg`'s; the index only chooses which files
  ripgrep opens. A stale or imperfect index yields extra candidates, never wrong output or an invented
  match.
- **Confirmed against disk.** Because ripgrep reads the real file, a returned line reflects current
  content; a just-edited file is picked up by the watcher within a moment, and a daemon restart
  reconciles changes made while it was down.
- **Freshness boundary.** Candidate selection trusts the index, so a content change that preserves
  both byte size and mtime (e.g. a mtime-preserving copy) can be missed until the next size/mtime
  change — the standard tradeoff of mtime/size incremental indexing.

Index size (roaring postings, ≈ on-disk snapshot, source-heavy repos): kubernetes ~47 MB, vscode
~42 MB, linux ~198 MB — roughly 13–30% of corpus.

## 7. Storage

The resident index is the in-RAM structure above (`FxHashMap<trigram, RoaringBitmap>` + file table).
It is persisted to a **single versioned snapshot file** so the daemon warm-starts instantly instead of
re-walking:

- Location: `$XDG_CACHE_HOME/rgx/<hash>/index.bin` (else `~/.cache/rgx/...`), keyed by the
  canonicalized root — **never written into the indexed repo**.
- Format: a magic + version header (`RGXIDX01`), the file table, then per-trigram roaring postings.
  Saved atomically (temp file + rename). On load, postings whose file IDs fall outside the table are
  rejected (corrupt/foreign snapshot ⇒ rebuild rather than risk a bad candidate).
- The snapshot is a rebuildable cache: a version mismatch wipes and rebuilds rather than migrating.

A general embedded KV store (e.g. `fjall`) or a `zoekt`-style segmented mmap format would matter for a
much larger corpus or multi-repo scale; at the sizes above the in-RAM index plus snapshot is simpler
and queries evaluate in well under a millisecond.

## 8. Benchmarks

`rgx` (warm daemon) vs ripgrep 15.1.0 — see [`README.md`](../README.md#benchmarks) for the table and
methodology, and [`bench/bench.sh`](../bench/bench.sh) (the regression guard) to reproduce. Summary:
selective queries are **12–27× faster** (more on larger repos), alternations ~**23×**, and rgx's
latency is far more *consistent* than a full `rg` scan (tight σ). Fallback queries that the index
can't narrow land at parity with `rg`.

### Fallback throughput: the one residual

A *match-everything* query like `.*` over the largest repo (printing the whole 1.5 GB corpus) runs at
~**0.8×** of `rg`. This is the only case slower than `rg`, and it is a degenerate "cat the repo", not a
search — output is byte-identical. The gap is ripgrep's output pipeline: `rg`'s parallel path renders
each file into a reused per-worker `termcolor::Buffer` lock-free and does one locked `write_all`,
whereas rgx streams through a `Mutex<BufWriter<Stdout>>` with an extra copy. (mmap is *not* the
difference: `rg` uses buffered reads for tree walks, not mmap.) Closing it would mean adopting
`termcolor::BufferWriter` + per-worker buffers and capping workers at 12 — not worth the code for a
degenerate query, so we accept it. Every realistic search is much faster.

## 9. References

- Russ Cox, *Regular Expression Matching with a Trigram Index* — https://swtch.com/~rsc/regexp/regexp4.html
- zoekt design — https://github.com/sourcegraph/zoekt/blob/main/doc/design.md
- livegrep / suffix arrays — https://blog.nelhage.com/2015/02/regular-expression-search-with-suffix-arrays/
- `regex-syntax` literal extraction — https://docs.rs/regex-syntax/latest/regex_syntax/hir/literal/index.html
- ripgrep `grep-regex` inner literals — https://github.com/BurntSushi/ripgrep/blob/master/crates/regex/src/literal.rs
- `ignore`, `grep-searcher` — https://docs.rs/ignore , https://docs.rs/grep-searcher
- `roaring` — https://docs.rs/roaring ; `fst` — https://docs.rs/fst
