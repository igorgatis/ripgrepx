# ripgrepx (`rgx`)

**Instant ripgrep for projects you search again and again.**

`rgx` keeps a persistent, always-fresh index of which files contain what, so every search jumps
straight to the candidate files instead of walking the whole tree — but **ripgrep still does the
matching**, so results, patterns, flags, and output are byte-for-byte what `rg` gives you. It's a
true drop-in: `alias rg=rgx` and every command you already type just gets faster. A stale or
imperfect index can only cost a little speed, never a missed or invented match.

`rgx` aims to be the **one stop shop for finding things in a codebase** — content search with full
ripgrep semantics, plus file/directory lookup by name or path (find/fd-style) — usable both from
the terminal and directly by AI agents over MCP.

## Install & use

> Installation is still TBD.

Alias `rgx` over ripgrep and keep working exactly as before — every command just gets faster:

```sh
alias rg=rgx
rg TODO -t rust          # accelerated content search, identical results to plain rg
rgx --find config        # locate files/dirs by name or path (find/fd-style)
rgx --server status      # index health, and whether an update is in flight
```

A bare `rgx <pattern>` is always a plain (accelerated) ripgrep search; the `--server` gate holds the
daemon commands. See [`docs/cli.md`](docs/cli.md) for the full surface.

### Where state lives

The index and daemon socket are kept outside the repo, under a per-project cache dir keyed by the
canonical root — rgx never writes into the tree it indexes. The location resolves as:

- `$RGX_CACHE_DIR/<hash>/` if set — relocates **only** rgx's state;
- else `$XDG_CACHE_HOME/rgx/<hash>/` (note: `$XDG_CACHE_HOME` is shared by other tools);
- else `~/.cache/rgx/<hash>/`.

The contents are a rebuildable cache — safe to delete; rgx re-indexes on the next run.

### Token-savings view (`--compact`)

For agents (or anyone) who want a denser result, `rgx --compact <pattern>` groups matches by file
(the path is printed once), pages the output, and trims very long lines around the match:

```sh
rgx --compact 'fn .*Handler'     # grouped + paged; footer shows the next-page command
rgx --compact --page 2 'fn .*Handler'
```

This is the one surface that is not byte-for-byte `rg`, and the trade is narrow: the match set is
still exactly ripgrep's — nothing is added or silently dropped. Pagination is the only volume
control, and because the index is warm, fetching the next page is cheap, so every match stays
reachable. Over MCP this is how `content_search` returns results (pass `page` to advance).

## Use with AI agents (MCP)

`rgx` is self-contained — ripgrep's engine is linked in, so **you do not need `rg` installed**.

Register `rgx` as an MCP server so an agent can search through it:

```sh
claude mcp add rgx -- rgx --server mcp        # Claude Code
```

or add it to any MCP client config:

```json
{ "rgx": { "command": "rgx", "args": ["--server", "mcp"] } }
```

The MCP server exposes `content_search`, `file_search`, and `status` tools. `content_search` returns
the token-savings view (grouped by file, paged — pass `page` to advance), with a match set identical
to `rg`. To also teach an agent to *prefer* `rgx` over `rg`/`grep`/`find`/`fd`, install the bundled
skill:

```sh
rgx --skill        # installs ~/.claude/skills/rgx/SKILL.md and prints MCP setup
```

See [`docs/mcp.md`](docs/mcp.md) for the full agent integration guide.

## Benchmarks

rgx (**warm daemon**, index resident) vs **ripgrep 15.1.0** on four real repositories. Output is
byte-for-byte `rg`'s, so this measures only how much less work the index lets ripgrep do.

