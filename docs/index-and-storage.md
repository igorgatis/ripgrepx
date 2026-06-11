# Index & Storage Spec

Status: **draft / research-backed**. This document records what kind of index `rgx` needs, how it
gets built and kept fresh, which storage engine to use, and — most importantly — the **hypotheses
that still need verification** before they harden into design. Where a claim is backed by a
measurement, the number is given; where it is a guess, it is labelled a hypothesis (**H-n**).

It builds on [`design.md`](design.md) (the candidate-index-in-front-of-ripgrep model) and
[`indexing.md`](indexing.md) (the freshness contract). Read those first.

## 1. What the index is for

The index answers one question fast: **"which files could contain a match for this pattern?"** It
never decides what matches — ripgrep does, against the real bytes on disk. So the index is free to
over-approximate (return a few extra candidate files: a little slower) but must **never**
under-approximate (drop a file that does match: a lost result). Every decision below is subordinate
to that asymmetry.

This is the classic **trigram inverted index** of Russ Cox's *Regular Expression Matching with a
Trigram Index* and the lineage that followed it (Google Code Search, `zoekt`, `livegrep`). `rgx`
adopts that model and defers all matching to ripgrep.

## 2. The index: file-granularity trigram inverted index

### 2.1 Structure

- **Trigram → posting list of file IDs.** A trigram is any 3 consecutive bytes. For each file we
  record the *set* of distinct trigrams occurring anywhere in it, and invert that into
  `trigram -> {file IDs}`.
- **File granularity, not line/offset.** We store *which files* contain a trigram, not where.
  ripgrep recomputes exact line numbers when it confirms, so positions in the index would be
  redundant work and ~15× more disk (zoekt's positional index is ~3× corpus; Cox's file-granularity
  index is ~18%). **Decision: doc-ID only.** Measured index sizes (§7) confirm this stays small.
- **File metadata table.** `file ID -> {path, size, mtime, inode, type-tag}`. Drives freshness
  checks (§6), path/type scoping, and the find/fd-style name lookup.
- **Filename index.** A separate structure for fd/find-style name/path lookup (§5).

### 2.2 Trigram is the right n

Bigrams are too common (weak pruning); 4-grams explode the key space. Trigram is the empirical sweet
spot every prior code-search engine converged on. The byte trigram space is 2²⁴ ≈ 16.7M keys; a
source-heavy repo uses a small fraction of it (linux kernel: **643,953 distinct trigrams**, §7).

### 2.3 Regex → trigram query (the soundness core)

This is where correctness is earned. A regex is turned into a **boolean formula over trigrams — an
AND-of-ORs** — that every matching file is guaranteed to satisfy. The translation is driven by
`regex-syntax`'s literal extraction (`regex_syntax::hir::literal::{Extractor, Seq, Literal}`), whose
**exact / inexact** bit is the soundness oracle:

- A `Seq` of literals is an **OR** ("at least one must match"). An **exact** literal is a complete
  match path (safe to require); an **inexact** one was cut (the match extends beyond it).
- **The invariant:** the only sound way to add an AND-constraint is from a *proven-required* literal
  — exact, not under `*`/`?`, and at least n (=3) bytes long. Everything uncertain degrades to
  "match all files" (= fall back to a full ripgrep scan). A false negative loses a result; a false
  positive only costs a scan. So **when in doubt, scan.**

Construct-by-construct (mirrors Cox's algebra and ripgrep's own `grep-regex/src/literal.rs`
`InnerLiterals`):

| Regex construct | Trigram query contribution |
| --- | --- |
| literal `foo` (≥3) | AND of its sliding trigrams (`foo`→`foo`; `_SUSPEND`→`_su,sus,usp,...`) |
| concatenation `foo.*bar` | AND of each part's required trigrams (+ trigrams spanning a join of two exact literals) |
| alternation `a\|b\|c` | OR of every branch's query; if **any** branch lacks a required literal → whole query is "match all" |
| `a*`, `a?`, `{0,n}` | atom **not** required → no constraint |
| `a+`, `{1,}` | at least one `a` required |
| `(?i)foo` | let the HIR case-fold (it expands `k`→also U+212A); OR over folded trigram variants |
| `\bfoo\b`, `^`, `$` | zero-width: still require `foo`; ripgrep confirms boundaries |
| `.`, `\w+`, `[A-Z]+`, short patterns (<3) | no required trigram → **fall back to full scan** |

