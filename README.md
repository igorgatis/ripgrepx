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