| repo | files | index size | cold build |
| --- | --- | --- | --- |
| [lucene](https://github.com/apache/lucene) | 7.4k | 22 MB | ~1.5 s |
| [vscode](https://github.com/microsoft/vscode) | 15.1k | 46 MB | ~1.2 s |
| [kubernetes](https://github.com/kubernetes/kubernetes) | 30.2k | 53 MB | ~1.5 s |
| [linux](https://github.com/torvalds/linux) | 93.6k | 210 MB | ~7.4 s |

Real queries (the kind of symbol / error string / API name a developer actually searches for, drawn
from each project's own code and commit history), `mean ± σ` over 10 runs:

| repo | query | `rg` | `rgx` | speedup |
| --- | --- | --- | --- | --- |
| lucene | `CorruptIndexException` | 101 ± 2 ms | 4.6 ± 0.2 ms | **22×** |
| lucene | `IndexWriter` | 103 ± 1 ms | 17.8 ± 0.8 ms | **6×** |
| lucene | `TieredMergePolicy\|LogMergePolicy` | 101 ± 1 ms | 6.3 ± 0.3 ms | **16×** |
| vscode | `TreeDataProvider` | 198 ± 2 ms | 4.1 ± 0.1 ms | **48×** |
| vscode | `onDidChangeConfiguration` | 201 ± 2 ms | 13.6 ± 0.3 ms | **15×** |
| vscode | `registerCommand` | 200 ± 2 ms | 14.0 ± 0.2 ms | **14×** |
| kubernetes | `func (kl *Kubelet)` | 409 ± 6 ms | 3.2 ± 0.2 ms | **128×** |
| kubernetes | `context deadline exceeded` | 418 ± 7 ms | 5.7 ± 0.1 ms | **73×** |
| kubernetes | `EndpointSlice` | 419 ± 9 ms | 8.4 ± 0.2 ms | **50×** |
| kubernetes | `metav1.ObjectMeta` | 411 ± 10 ms | 29.9 ± 0.2 ms | **14×** |
| linux | `struct task_struct` | 1803 ± 373 ms | 42.8 ± 1.0 ms | **42×** |
| linux | `kmalloc` | 2308 ± 507 ms | 57.5 ± 1.4 ms | **40×** |
| linux | `EXPORT_SYMBOL_GPL` | 1606 ± 56 ms | 54.0 ± 1.3 ms | **30×** |
| linux | `MODULE_LICENSE` (broad) | 2518 ± 176 ms | 161.6 ± 1.8 ms | **16×** |

The more selective the query, the bigger the win (a rare symbol touches few files; a `func (kl
*Kubelet)` receiver hits 13 of 30k). rgx is also markedly **more consistent**: its σ stays sub-2 ms
while a full `rg` scan's swings with cache state (linux `kmalloc`: rg 2308 ± 507 ms vs rgx 57 ± 1 ms).
The full set (and the fallback rows below) is in [`bench/baseline.txt`](bench/baseline.txt).

**Honest caveat.** A *fallback* query the index can't narrow — no usable trigram, e.g. `\w+` or a
2-char pattern — is handled by an in-process pipelined scan and lands at **parity** with `rg`. The one
exception is a *match-everything* query like `.*` over the largest repo (printing all 1.5 GB), at
~**0.8×**: a degenerate "cat the repo", not a search. See
[`docs/index-and-storage.md`](docs/index-and-storage.md) §8 for why.

### Methodology

- Machine: 12-core / 24 GB, macOS; **ripgrep 15.1.0** (`rg --version`, recorded by the harness);
  timings via `hyperfine` (1 warmup, 10 runs, reported as mean ± σ), output discarded.
- `rgx <pattern> <repo>` (CLI talking to its warm daemon) vs `rg -n <pattern> <repo>`; both pipe to
  the same sink, so the comparison is apples-to-apples.
- Reproduce: `RGX=target/release/rgx bench/bench.sh <repo> <pattern>...` (the script prints the `rg`
  version, warms the daemon, benchmarks each pattern, and flags any regression). Numbers vary with
  hardware and cache state.

## Documentation

- [`docs/design.md`](docs/design.md) — mission, the index-in-front-of-ripgrep model, correctness
  contract, open questions.
- [`docs/cli.md`](docs/cli.md) — command surface and the `--server` gate.
- [`docs/mcp.md`](docs/mcp.md) — the agent-facing MCP tools.
- [`docs/indexing.md`](docs/indexing.md) — streaming index, freshness, incremental updates.
- [`docs/profiling.md`](docs/profiling.md) — how to profile build/query (criterion, samply, dhat).
- [`docs/index-and-storage.md`](docs/index-and-storage.md) — trigram index design, storage engine
  choice, and benchmark results vs `rg`.

## License

MIT — see [LICENSE](LICENSE).