**Mandatory fallback.** When the formula reduces to "match all" (too few trigrams, large class,
case-fold blow-up, multiline straddle), `rgx` bypasses the index and hands the whole tree to
ripgrep. Falling back is always sound; it only forfeits the speedup.

### 2.4 Posting list representation

- **Roaring bitmaps** (`roaring` crate) for posting lists. They keep file IDs sorted (cheap
  AND/OR — exactly the query evaluation we need), compress dense and sparse lists well, and are
  mutable in place (good for incremental). The prototype used roaring throughout and query
  evaluation was **≤0.1 ms** (§7).
- Alternative considered: delta+varint for the rare-trigram long tail, roaring for hot/dense lists.
  Roaring-everywhere is simpler and "never a bad choice"; the split is a possible later optimization
  (**H-7**).

## 3. Building the index fast (cold, first use)

### 3.1 Pipeline

Reuse ripgrep's own crates so the indexed file set is **exactly** the set `rg` would search:

- **Walk:** `ignore::WalkBuilder::new(root).build_parallel()` with **default settings**, which equal
  `rg`'s defaults (hidden skipped, all git rules on, `.ignore` on, `parents` on, `require_git`).
  Don't diverge unless mirroring a specific `rg` flag.
- **Streaming + parallel:** walk-producer → bounded channel → rayon consumers (fsnav's pattern). Each
  worker reads a file, computes its distinct trigrams, and merges into sharded postings (the
  prototype used 256 mutex-guarded shards keyed by `trigram & 255`; contention was negligible).
- **Serve immediately:** bind/serve before the first index completes; until a region is indexed,
  queries **fall back to a plain ripgrep scan** (strictly better than fsnav's "try again later" —
  see §8). Seed readiness from a persisted index so warm starts are instant.

### 3.2 Measured cold-build cost (prototype, this machine: 12 cores, 24 GB)

| repo | files | searchable | walk | index | **total** |
| --- | --- | --- | --- | --- | --- |
| lucene | 7,426 | 129 MB | 41 ms | 1,466 ms | **1.5 s** (binary-inflated, see §3.3) |
| kubernetes | 30,165 | 252 MB | 158 ms | 1,311 ms | **1.5 s** |
| vscode | 15,116 | 209 MB | 95 ms | 1,100 ms | **1.2 s** |
| linux | 93,596 | 1,581 MB | 197 ms | 7,133 ms | **7.4 s** |

The walk is a tiny fraction; cost is read + trigram extraction. **7.4 s to fully index the entire
Linux kernel** while already serving fallback queries is acceptable for "fast on first use." This is
single-pass and unoptimized (e.g. no mmap reads, no SIMD trigramming) — headroom exists (**H-2**).

### 3.3 Binary/large files inflate the index — exclude them

lucene's index was **293 MB / 14.2M distinct trigrams** vs linux's 199 MB / 0.64M, because lucene's
`.zip`/`.dic`/`.aff` files are near-random bytes that explode the trigram space and slow the build.
ripgrep skips binary content at *search* time; `rgx` should skip it at *index* time.

**H-1 RESOLVED (empirically) — "skip every file containing a NUL" is UNSOUND.** Measured ripgrep
15.1.0 behavior, recursive (traversal) search:

- A NUL **inside the initial detection window** ⇒ the whole file is suppressed (no output). Example:
  `NEEDLE\0` with the NUL at offset 19 produced *nothing* in a tree search.
- A NUL **deep in an otherwise-text file** ⇒ ripgrep prints every match *before* the NUL, then quits
  with a **stderr** warning (`WARNING: stopped searching binary file after match`). Example: a 144 KB
  file with `EARLY_NEEDLE` on line 1 and a NUL at offset ~143 924 printed `:1:EARLY_NEEDLE` and
  suppressed only the post-NUL match.

So a file with a deep NUL still yields real, printed matches. Skipping it at index time would drop
them — unsound. The **safe rule**: skip indexing a file *only* when ripgrep would suppress it
entirely, i.e. a NUL falls within ripgrep's initial binary-detection window (the `.zip`/`.dic` case
that bloated lucene). Index everything else fully — indexing a deep-NUL file's trigrams (even
post-NUL ones) only over-includes candidates, which is harmless. Use `grep-searcher`'s own detection
to make the call so it matches ripgrep exactly; persist the size/binary policy so a config change
forces a rebuild.

