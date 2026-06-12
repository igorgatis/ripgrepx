# Contributing

`rgx` is a candidate-index layer in front of ripgrep — see [`README.md`](README.md) for what it is and
[`docs/design.md`](docs/design.md) for why it's built this way. This page is the development process:
how to build, test, profile, and release.

## Setup & commands

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

## Conventions

- **Trunk-based.** All development happens on `main`; commit and push directly there.
- **Testable from conception.** Test components in isolation — pure functions, dependency injection
  over globals, seams that exercise the indexer, candidate selection, and ripgrep invocation without a
  live filesystem or daemon. Write the test alongside the code. Prefer coarse-grained tests, and
  always include a happy-path case.
- **Docs and skill move with the code.** Any change to behavior, the command surface, or output shape
  updates the relevant `docs/`, `README.md`, and [`assets/skill.md`](assets/skill.md) in the same
  commit. `assets/skill.md` (embedded at build time, installed by `rgx --agent install`) is the
  agent-facing source of truth — never let docs or skill describe a state the code isn't in.

## Testing

`mise run test` runs the suite. Two suites carry the correctness contract and are worth knowing:

- **Query soundness** (`tests/soundness.rs`, `tests/index_soundness.rs`) — fuzz random patterns
  against the real `regex` engine and real files to prove the trigram query never drops a match
  ("zero missed matches"). The guarantee it protects is in
  [`docs/querying.md`](docs/querying.md#soundness-is-verified).
- **Walk parity** — a differential fuzz against the real `rg` binary (baked fixtures plus randomized
  ignore layouts) proving the index walk yields exactly the files `rg` would. The rule it protects is
  in [`docs/indexing.md`](docs/indexing.md#the-ripgrep-parity-walk).

For profiling and benchmarking the hot paths (criterion, samply, dhat, the macro A/B vs `rg`), see
[`docs/profiling.md`](docs/profiling.md).

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

### Registry caveats

Hard-won, and easy to trip over when cleaning up or re-cutting a release:

- **crates.io is immutable.** Published versions can't be deleted — only `cargo yank` (reversible, the
  version stays downloadable). A whole-crate delete is allowed only within 72 h of first publish; and
  after deleting, re-publishing the same name can 404 transiently, so a delete-then-republish within
  one release may need a retry.
- **npm name-lock.** Unpublishing the *last* remaining version removes the package and locks the name
  for **24 h**. Unpublish old versions *after* the new one is live, never before.
- **PyPI burns versions.** Deleting a release permanently reserves that version number — it can never
  be re-uploaded. Deletion is web-UI only (no API/CLI).
- **CI self-skips.** The npm / crates.io / PyPI publish jobs each skip silently when their token/secret
  is unset, so a "successful" workflow can still publish nothing — check the job logs.
