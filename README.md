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

## Documentation

- [`docs/design.md`](docs/design.md) — mission, the index-in-front-of-ripgrep model, correctness
  contract, open questions.
- [`docs/cli.md`](docs/cli.md) — command surface and the `--server` gate.
- [`docs/mcp.md`](docs/mcp.md) — the agent-facing MCP tools.
- [`docs/indexing.md`](docs/indexing.md) — streaming index, freshness, incremental updates.

## License

MIT — see [LICENSE](LICENSE).
