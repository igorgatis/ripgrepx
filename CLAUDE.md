# CLAUDE.md

`rgx` is a candidate-index layer in front of ripgrep: the index narrows which files get scanned,
ripgrep does the matching. Correctness is ripgrep's, speed is ours.

## Docs

- [`README.md`](README.md) — what it is, install & use.
- [`docs/design.md`](docs/design.md) — mission, model, correctness contract, open questions.
- [`docs/cli.md`](docs/cli.md) — command surface and the `--server` gate.
- [`docs/mcp.md`](docs/mcp.md) — agent-facing MCP tools.
- [`docs/indexing.md`](docs/indexing.md) — streaming index, freshness, incremental updates.

## Commands

Tooling is managed by [mise](https://mise.jdx.dev). Run via `mise run <task>`:

- `mise run build` — compile
- `mise run test` — run tests
- `mise run fmt` / `fmt-check` — format / check formatting
- `mise run lint` — clippy, warnings as errors
- `mise run ci` — fmt-check + lint + test

## Conventions

- **Testable from conception.** Design every component to be tested in isolation — favor pure
  functions, dependency injection over globals, and seams that let the indexer, candidate selection,
  and ripgrep invocation be exercised without a live filesystem or daemon. Write the test alongside
  the code, not after.
- **Keep docs in sync.** Treat `docs/`, `README.md`, and this file as part of the change: before
  committing anything that alters behavior, the command surface, or the design, update the relevant
  doc in the same commit. Docs must never describe a state the code isn't in.
