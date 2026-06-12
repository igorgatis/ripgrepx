# CLAUDE.md

`rgx` is a candidate-index layer in front of ripgrep: the index narrows which files get scanned,
ripgrep does the matching. Correctness is ripgrep's, speed is ours.

## Docs

- [`README.md`](README.md) — what it is, install & use.
- [`docs/design.md`](docs/design.md) — mission, model, correctness contract, open questions.
- [`docs/cli.md`](docs/cli.md) — command surface and the `--server` gate.
- [`docs/mcp.md`](docs/mcp.md) — agent-facing MCP tools.
- [`docs/indexing.md`](docs/indexing.md) — write path: building, storing, and keeping the trigram
  index fresh, plus the ripgrep-parity walk.
- [`docs/querying.md`](docs/querying.md) — read path: regex→trigram query, candidate selection,
  ripgrep confirm, output/paging.
- [`docs/profiling.md`](docs/profiling.md) — profiling the build/query paths (criterion, samply, dhat).

Development process — build/test/lint commands, conventions, testing, and the release flow — lives in
[`CONTRIBUTING.md`](CONTRIBUTING.md), imported below.

@CONTRIBUTING.md
