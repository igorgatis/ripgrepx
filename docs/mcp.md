# MCP interface

`rgx --server mcp` runs a stdio MCP server that exposes `rgx`'s search to AI agents as tools. Each tool
proxies to the same shared index and background indexer the CLI uses, and returns **ripgrep-style
text** — the `path:line:text` shape models already know — rather than bespoke structured JSON.

`rgx` is self-contained (ripgrep's engine is a linked-in library), so **agents do not need the `rg`
binary installed** — just `rgx`.

## Setup

Register `rgx` as a stdio MCP server with your agent:

```sh
claude mcp add rgx -- rgx --server mcp          # Claude Code
```

Or add it to any MCP client's config directly:

```json
{ "rgx": { "command": "rgx", "args": ["--server", "mcp"] } }
```

The MCP server keys off the working directory it's launched in, so run it from (or point it at) the
project root you want searched. The index builds on first use and stays fresh on its own.

Optionally install the **skill** so the agent is taught to prefer `rgx` over `rg`/`grep`/`find`/`fd`:

```sh
rgx --skill        # writes ~/.claude/skills/rgx/SKILL.md (override dir via $RGX_SKILL_DIR)
```

The skill text is version-controlled in [`assets/skill.md`](../assets/skill.md).

## Tools

- **content search** — run a ripgrep query (regex by default, plus the usual literal / case /
  whole-word / glob / type / path-scope / context options). The index selects candidate files,
  ripgrep confirms; the match set is identical to a plain `rg` run. Results are returned in the
  token-savings view (grouped by file, paged) — pass `page` to fetch the next page.
- **file search** — locate files/directories by partial name or path (find/fd-style).
- **status** — what's indexed and whether an update is in flight.

## Response shape

- Content matches come back grouped by file: a `path` line, then `  line: text` for each match
  under it (context lines, when requested, use a `-` gutter). The leading header reports the page and
  total match/file counts; a trailing hint says how to fetch the next page when one exists.
- File search returns one path per line.
- **Paging:** results are paged — the response reports the window and tells the agent to call again
  with the next `page` — so an agent pulls more on demand instead of receiving one giant dump.
  Paging is cheap (the index is warm); nothing is dropped, so every match is reachable.
- **Freshness inline, only when actionable:** if a returned line no longer matches what's on disk,
  it's flagged so the agent re-reads that file rather than trusting stale text.

## Why ripgrep-style text

Same shape as the CLI, so the two surfaces are interchangeable — the same query yields the same bytes
either way, with zero new parsing for the agent.

## Skill

`rgx --skill` installs a short agent skill that teaches a model to prefer these tools over raw
`rg`/`find`/`fd`. The skill is version-controlled in [`assets/skill.md`](../assets/skill.md) and kept
in sync with behavior (see `CLAUDE.md`).

## Implementation status

This page describes the intended interface; the current stdio server implements a subset:

- **content_search** — `pattern` (regex) plus `case_insensitive` / `word` / `fixed_strings` /
  `multi_line`, and `page`; results in the compact, paged, grouped-by-file view.
- **file_search** — `query` substring over indexed paths.
- **status** — index readiness and counts.

Not yet wired through MCP: the rest of the ripgrep flag set (glob/type/path-scope/context are in the
CLI but not all surfaced as tool params) and inline **freshness flags**. Freshness is planned — the
model is real at the engine level (ripgrep matches the file on disk), but the stale-line marker is not
yet emitted in tool responses.