**This also constrains the confirm step (see §10).** Passing candidates to the `rg` *binary* as
explicit args does **not** reproduce traversal semantics: explicitly-listed binary files print
`binary file matches` to **stdout** (and pre-NUL matches), whereas traversal prints pre-NUL matches
plus a **stderr** warning. To stay byte-for-byte identical, the confirm step must reproduce
*traversal* binary handling (`grep-searcher` `BinaryDetection::quit`), which favors an **in-process
confirm** over exec'ing `rg`.

## 4. Keeping it fresh fast (incremental)

### 4.1 Change detection & coalescing

From fsnav (two-stage) and codegraph:

- **Watch** with `notify` + `notify-debouncer-full` (settle window ~200 ms) for the millisecond
  burst of FS events one save fires.
- **Leading-edge throttle** on top: the first change after a quiet period is indexed *immediately*;
  further changes within a window (~500 ms) accumulate and flush once — so a single save lands at
  once, a branch switch / save-all coalesces into one update.
- Keep the watcher's recursive prescan **off** the serve path (it can be slow on big ignored trees).

### 4.2 Update cost (derived from measured throughput)

Per-file index cost: **43 µs (k8s) – 76 µs (linux)**, read included. Realistic change sets are small
(a typical commit touches 1–100 files). So:

| change | files | index work |
| --- | --- | --- |
| single save | 1 | <1 ms |
| feature commit | ~50 | ~3 ms |
| big branch switch | ~2,000 | ~150 ms |

The dominant cost is re-reading changed files; posting-list update is on top of this.

### 4.3 The append-unfriendly problem & deletes (H-3)

Inverted lists don't like in-place edits, and a changed/deleted file leaves **dead file IDs** in
trigram lists it no longer contains. fsnav tolerates stale postings because every candidate is
re-confirmed — and **so can `rgx`, because ripgrep is the confirm step**: a dead file ID just yields
an extra candidate that ripgrep scans and finds nothing in. So correctness survives stale postings;
only *precision* (and thus speed) degrades. The open question is how to keep precision high cheaply:

- **Option A — KV merge-operator** (fsnav/RocksDB-style): emit add/remove(file-ID) operands folded
  into posting lists; no read-modify-write. Removing a file's old trigrams requires knowing them
  (re-read old content or store per-file trigram sets).
- **Option B — Lucene-style immutable segments + tombstones + background merge**: changed files go
  into a small new segment in ms; a tombstone bitset masks superseded file IDs; periodic merge
  rebuilds and drops tombstones. Best fit for "few files changed, re-index fast" and the natural
  match for an mmap-first layout.
- **Option C — accept stale postings, periodically rebuild**: simplest; relies entirely on ripgrep
  to filter false positives; rebuild in the background when precision drifts.

**Leaning B**, but which one wins on the build-fast / update-fast / precision triangle is **H-3** —
must be benchmarked with simulated edits (git history / file mutation).

## 5. Filename / path index (find/fd-style)

`rgx --find` locates files/dirs by name or path. Strongest fit: an **FST** (`fst` crate) over the
sorted set of paths — mmap-able, tiny (paths share prefixes), and supports prefix, range, glob, and
Levenshtein-fuzzy lookup via automaton intersection in one structure. A trigram-on-paths index
(reusing §2 machinery) is an alternative for arbitrary substring path search. **H-6:** FST is enough
for v1; add path-trigrams only if substring path search demands it.

## 6. Freshness & correctness at query time

- **ripgrep is the confirm step.** Output is byte-for-byte `rg`'s; the index only chooses which
  files `rg` opens. Stale/imprecise index ⇒ extra candidates, never wrong output.
