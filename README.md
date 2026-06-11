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

The MCP server exposes `content_search`, `file_search`, and `status` tools, returning the same
`path:line:text` shape as the CLI. To also teach an agent to *prefer* `rgx` over `rg`/`grep`/`find`/`fd`,
install the bundled skill:

```sh
rgx --skill        # installs ~/.claude/skills/rgx/SKILL.md and prints MCP setup
```

See [`docs/mcp.md`](docs/mcp.md) for the full agent integration guide.

## Benchmarks

rgx vs ripgrep on four real repositories, **warm daemon** (index resident). Output is byte-for-byte
`rg`'s, so this measures only how much less work the index lets ripgrep do.

Times are `mean ± σ` over 10 runs (one standard deviation):

| repo | files | index size | cold build | example query | `rg` | `rgx` | speedup |
| --- | --- | --- | --- | --- | --- | --- | --- |
| lucene | 7.4k | 22 MB | ~1.5 s | `MergePolicy` | 102 ± 2 ms | 8.2 ± 0.2 ms | **12×** |
| vscode | 15.1k | 46 MB | ~1.2 s | `createDecorator` | 200 ± 1 ms | 12.5 ± 0.2 ms | **16×** |
| kubernetes | 30.2k | 53 MB | ~1.5 s | `PodSpec` | 422 ± 5 ms | 15.2 ± 0.3 ms | **27×** |
| linux | 93.6k | 210 MB | ~7.4 s | `EXPORT_SYMBOL_GPL` | 1411 ± 37 ms | 54 ± 1 ms | **26×** |

Across query classes (kubernetes): literal **12–27×**, alternation (`A|B|C`) **23×**. The win scales
with repo size — the bigger the tree, the more ripgrep work the index removes. rgx is also markedly
**more consistent**: its σ stays around 0.2–1.7 ms while a full `rg` scan's varies far more with cache
state (e.g. linux `spin_lock_irqsave`: rg 2056 ± 698 ms vs rgx 55.6 ± 0.7 ms).

**Honest caveat.** A *fallback* query that the index can't narrow — one with no usable trigram, e.g.
`\w+` or a 2-char pattern — is handled by an in-process pipelined scan and lands at **parity** with
`rg`. The one exception is a *match-everything* query like `.*` over the largest repo (printing all
1.5 GB), which is ~**0.8×**: a degenerate "cat the repo", not a search. See
[`docs/index-and-storage.md`](docs/index-and-storage.md) §9b for why.

### Methodology

- Machine: 12-core / 24 GB, macOS; ripgrep 15.1.0; timings via `hyperfine` (1 warmup, 10 runs,
  reported as mean ± σ), output discarded.
- `rgx <pattern> <repo>` (CLI talking to its warm daemon) vs `rg -n <pattern> <repo>`; both pipe to
  the same sink, so the comparison is apples-to-apples.
- Reproduce: `RGX=target/release/rgx bench/bench.sh <repo> <pattern>...` (the script warms the daemon,
  then benchmarks each pattern and flags any regression). Numbers vary with hardware and cache state.

## Documentation

- [`docs/design.md`](docs/design.md) — mission, the index-in-front-of-ripgrep model, correctness
  contract, open questions.
- [`docs/cli.md`](docs/cli.md) — command surface and the `--server` gate.
- [`docs/mcp.md`](docs/mcp.md) — the agent-facing MCP tools.
- [`docs/indexing.md`](docs/indexing.md) — streaming index, freshness, incremental updates.
- [`docs/index-and-storage.md`](docs/index-and-storage.md) — trigram index design, storage engine
  choice, benchmark results vs `rg`, and open hypotheses.

## License

MIT — see [LICENSE](LICENSE).
