# Design

## Mission

Make ripgrep instant on a project you search over and over. Agents and humans grep the same tree
hundreds of times an hour; `rgx` walks it once, keeps that knowledge fresh as files change, and then
each search only looks at the files that could possibly match. Two goals dominate every choice:
**be fast**, and **be effortless for an AI agent to use well**.

## The model: a candidate index in front of ripgrep

`rgx` is not a new grep. It is a **candidate selector** layered in front of the ripgrep crates:

1. **Index** — answers "which files could contain a match for this pattern?" quickly, from a
   persistent index of which files contain what.
2. **Confirm** — ripgrep runs its real search over just those candidate files and produces the
   actual matches.

The division of labor is the whole point:

- **Correctness is ripgrep's.** Matching is done by ripgrep against the real files on disk, so the
  semantics, patterns, flags, and output are exactly `rg`'s.
- **Speed is ours.** The index only changes *how few files get scanned*, never *what comes back*.

### Why this is safe

- A **stale** index can only cost a little speed — a few extra candidate files, or a quick re-check
  — never a missed or invented result.
- When the index **can't accelerate** a query (a pattern the index can't reason about, or a
  not-yet-indexed area), `rgx` **falls back to a normal scan** transparently.
- The output is byte-for-byte ripgrep's, so `alias rg=rgx` is a true drop-in.

This guarantee — correctness from ripgrep, speed from the index — should survive every future design
decision.

### The one deliberate exception: `--compact`

The opt-in `--compact` view (and the MCP `page` parameter) reshapes output to save agent tokens:
results are grouped by file (the path printed once), paged, and long lines are trimmed around the
match. This is the **only** surface that is not byte-for-byte `rg`, and the carve-out is narrow: the
**match set is still exactly ripgrep's** — nothing is added, and nothing is ever silently dropped
(pagination is the sole volume control, and every match is reachable by fetching the next page). Only
*presentation* changes. A bare `rgx <pattern>` is untouched, so the drop-in promise above still holds.

## Search scope

Search semantics are **exactly ripgrep's**. `rgx` does **not** add symbol awareness or
semantic/meaning-based search; the only novel work is the indexer that narrows the candidate file
set. On top of content search, it provides **file/directory lookup by name or path** (find/fd-style)
straight from the index, so it's the one stop shop for "find things in this codebase."

## Audience

Built first for **AI coding agents** (via MCP), with a first-class CLI that doubles as a drop-in
`rg`. The agent ergonomics — familiar output, low noise, paging, inline freshness flags, an
installable skill — are core, not an afterthought.

## Drop-in and the `--server` gate

A bare `rgx <pattern>` is always a plain ripgrep search, so rgx adds as few flags to rg's surface as
possible — four, and only ever recognized as the **leading token**:

- **Search modes** — the bare `<pattern>` (content) and `--find` (file/dir names). These are
  searches, so they belong next to rg's flags.
- **`--compact`** — the same content search rendered as the token-savings view (grouped + paged); it
  also accepts `--page-size N`, the `-l`/`-c` orientation modes, and `--cursor TOK` to fetch the next
  page (an opaque, self-contained cursor — see [`cli.md`](cli.md)). An opt-in modifier, so the bare
  search stays `rg`-identical.
- **`--skill`** — a one-shot install of the agent skill.
- **`--server` (the gate)** — flips rgx out of ripgrep-passthrough into its own subcommand grammar
  (`start`, `stop`, `status`, `mcp`). Everything daemon-related lives behind it, so management never
  shares rg's flag namespace and can never collide with a present or future rg flag.

So:

- `rgx status` → greps for "status"
- `rgx --server status` → reports index health

ripgrep does not define these flags, so they cannot collide with a real `rg` flag. To search for a
literal that looks like a flag, use ripgrep's own escapes (`-e`, or a trailing `--`). Settings live
in a project `.toml` the user edits directly — there is no config-editing CLI.

## Open questions

- **`--find` flag space.** `rgx --find <name>` is clean for the simple case. If file search grows
  options (globs, type filters, dirs-only), `rgx --find config --type d --glob '*.rs'` is simplest
  but lets file-search flags share rg's namespace. If that proves cramped, `--find` can follow the
  `--server` precedent and become a gate with its own subcommand grammar.
- **Headline command list.** Whether `--skill` and the `--server` subcommands belong in the primary
  help output or stay secondary.

(Storage is settled — an in-RAM trigram index with a versioned on-disk snapshot; see
[`index-and-storage.md`](index-and-storage.md). It's an implementation detail that doesn't affect the
model or guarantees above.)
