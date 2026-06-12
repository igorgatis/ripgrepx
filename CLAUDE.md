# CLAUDE.md

`rgx` is a candidate-index layer in front of ripgrep: the index narrows which files get scanned,
ripgrep does the matching. Correctness is ripgrep's, speed is ours.

## Docs

- [`README.md`](README.md) — what it is, install & use.
- [`docs/design.md`](docs/design.md) — mission, model, correctness contract, open questions.
- [`docs/ripgrep-ignore-and-scope.md`](docs/ripgrep-ignore-and-scope.md) — parity spec: how ripgrep
  decides ignores and scope (from the `ignore` crate source), and what it dictates for index reuse.
- [`docs/cli.md`](docs/cli.md) — command surface and the `--server` gate.
- [`docs/mcp.md`](docs/mcp.md) — agent-facing MCP tools.
- [`docs/indexing.md`](docs/indexing.md) — streaming index, freshness, incremental updates.
- [`docs/index-and-storage.md`](docs/index-and-storage.md) — trigram index design, storage, and
  benchmark results.
- [`docs/profiling.md`](docs/profiling.md) — profiling the build/query paths (criterion, samply, dhat).

## Commands

Tooling is managed by [mise](https://mise.jdx.dev). Run via `mise run <task>`:

- `mise run build:install` — build release and install rgx to `~/.local/bin`
- `mise run run -- <args>` — run rgx with the given args
- `mise run clean` — remove build artifacts and all untracked/ignored files
- `mise run test` — run tests
- `mise run fmt` / `fmt-check` — format / check formatting
- `mise run lint` — clippy, warnings as errors
- `mise run fix` — auto-fix clippy lints and formatting
- `mise run ci` — fmt-check + lint + test

After `build:install`, kill stale daemons (`pkill -f 'rgx.*server'`) — an old daemon keeps serving the
previous wire format and returns wrong or empty results.

## Conventions

- **Trunk-based.** All development happens on `main`; commit and push directly there.
- **Testable from conception.** Test components in isolation — pure functions, dependency injection
  over globals, seams that exercise the indexer, candidate selection, and ripgrep invocation without a
  live filesystem or daemon. Write the test alongside the code.
- **Docs and skill move with the code.** Any change to behavior, the command surface, or output shape
  updates the relevant `docs/`, `README.md`, and [`assets/skill.md`](assets/skill.md) in the same
  commit. `assets/skill.md` (embedded at build time, installed by `rgx --agent install`) is the
  agent-facing source of truth — never let docs or skill describe a state the code isn't in.
