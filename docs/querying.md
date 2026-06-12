# Querying

How a search runs: `rgx` turns the pattern into a trigram query, the index returns a small candidate
set, and ripgrep confirms the real matches over just those files. Matching is **ripgrep's** — the
query layer only decides *which files ripgrep opens*, never *what comes back*. This builds on
[`design.md`](design.md) (the model) and [`indexing.md`](indexing.md) (the index it queries).

## Regex → trigram query (the soundness core)

`src/query.rs`

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

### Soundness is verified

`tests/soundness.rs` fuzzes ~thousands of random patterns (plus `(?i)`, anchors, alternation,
classes) against the real `regex` engine over 100k+ (pattern, text) checks: **zero missed matches**.
`tests/index_soundness.rs` confirms the same at the candidate level over real files — every file
ripgrep matches is in the candidate set. Precision is high in practice (often 85–100% on
identifiers); the only precision gap is long `(?i)` literals, where `regex-syntax` case-folds the
whole literal and may give up (correctly falling back to scan). The harness lives in
[`CONTRIBUTING.md`](../CONTRIBUTING.md#testing).

## Candidate resolution

The trigram query evaluates directly over the inverted index: each required trigram's posting list is
a roaring bitmap of file IDs, so the AND-of-ORs is a series of cheap sorted-bitmap merges, and the
result is the set of candidate file IDs (resolved to live paths via the file table). See
[`indexing.md`](indexing.md) for the index structure.

A fallback query (no usable trigram) simply makes *every* live file a candidate; there is no separate
code path — ripgrep confirms over whatever `candidates()` returns. The CLI shortcuts a fallback query
straight to an in-process pipelined scan (below) rather than the daemon, since the index can't narrow
it.

## The confirm step

`src/confirm.rs`

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

## Correctness & freshness at query time

- **ripgrep is the confirm step.** Output is byte-for-byte `rg`'s; the index only chooses which files
  ripgrep opens. A stale or imperfect index yields extra candidates, never wrong output or an invented
  match.
- **Confirmed against disk.** Because ripgrep reads the real file, a returned line reflects current
  content; a just-edited file is picked up by the watcher within a moment, and a daemon restart
  reconciles changes made while it was down.
- **Freshness boundary.** Candidate selection trusts the index, so a content change that preserves
  both byte size and mtime (e.g. a mtime-preserving copy) can be missed until the next size/mtime
  change — the standard tradeoff of mtime/size incremental indexing. How freshness is maintained:
  [`indexing.md`](indexing.md#keeping-it-fresh-incremental).

## Output and paging

A bare `rgx <pattern>` streams ripgrep's `path:line:text` verbatim. The opt-in `--compact` view (and
the MCP paging mode) reshapes output for agents: grouped by file, paged, long lines trimmed around the
match — the **only** surface that isn't byte-for-byte `rg`, and even there the match set is exactly
ripgrep's (pagination is the sole volume control; every match is reachable).

Paging uses an **opaque keyset cursor**: it records the entire query (pattern + every flag) plus a
resume position, so the next page can't drift to a different search, and a result set that changed
between pages is flagged with a `note:` line. The daemon parks the cursor for ~2 minutes and hands
back a short id in its place (single-use); an expired one returns `pagination expired — re-run the
search`. The user- and agent-facing surfaces are in [`cli.md`](cli.md) and [`mcp.md`](mcp.md).

## Ordering (`--sort` / `--sortr`)

`src/sort.rs`

`--sort=KEY` (ascending) and `--sortr=KEY` (descending) *reorder* results without changing the match
set — ripgrep's own flags and vocabulary, with one rgx extension:

| Key | Order value | Where it comes from |
| --- | --- | --- |
| `path` | the path | `Path::cmp` (rg's lexical file order) |
| `modified` / `accessed` / `created` | file time (ns) | `stat`, like `rg --sort` |
| `weight` | `-(score · 1e6)` | weighted match (`--weights`), best-first |
| `none` (default) | — | don't reorder |

All keys are **file-level**: a per-file order value (a single `i64`) decides the order files appear in;
lines within a file keep ripgrep's order, and `--sortr` reverses only the file order (matching rg).
The order is a deterministic total order — `(order_value, path, lineno)` — so the keyset cursor stays
stable across pages. Ordering works on the **bare** output (buffered like `rg --sort`, which also drops
to single-threaded to sort) and in the `--compact`/MCP view; absence of `--sort` keeps the streaming,
historical-order path untouched. rgx orders files exactly as `rg --sort` does; the line format is
rgx's usual one (always-on line numbers, root-relative paths).

### Weighted match (`--sort=weight --weights=…`)

`src/rank.rs`

The `weight` key is a **model-supplied** relevance signal. Declare named weights and tag regex
alternation branches with `<label>`:

```sh
rgx --sort=weight --weights=impl:0.7,call:0.3 'fn (process<impl>|process\(<call>)'
```

The `<label>` tags are **stripped** to form the plain pattern ripgrep actually searches
(`fn (process|process\()`), so the match set is exactly `rg`'s. In parallel, rgx builds a
capture-instrumented copy of the regex — each labeled branch wrapped in a named capture group — and,
for each matching line, reads which branch participated to get its weight. A file's score is the
**max** weight over its matched lines (per-file aggregation), and a match attributable to no labeled
branch scores 0 and **sinks last** — still present and reachable, just deprioritized. `--sort=weight`
puts highest-weight files first (`--sortr=weight` flips it); `-F` has no branches to weight and is
rejected.

This is reorder-only *by construction*: scoring runs in the presentation layer over already-confirmed
matches (which branch matched is the regex engine's call, never ours). If the instrumented regex ever
misbehaved, the worst case is a worse ordering — never a wrong or missing match. Weighted match can't
be backtested against transcripts; it's a forward bet that letting the model express intent improves
page 1.

## Benchmarks

`rgx` (warm daemon) vs ripgrep 15.1.0 — see [`README.md`](../README.md#benchmarks) for the table and
methodology, and [`benches/bench.sh`](../benches/bench.sh) (the regression guard) to reproduce.
Summary: selective queries are **12–27× faster** (more on larger repos), alternations ~**23×**, and
rgx's latency is far more *consistent* than a full `rg` scan (tight σ). Fallback queries that the
index can't narrow land at parity with `rg`.

### Fallback throughput: the one residual

A *match-everything* query like `.*` over the largest repo (printing the whole 1.5 GB corpus) runs at
~**0.8×** of `rg`. This is the only case slower than `rg`, and it is a degenerate "cat the repo", not
a search — output is byte-identical. The gap is ripgrep's output pipeline: `rg`'s parallel path
renders each file into a reused per-worker `termcolor::Buffer` lock-free and does one locked
`write_all`, whereas rgx streams through a `Mutex<BufWriter<Stdout>>` with an extra copy. (mmap is
*not* the difference: `rg` uses buffered reads for tree walks, not mmap.) Closing it would mean
adopting `termcolor::BufferWriter` + per-worker buffers and capping workers at 12 — not worth the code
for a degenerate query, so we accept it. Every realistic search is much faster.

## References

- Russ Cox, *Regular Expression Matching with a Trigram Index* — https://swtch.com/~rsc/regexp/regexp4.html
- zoekt design — https://github.com/sourcegraph/zoekt/blob/main/doc/design.md
- livegrep / suffix arrays — https://blog.nelhage.com/2015/02/regular-expression-search-with-suffix-arrays/
- `regex-syntax` literal extraction — https://docs.rs/regex-syntax/latest/regex_syntax/hir/literal/index.html
- ripgrep `grep-regex` inner literals — https://github.com/BurntSushi/ripgrep/blob/master/crates/regex/src/literal.rs
- `grep-searcher` — https://docs.rs/grep-searcher
