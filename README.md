# ripgrepx (`rgx`)

[![CI](https://github.com/igorgatis/ripgrepx/actions/workflows/ci.yml/badge.svg)](https://github.com/igorgatis/ripgrepx/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/ripgrepx.svg)](https://pypi.org/project/ripgrepx/)
[![npm](https://img.shields.io/npm/v/ripgrepx.svg)](https://www.npmjs.com/package/ripgrepx)
[![crates.io](https://img.shields.io/crates/v/ripgrepx.svg)](https://crates.io/crates/ripgrepx)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Instant ripgrep for codebases you search over and over.**

`rgx` is [ripgrep](https://github.com/BurntSushi/ripgrep)'s matcher fronted by a [Russ Cox–style
trigram index](https://swtch.com/~rsc/regexp/regexp4.html) — the candidate-index idea behind Google
Code Search and [`zoekt`](https://github.com/sourcegraph/zoekt). The index narrows *which* files to
scan; **ripgrep still does the matching**, so results are byte-for-byte `rg`'s, just faster. A stale
index can only cost a little speed, never a missed or invented match. It searches content (full
ripgrep regex) and locates files by name (find/fd-style), from the terminal or an AI agent over MCP.

Warm, `rgx` answers most queries in well under 60 ms where `rg` takes 100 ms to 2.5 s — a **15–50×**
speedup on the kind of symbol searches a developer actually runs, up to **128×** on the most
selective. See the [benchmarks](#benchmarks) for the full numbers.

## Install

`rgx` is one self-contained ~4 MB binary — ripgrep's engine is linked in, so you do **not** need `rg`
installed. Pick whichever channel you prefer:

```sh
# curl | sh — prebuilt binary, no toolchain (macOS/Linux; re-run to update)
curl -fsSL https://raw.githubusercontent.com/igorgatis/ripgrepx/main/install.sh | sh

# npm — fetches the right prebuilt binary
npm install -g ripgrepx

# pipx (or pip) — prebuilt wheel that bundles the binary
pipx install ripgrepx

# Cargo — prebuilt via binstall, or compiled from source
cargo binstall ripgrepx
cargo install ripgrepx
```

Or download a prebuilt archive (Windows included) from the
[latest release](https://github.com/igorgatis/ripgrepx/releases/latest) and put `rgx` on your `PATH`.
On **Windows**, use npm, pipx, Cargo, or the release `.zip` (`x86_64-pc-windows-msvc` /
`aarch64-pc-windows-msvc`).

## For AI agents

`rgx` is built first for AI coding agents: fast, token-frugal code search an agent calls over **MCP**
or as a **CLI**. After installing the binary above, wire it into your agent in two steps.

**1. Teach the agent** to prefer rgx over rg/grep/find/fd. For Claude Code, `rgx --agent install`
writes the skill to `~/.claude/skills/rgx/SKILL.md`; for any other agent, `rgx --agent skill` prints
the same markdown — append it to that agent's instructions/rules file.

**2. (optional) Register the MCP server** — exposes `content_search`, `file_search`, and `status` as
tools. The recommended setup per agent:

| Agent | Teach the agent (skill / rules) | Register the MCP server |
| --- | --- | --- |
| **Claude Code** | `rgx --agent install` | `claude mcp add rgx -- rgx --agent mcp` |
| **Codex** | `rgx --agent skill >> AGENTS.md` | `codex mcp add rgx -- rgx --agent mcp` |
| **Cursor** | `rgx --agent skill > .cursor/rules/rgx.md` | add the JSON below to `.cursor/mcp.json` |
| **Gemini CLI** | `rgx --agent skill >> GEMINI.md` | `gemini mcp add rgx -- rgx --agent mcp` |
| **VS Code (Copilot)** | `rgx --agent skill >> .github/copilot-instructions.md` | `code --add-mcp '{"name":"rgx","command":"rgx","args":["--agent","mcp"]}'` |
| **Any MCP client** | paste `rgx --agent skill` into its instructions | the JSON below |

```jsonc
// Cursor (~/.cursor/mcp.json or .cursor/mcp.json), Windsurf, and most MCP clients:
{ "mcpServers": { "rgx": { "command": "rgx", "args": ["--agent", "mcp"] } } }
```

```toml
# Codex — if you prefer editing ~/.codex/config.toml over `codex mcp add`:
[mcp_servers.rgx]
command = "rgx"
args = ["--agent", "mcp"]
```

### Token savings (`--compact`)

Like [rtk](https://github.com/rtk-ai/rtk), `rgx` can compact search output to save agent tokens:
`--compact` groups matches by file (the path is printed once), pages the result behind an opaque
cursor, and trims very long lines around the match. Unlike a lossy filter, **nothing is dropped** —
the match set is exactly `rg`'s, the header reports the full total so you know what you have not seen,
and because the index is warm, fetching the next page is cheap, so every match stays reachable.

```sh
rgx --compact 'fn .*Handler'                 # grouped + paged; footer prints the next-page command
rgx --compact --cursor '<token>'             # next page (token copied from the footer)
rgx --compact -l 'fn .*Handler'              # matching files only;  -c for per-file counts
```

```
[matches 1-50 of 142 in 18 files]
src/server.rs
  210: fn content_search(...) -> Result<()> {
src/main.rs
  168: fn content_cmd(args: &[String]) -> ExitCode {
next: rgx --compact --cursor 'AQEAAAAAAA...'
```

The cursor carries the entire query (pattern + every flag) plus a keyset resume position, so the next
page is always the same search — never a different one — and a result set that changed between pages
is flagged with a `note:` line.

### MCP or CLI

- **MCP** — `rgx --agent mcp` exposes `content_search` (returns the `--compact` paged view by
  default; pass the response `cursor` to advance, or `files_only`/`count` to orient), `file_search`,
  and `status`. See [`docs/mcp.md`](docs/mcp.md).
- **CLI** — a near-drop-in for `rg`: `rgx <pattern>` takes the same command line and just runs
  faster. A bare `rgx <pattern>` is plain (accelerated) ripgrep; `rgx --find <name>` locates files; `--server`
  manages the daemon. See [`docs/cli.md`](docs/cli.md).

State (index + daemon socket) lives outside the repo under `$RGX_CACHE_DIR`, else the config file's
`cache_dir`, else `$XDG_CACHE_HOME/rgx`, else `~/.cache/rgx` — a rebuildable cache, safe to delete,
never written into the indexed tree.

### Config

Optional TOML at `$RGX_CONFIG`, else `$XDG_CONFIG_HOME/rgx/config.toml`, else
`~/.config/rgx/config.toml`. A missing file is fine; a malformed or invalid one is an error.

```toml
# Base directory for the rebuildable cache (index + socket). $RGX_CACHE_DIR overrides this.
# Must be an absolute path (no ~ expansion).
cache_dir = "/var/tmp/rgx-cache"

# Persist the index only if the cold build took at least this long; below it the index stays
# RAM-only and is rebuilt on each daemon start. 0 always persists. Default 1000.
persist_threshold_ms = 1000

# Exit the daemon after this many seconds with no search, freeing its RAM; the next search
# respawns it. Zero or negative stays resident forever. Default 3600.
idle_timeout_secs = 3600
```

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