- **Query-time stat verification** (fsnav's best idea, adopted by codegraph): before trusting that a
  candidate set is complete, reconcile recently-changed files by `(size, mtime)` — correct
  regardless of whether the watcher saw the change, and sub-microsecond. New/edited files not yet in
  the index are routed to the fallback scan or re-indexed inline.
- **First-call gate** (codegraph): gate the very first query on a quick reconciliation; later calls
  don't block.
- **Staleness surfaced, not hidden** (codegraph banner / `indexing.md` freshness flag): when a
  result might miss a just-changed file, say so (MCP) so the agent re-reads — never silently serve
  stale.

## 7. Empirical results (prototype)

A throwaway prototype (`../rgx-proto`, sibling crate: `ignore` walk + rayon + roaring postings, file
granularity, literal/alternation trigram extraction) was run against all four repos. rg = ripgrep
15.1.0. **rgx latency = candidate computation + one ripgrep invocation over the candidate files,
with the index resident in RAM (the daemon model); both rg invocations capture output, so the
speedup ratios are apples-to-apples.** Index-load-from-disk is excluded (amortized by the daemon).

**Query speedup vs full-tree ripgrep** (median of 3, candidate compute ≤0.1 ms throughout):

| repo | query | kind | candidates / total | speedup |
| --- | --- | --- | --- | --- |
| linux | `\bstruct \w+_ops\b` | regex+literal | 1,838 / 93,596 | **64×** |
| linux | `spin_lock_irqsave` | literal | 3,752 / 93,596 | **40×** |
| linux | `EXPORT_SYMBOL_GPL` | literal | 3,658 / 93,596 | **37×** |
| linux | `devm_kzalloc` | literal | 5,690 / 93,596 | **30×** |
| linux | `task_struct\|spin_lock\|mutex_lock` | alternation | 12,925 / 93,596 | **12×** |
| kubernetes | `func \w+Reconcile` | regex+literal | 235 / 30,165 | **42×** |
| kubernetes | `PodSpec` | literal | 783 / 30,165 | **24×** |
| kubernetes | `DaemonSet\|StatefulSet\|ReplicaSet` | alternation | 861 / 30,165 | **23×** |
| kubernetes | `kubelet` | literal (common) | 2,532 / 30,165 | **11×** |
| vscode | `createDecorator` | literal | 595 / 15,116 | **14×** |
| vscode | `registerCommand` | literal | 791 / 15,116 | **13×** |
| vscode | `IDisposable\|registerCommand\|createDecorator` | alternation | 2,220 / 15,116 | **5.5×** |
| vscode | `\w+Service` | broad literal | 6,225 / 15,116 | **2.1×** |
| lucene | `MergePolicy` | literal | 320 / 7,426 | **9.3×** |
| lucene | `IndexWriter` | literal | 839 / 7,426 | **5.5×** |
| lucene | `TokenStream\|IndexWriter\|BytesRef` | alternation | 2,266 / 7,426 | **2.4×** |
| lucene | `\w+Exception` | broad literal | 4,454 / 7,426 | **1.4×** |
| kubernetes | `.*` | pure fallback | (scan all) | **0.95×** (no regression) |

**Findings:**

1. **Selective literal/regex-with-literal queries: 5–64× faster.** Bigger repo ⇒ bigger win (the
   linux full scan is 1.6–2.8 s; rgx answers in tens of ms).
2. **Broad literals still win** (`\w+Service` 2.1×, `\w+Exception` 1.4×) — never a regression, because
   ripgrep still scans fewer bytes.
3. **Pure fallback (`.*`) is a wash (0.95×)** — `rgx` correctly detects "no required trigram" and runs
   plain ripgrep; the only cost is a sub-ms index check.
4. **Soundness verified — MISSED = 0.** For every tested query, *every* file ripgrep matched was in
   the candidate set, with excellent precision:

   | query | rg match files | candidates | missed | precision |
   | --- | --- | --- | --- | --- |
   | linux `EXPORT_SYMBOL_GPL` | 3,611 | 3,658 | **0** | 99% |
   | linux `spin_lock_irqsave` | 3,463 | 3,752 | **0** | 92% |
   | kubernetes `PodSpec` | 764 | 783 | **0** | 98% |
   | vscode `createDecorator` | 505 | 595 | **0** | 85% |
   | lucene `MergePolicy` | 320 | 320 | **0** | 100% |

**Index size (in-RAM roaring, ≈ on-disk):** linux 199.6 MB (12.6% of corpus), kubernetes 79.8 MB,
vscode 129.8 MB, lucene 293 MB (binary-inflated — see §3.3). Source-heavy repos land ~13–30% of
corpus; the linux kernel's entire content index is ~200 MB.

> Caveat: prototype rg-tree times measured via captured output run higher than the `hyperfine`
> baselines (e.g. linux ~2.7 s vs 1.7 s warm) because `.output()` collects all matches; the ratios
> are still fair (both sides capture). The standalone `hyperfine` rg baselines are: lucene ~105 ms,
> kubernetes ~420 ms, vscode ~200 ms, linux ~1.67 s (literal, warm).

## 8. Storage engine

Workload, in priority order: (1) fast cold bulk-build (millions of postings); (2) fast low-latency
incremental updates; (3) fast random posting-list reads at query (mmap-friendly); (4) compact;
(5) durability **not** critical (the index is a rebuildable cache); (6) low dep weight, pure-Rust
preferred.

Ranked candidates (full survey in commit history of this doc / research notes):

1. **fjall** (pure-Rust LSM, `fjall-rs`) — **recommended starting point.** LSM shape matches
   bulk-write + low-latency incremental; keyspaces map to {postings, filenames, metadata};
   configurable non-durable persist fits a rebuildable cache; light dep, active, MIT/Apache.
   **H-4:** fjall's bulk-build and posting-read latency are within striking distance of RocksDB at
   kernel scale — must benchmark.
2. **Custom segmented mmap format over `fst`** (zoekt/livegrep-style) — highest performance ceiling
   and the natural home for Lucene-style segments+tombstones (§4.3) and an mmap-first query path
   (the prototype already shows roaring postings + RAM-resident index give ≤0.1 ms queries). Higher
   build/maintenance cost (merge logic, crash-tolerant overlay, format versioning). **H-5:** the
   custom format beats the best KV store by enough to justify its complexity — build the KV baseline
   first, then measure.
3. **RocksDB** (`rust-rocksdb`) — proven write-throughput leader with SST bulk-ingest; falls afoul
   of pure-Rust preference (heavy C++ dep, ~40 s compile). The write-throughput safety net if fjall
   underperforms.
4. **redb** (pure-Rust COW B-tree) — great reads + format stability, but COW B-tree is the wrong
   shape for random-key bulk write. Fallback if fjall's maturity worries.
5. **tantivy** — a ready-made inverted index (ngram tokenizer), but couples to heavyweight full-text
   machinery we'd fight; a spike, not a default.

Not recommended: sled (perpetual alpha / being rewritten), bare LMDB/MDBX (read-optimized,
single-writer hurts bulk write), persy/jammdb/canopydb (immature or transaction-focused).

**Storage-policy decisions to persist** (fsnav lessons): a `SCHEMA_VERSION` that **wipes & rebuilds**
rather than migrates (cheap for a rebuildable cache); persist the **parameters the index was built
with** (size cap, binary policy, include rules) so a config change that invalidates the fast path
forces a rebuild; WAL-less bulk writes for cold build, WAL-on for incremental edits.

## 9. Hypotheses to verify

> **Verified so far:** **H-8** (sound regex→trigram extraction) — a 104,000-check fuzz against the
> real `regex` engine found **zero** missed matches (`tests/soundness.rs`); precision 63.6% on
> adversarial random patterns, 85–100% on real identifier queries. **H-1** (binary handling) —
> resolved empirically (§3.3): skip a file only when ripgrep would suppress it entirely (NUL in the
> initial detection window); deep-NUL files must be indexed, and the confirm step must use
> traversal-mode binary handling.

- **H-1 (correctness): RESOLVED — see §3.3.** "Skip every file with a NUL" is unsound; skip only
  files ripgrep suppresses entirely (NUL in the initial detection window). Confirm step must
  reproduce *traversal* binary semantics.
- **H-8 (case-insensitive / sound extraction): VERIFIED** via `tests/soundness.rs`. *Precision
  caveat:* `regex-syntax` extraction case-folds the **whole literal** (cross-product), so a long
  `(?i)` literal explodes past the limit and falls back to scan. Better precision needs per-trigram
  case folding (fold each 3-byte window's variants, AND across positions) — a future enhancement,
  not a correctness issue.
- **H-2 (build speed):** cold build can beat the prototype's 7.4 s on linux (mmap reads, SIMD
  trigramming, better shard merge) — and stays "instant to serve" via fallback regardless.
- **H-3 (incremental):** segments+tombstones (Option B) beats merge-operator and rebuild-on-drift on
  the build-fast / update-fast / precision triangle, under simulated small and large (branch-switch)
  changes. *git history is usable on kubernetes; linux clone is shallow (depth 1) — simulate via file
  mutation.*
- **H-4 (storage):** fjall's bulk-build wall-clock and posting-read latency at kernel scale are
  competitive with RocksDB; measure index-on-disk size and p50/p99 incremental latency.
- **H-5 (custom format):** a custom mmap+fst+roaring segment format beats the best general KV store
  by enough to justify its maintenance cost.
- **H-6 (filenames):** an FST over paths covers `--find` for v1 without a separate path-trigram index.
- **H-7 (postings):** roaring-everywhere is good enough; a delta-varint long-tail + roaring-hot split
  is an optimization, not a requirement.
- **H-9 (confirm path): RESOLVED — everything in-process; no `rg` binary, ever.** Accelerated queries
  confirm in-process over candidate files via `grep-searcher` (`BinaryDetection::quit`) — verified
  byte-for-byte vs `rg`. Fallback queries (no trigram → every file a candidate) bypass the daemon and
  run a **pipelined parallel walk+search** (`confirm::full_scan`) streamed straight to stdout, exactly
  like `rg`'s own model. ripgrep's engine is linked in as a library, so rgx is fully self-contained —
  installing `rg` is **not** required. (An earlier exec-`rg` shortcut was removed; see §9b for the one
  workload where pure-library is slower and why we accept it.)

## 9a. Implementation status (built) {#impl}

The content-search path is fully implemented and CI-green: `trigram` → `query` (sound extraction) →
`index` (build/incremental/snapshot, binary-skip) → `confirm` (in-process ripgrep, streaming) → a
per-project **daemon** (`server`) holding the index resident, a **client** that spawns it on first
use, a `notify`-debounced **watcher** that reconciles on change, a **CLI** with the `--server` gate,
`--find`, and an `-i/-s/-w/-F/-U/-A/-B/-C` flag subset, and a stdio **MCP** server. State lives under
`~/.cache/rgx/<hash>` (never in the repo). Streaming responses keep memory bounded on both ends.

**Warm-daemon `rgx` vs `rg` (regression guard, `bench/bench.sh`, medians):**

| repo | accelerated speedup | modest fallback (e.g. `Po`) | `.*` (match every line) |
| --- | --- | --- | --- |
| lucene | 1.5–13× | ~1× | ~1× |
| vscode | 1.9–16× | ~1× | ~1× |
| kubernetes | 12–61× | 1.0–1.4× | 0.92× |
| linux | 14–53× | ~1× | 0.79× (see §9b) |

Every realistic query is multiples faster; modest fallback queries are at parity. The one residual is
`.*`-class "print every line" over the largest repo — a degenerate `cat`-the-repo query, not a search
(output is byte-identical to `rg`). This validates **H-2** in practice (cold build 1.2–7.4 s, served
immediately via fallback) and **H-3** (incremental reconcile on watch). Storage is still the in-RAM
index + custom snapshot (H-4/H-5 — fjall/segmented format — remain open optimizations).

## 9b. Fallback throughput: why `rg` is ~20% faster on `.*` (and why we accept it) {#throughput}

For fallback queries rgx runs a pipelined parallel walk+search using ripgrep's library crates. On
the extreme `rg .*` over the Linux kernel (≈tens of millions of matched lines, the whole 1.58 GB
corpus), rgx measures **0.79×**. We investigated ripgrep's binary (`crates/core`) to understand the
gap; the mechanism, for the record:

- **Lock-free render, single locked write per file.** `rg`'s parallel path (`search_parallel` in
  `crates/core/main.rs`) gives each worker a reused `termcolor::Buffer`; the `Standard` printer
  renders a whole file into that buffer with **no lock held**, then `BufferWriter::print` takes the
  stdout lock **once** and does a single `write_all` of the finished bytes. rgx already renders into a
  per-thread buffer lock-free, but then writes through a `Mutex<BufWriter<Stdout>>` — an extra
  userspace copy and buffering layer versus termcolor's direct one-`write_all`-per-file.
