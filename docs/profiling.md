# Profiling

rgx splits into a few distinct hot paths; profile each in isolation rather than the binary as a
whole. Cold index build is the only step a user *notices* (≈7 s for the Linux kernel); query latency
is already tens of ms, dominated by ripgrep's confirm.

| path | code | how to exercise it |
| --- | --- | --- |
| cold build | `Index::build` / `index_files` | `cargo bench build_2000_files`; samply the daemon during a cold build |
| candidate selection | `Index::candidates` / `eval` | `cargo bench candidates` |
| confirm | `confirm::search_streaming` | `cargo bench confirm` |
| trigram extraction | `trigram::for_each` | `cargo bench trigram_distinct_one_file` |
| incremental reconcile | `Index::reconcile` | edit files under a watched root; samply the daemon |

## Microbenchmarks (criterion)

```sh
cargo bench                      # all benches; HTML report under target/criterion/
cargo bench -- build_2000_files  # one bench (substring filter)
```

The benches in [`benches/index_bench.rs`](../benches/index_bench.rs) run against a deterministic
synthetic corpus in `$TMPDIR/rgx_bench_corpus_*` (generated once; delete to regenerate). They call the
library directly — no daemon, no socket — so deltas are clean. Use them to validate any optimization
before/after.

## Sampling profiler (samply)

`samply` is the easiest cross-platform CPU profiler (Firefox-Profiler UI), and works on macOS without
sudo.

```sh
cargo install samply                      # once
cargo build --profile profiling           # optimized + line tables (see Cargo.toml)

# Cold build: run the daemon in the foreground over a repo and let it build, then Ctrl-C.
samply record target/profiling/rgx --server        # cd into the repo first

# A bench under the profiler:
cargo bench --no-run                                # build the bench binary
samply record target/release/deps/index_bench-<hash> --bench build_2000_files
```

The `[profile.profiling]` profile keeps optimizations but adds line tables so frames resolve; the
`[profile.bench]` profile does the same for criterion binaries.

## Heap profiling (dhat)

```sh
cd <repo>
cargo run --release --features dhat-heap -- --server   # let the cold build run, then Ctrl-C
# writes dhat-heap.json -> open at https://nnethercote.github.io/dh_view/dh_view.html
```

This captures allocation count/bytes for the whole run — ideal for spotting churn in the build path
(per-file read buffers, posting inserts).

## Instruments (macOS, optional)

Xcode's *Time Profiler* and *Allocations* instruments can attach to the running daemon pid
(`rgx --server status` won't show it, but `pgrep -f 'rgx --server'` will). Useful when you want
allocation call trees without rebuilding.

## Allocator experiment

The build is trigram/hash-heavy, so it's allocator-sensitive. A quick A/B: add a `mimalloc` (or
`jemalloc`) `#[global_allocator]` behind a feature and re-run `cargo bench build_2000_files`. If it
moves the needle, that's a near-free win.

## Macro A/B vs ripgrep

[`bench/bench.sh`](../bench/bench.sh) is the end-to-end regression guard (warm daemon vs `rg`, mean ±
σ, records the `rg` version). See [`bench/baseline.txt`](../bench/baseline.txt) and the README
Benchmarks section.
