# CLAUDE.md

`rgx` is a candidate-index layer in front of ripgrep: the index narrows which files get scanned,
ripgrep does the matching. Correctness is ripgrep's, speed is ours.

## Docs

- [`README.md`](README.md) — what it is, install & use.
- [`docs/design.md`](docs/design.md) — mission, model, correctness contract, open questions.
- [`docs/cli.md`](docs/cli.md) — command surface and the `--server` gate.
- [`docs/mcp.md`](docs/mcp.md) — agent-facing MCP tools.
- [`docs/indexing.md`](docs/indexing.md) — streaming index, freshness, incremental updates.
- [`docs/index-and-storage.md`](docs/index-and-storage.md) — trigram index design, storage, and
  benchmark results.
- [`docs/profiling.md`](docs/profiling.md) — profiling the build/query paths (criterion, samply, dhat).

## Commands

Tooling is managed by [mise](https://mise.jdx.dev). Run via `mise run <task>`:

- `mise run build:install` — build release and install rgx to `~/.local/bin`
- `mise run clean` — remove build artifacts and all untracked/ignored files
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
- **Keep the agent skill in sync.** The skill installed by `rgx --agent install` is the version-controlled
  [`assets/skill.md`](assets/skill.md) (embedded at build time). It is agent-facing documentation of
  the command surface and behavior, so the same rule as docs applies: any change to flags, the
  command surface, output shape, or freshness/MCP behavior must update `assets/skill.md` in the same
  commit. The skill must never teach agents a usage the code doesn't support.
