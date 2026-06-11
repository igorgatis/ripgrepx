# Indexing

> For the concrete index data structure, storage-engine choice, benchmark results, and the open
> hypotheses behind them, see [`index-and-storage.md`](index-and-storage.md). This page describes the
> freshness contract; that one describes the mechanism.

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
- **gitignore-aware.** The walk honors `.gitignore`, with a config escape hatch to force-include
  paths that would otherwise be dropped (e.g. a generated directory you want searchable).
- **Verified at query time.** Because ripgrep reads the real files, a result is always confirmed
  against current disk contents; when the index and disk disagree, that's surfaced as a freshness
  flag rather than silently returning stale text.
- **Config reconciles.** Changing index config (size limits, include rules) can change which files
  are candidates; rgx reconciles rather than silently keeping the old rules.

## Self-managing

The indexer starts on first use, updates itself as files change, reloads config without a restart,
and cleans up after itself when idle — so `--server start`/`--server stop` are for explicit control,
not routine use.
