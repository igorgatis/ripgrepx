# Indexing

> For the concrete index data structure, storage, and benchmark results, see
> [`index-and-storage.md`](index-and-storage.md). This page describes the freshness contract; that one
> describes the mechanism.

The index exists for one purpose: to answer **"which files could contain a match for this
pattern?"** quickly, so ripgrep scans a small candidate set instead of the whole tree. It never
decides what matches — ripgrep does that against the real files — so the index is free to be
approximate in the direction of *more* candidates (safe: a little slower) but never *fewer* than the
truth (which would drop a real match).

## Really fast, even cold

Indexing a cold or large repo must never block searching:

- **Streaming + parallel.** The tree walk is pipelined straight into indexing, so reading and
  indexing overlap and run in parallel rather than as two sequential passes.
- **Serve immediately.** `rgx` answers queries the moment it's up; until a region is indexed, those
  queries simply fall back to a normal ripgrep scan, so results are correct from the first second.
- **Progress is visible.** A live count climbs while the first pass runs, instead of a silent wait.

## Always fresh

- **Incremental updates.** File changes are picked up as they happen. A single save lands almost
  immediately; a burst (branch switch, save-all) is coalesced into one quick update instead of
  reacting per file.
- **Ignore-aware, exactly like `rg`.** The walk yields the same files ripgrep would — see
  [What the walk includes](#what-the-walk-includes--ripgrep-parity). A config escape hatch can
  force-include paths that would otherwise be dropped (e.g. a generated directory you want searchable).
- **Verified at query time.** Because ripgrep reads the real files, a result is always confirmed
  against current disk contents; when the index and disk disagree, that's surfaced as a freshness
  flag rather than silently returning stale text.
- **Config reconciles.** Changing index config (size limits, include rules) can change which files
  are candidates; rgx reconciles rather than silently keeping the old rules.

## What the walk includes — ripgrep parity

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

Behavior is pinned to ripgrep's `ignore` 0.4.x; if that crate is bumped, re-verify against `rg`.

## Self-managing

The indexer starts on first use, updates itself as files change, and exits after an idle period
(default 1 h, `idle_timeout_secs`) to free its RAM — the next search respawns it. A small repo whose
cold build is cheap (under `persist_threshold_ms`, default 1 s) is kept **RAM-only**: no snapshot is
written, and each daemon start rebuilds from scratch. So `--server start`/`--server stop` are for
explicit control, not routine use. Config is read once at startup; restart the daemon to apply edits.
