# CLAUDE.md

`rgx` is a candidate-index layer in front of ripgrep: the index narrows which files get scanned,
ripgrep does the matching. Correctness is ripgrep's, speed is ours.

## Docs

- [`README.md`](README.md) — what it is, install & use.
- [`docs/design.md`](docs/design.md) — mission, model, correctness contract, open questions.
- [`docs/cli.md`](docs/cli.md) — command surface and the `--server` gate.
- [`docs/mcp.md`](docs/mcp.md) — agent-facing MCP tools.
- [`docs/indexing.md`](docs/indexing.md) — streaming index, freshness, incremental updates, and the
  ripgrep-parity walk (which files get indexed, exactly like `rg`).
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
previous wire format and returns wrong or empty results. Alternatively, `rgx --server restart` (from
the repo) replaces just that project's daemon.

## Releasing

Tag-driven: pushing a `vX.Y.Z` tag runs [`.github/workflows/release.yml`](.github/workflows/release.yml)
(GitHub release, prebuilt binaries for every target, and the npm / crates.io / PyPI publishes — each
self-skips until its token/flag is set). Before tagging, bump the version in **all three** of:

- **`Cargo.toml`** — `version` (crates.io publish requires it to equal the tag).
- **`Cargo.lock`** — the `ripgrepx` entry; `cargo build` (or `cargo update -p ripgrepx`) refreshes it
  after the `Cargo.toml` bump.
- **`npm/package.json`** — `version` (CI also sets it from the tag at publish, but keep it in sync).

`pyproject.toml` is `dynamic` — maturin reads the `Cargo.toml` version, so do **not** edit it.
`install.sh`/README carry no concrete version. Then: commit as `release: X.Y.Z`, push `main`, and
`git tag vX.Y.Z && git push origin vX.Y.Z`.

## Conventions

- **Trunk-based.** All development happens on `main`; commit and push directly there.
- **Testable from conception.** Test components in isolation — pure functions, dependency injection
  over globals, seams that exercise the indexer, candidate selection, and ripgrep invocation without a
  live filesystem or daemon. Write the test alongside the code.
- **Docs and skill move with the code.** Any change to behavior, the command surface, or output shape
  updates the relevant `docs/`, `README.md`, and [`assets/skill.md`](assets/skill.md) in the same
  commit. `assets/skill.md` (embedded at build time, installed by `rgx --agent install`) is the
  agent-facing source of truth — never let docs or skill describe a state the code isn't in.