- **No mmap for tree walks.** `rg` only memory-maps when given ≤10 explicit *file* paths
  (`MmapChoice::auto()` in `hiargs.rs`); a recursive walk uses a 64 KB rolling `read` buffer
  (`line_buffer.rs`). So mmap is *not* the difference and matching it would not help.
- **Worker cap at 12.** `rg` caps threads at `min(available_parallelism, 12)`; an uncapped walker on
  a many-core box adds lock contention without throughput. rgx lets `ignore` default.
- **Reused searcher + printer per worker.** rgx already does this.

**Decision: accept the residual; do not chase it.** It only affects "match (almost) every line"
queries — i.e. printing the corpus, not searching it — and the output is identical to `rg`. The two
cheap levers (use `termcolor::BufferWriter` directly; cap workers at 12) would mostly close it but add
code for a degenerate case; the rest is essentially reimplementing `BufferWriter`. If we ever do
revisit it, the fix is "adopt `termcolor::BufferWriter` + per-worker `Buffer` and cap at 12 threads,"
not mmap. Realistic searches return a small fraction of lines, where rgx is already 12–61× faster.

## 10. Open questions

- ~~In-process vs. exec'ing `rg`~~ **— settled (§9, H-9): everything is in-process, no `rg` binary.**
  Exec'ing `rg file1 file2…` applies explicit-arg binary semantics (prints `binary file matches` on
  stdout) vs. traversal semantics (pre-NUL matches + stderr warning), so it isn't byte-identical;
  in-process `grep-searcher` with `BinaryDetection::quit` reproduces traversal exactly. The
  flag→config mapping remains binary-private in ripgrep's `crates/core/flags/hiargs.rs`, so the full
  rg flag surface still has to be vendored or reimplemented (currently a subset).
