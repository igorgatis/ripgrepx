# MCP interface

`rgx --agent mcp` runs a stdio MCP server that exposes `rgx`'s search to AI agents as tools. Each tool
proxies to the same shared index and background indexer the CLI uses, and returns **ripgrep-style
text** — the `path:line:text` shape models already know — rather than bespoke structured JSON.

`rgx` is self-contained (ripgrep's engine is a linked-in library), so **agents do not need the `rg`
binary installed** — just `rgx`.

## Setup

Register `rgx --agent mcp` as a stdio MCP server with your agent (recommended setup per agent):

```sh
claude mcp add rgx -- rgx --agent mcp                                              # Claude Code
codex mcp add rgx -- rgx --agent mcp                                               # Codex
gemini mcp add rgx -- rgx --agent mcp                                              # Gemini CLI
code --add-mcp '{"name":"rgx","command":"rgx","args":["--agent","mcp"]}'           # VS Code (Copilot)
```

```jsonc
// Cursor (~/.cursor/mcp.json or .cursor/mcp.json), Windsurf, and most MCP clients:
{ "mcpServers": { "rgx": { "command": "rgx", "args": ["--agent", "mcp"] } } }
```

```toml
# Codex — if you prefer editing ~/.codex/config.toml over `codex mcp add`:
[mcp_servers.rgx]
command = "rgx"
args = ["--agent", "mcp"]
```

The MCP server keys off the working directory it's launched in, so run it from (or point it at) the
project root you want searched. The index builds on first use and stays fresh on its own.

Optionally add the **skill** so the agent is taught to prefer `rgx` over `rg`/`grep`/`find`/`fd`:

```sh
rgx --agent install   # writes ~/.claude/skills/rgx/SKILL.md (override dir via $RGX_SKILL_DIR) + prints MCP setup
rgx --agent skill     # just print the skill markdown (paste into Codex AGENTS.md or any agent's instructions)
```

The skill text is version-controlled in [`assets/skill.md`](../assets/skill.md).

## Tools

- **content search** — run a ripgrep query (regex by default, plus the usual literal / case /
  whole-word / glob / type / path-scope / context options). The index selects candidate files,
  ripgrep confirms; the match set is identical to a plain `rg` run. Results are returned in the
  token-savings view (grouped by file, paged) — pass the response's `cursor` to fetch the next page,
  or `files_only` / `count` for a quick scope read.
- **file search** — locate files/directories by partial name or path (find/fd-style).
- **status** — what's indexed and whether an update is in flight.

## Response shape

- Content matches come back grouped by file: a `path` line, then `  line: text` for each match
  under it (context lines, when requested, use a `-` gutter). The leading header reports the window
  and the total match/file counts (`[matches 1-50 of 421 in 88 files]`), so the agent always knows
  how much it has not seen; a trailing hint carries the cursor for the next page when one exists.
- File search returns a `[files X-Y of N]` header then one path per line, with a trailing `after` hint
  when more remain.
- **Paging:** results are paged via an **opaque cursor** the agent echoes back — the cursor carries the
  exact query and a keyset resume position, so the next page can't drift to a different search, and a
  result set that changed between pages is reported with a `note:` line. Paging is cheap (the index is
  warm); nothing is dropped, so every match is reachable.
- **Freshness inline, only when actionable:** if a returned line no longer matches what's on disk,
  it's flagged so the agent re-reads that file rather than trusting stale text.

## Why ripgrep-style text

Same shape as the CLI, so the two surfaces are interchangeable — the same query yields the same bytes
either way, with zero new parsing for the agent.

## Skill

`rgx --agent install` writes a short agent skill (and prints the MCP setup) that teaches a model to
prefer these tools over raw `rg`/`find`/`fd`; `rgx --agent skill` prints the same markdown so you can
paste it into Codex `AGENTS.md` or any other agent's instructions. The skill is version-controlled in
[`assets/skill.md`](../assets/skill.md) and kept in sync with behavior (see `CLAUDE.md`).

## Implementation status

This page describes the intended interface; the current stdio server implements a subset:

- **content_search** — `pattern` (regex) plus `case_insensitive` / `word` / `fixed_strings` /
  `multi_line`, `page_size`, the `files_only` / `count` orientation modes, and `cursor` (resume,
  supersedes the other args); results in the compact, paged, grouped-by-file view.
- **file_search** — `query` substring over indexed paths, plus `limit` and `after` (keyset paging);
  reports the true total.
- **status** — index readiness and counts.

Not yet wired through MCP: the rest of the ripgrep flag set (glob/type/path-scope/context are in the
CLI but not all surfaced as tool params) and inline **freshness flags**. Freshness is planned — the
model is real at the engine level (ripgrep matches the file on disk), but the stale-line marker is not
yet emitted in tool responses.