- How candidate-set size interacts with ripgrep's parallelism — at a few hundred files, is the win
  bounded by process startup rather than scan time? (lucene's ~100 ms absolute floor hints at this.)
- Index sharding for very large monorepos (zoekt's 4 GB shard cap via u32 offsets) — not needed at
  current scale but a known ceiling.

## 11. References

- Russ Cox, *Regular Expression Matching with a Trigram Index* — https://swtch.com/~rsc/regexp/regexp4.html
- zoekt design — https://github.com/sourcegraph/zoekt/blob/main/doc/design.md
- livegrep / suffix arrays — https://blog.nelhage.com/2015/02/regular-expression-search-with-suffix-arrays/
- `regex-syntax` literal extraction — https://docs.rs/regex-syntax/latest/regex_syntax/hir/literal/index.html
- ripgrep `grep-regex` inner literals — https://github.com/BurntSushi/ripgrep/blob/master/crates/regex/src/literal.rs
- `ignore`, `globset`, `grep-searcher` — https://docs.rs/ignore , https://docs.rs/globset , https://docs.rs/grep-searcher
- `fst` — https://docs.rs/fst ; `roaring` — https://docs.rs/roaring
- fjall — https://github.com/fjall-rs/fjall ; redb — https://github.com/cberner/redb
- fsnav (local sibling project) — daemon, freshness, debounce patterns
- codegraph — https://github.com/colbymchenry/codegraph — MCP staleness banner, first-call gate
