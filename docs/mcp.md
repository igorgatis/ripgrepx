# MCP interface

`rgx --agent mcp` runs a stdio MCP server that exposes `rgx`'s search to AI agents as tools. Each tool
proxies to the same shared index and background indexer the CLI uses, and returns **ripgrep-style
text** — the `path:line:text` shape models already know — rather than bespoke structured JSON.

`rgx` is self-contained (ripgrep's engine is a linked-in library), so **agents do not need the `rg`
binary installed** — just `rgx`.

## Setup

One command installs the rgx bundle (skill + MCP wiring) for each agent:

```sh
rgx --agent install            # auto-detect installed agents; or name: claude codex cursor gemini vscode
rgx --agent install gemini     # install for one agent (repeatable; --user or --project to pick scope)
rgx --agent list               # detected agents + install status
rgx --agent uninstall          # remove exactly what install wrote
```

`install` (and `uninstall`) print the exact changes and ask before touching anything — `--yes` (`-y`)
applies without prompting (required when stdin isn't a TTY), `--dry-run` (`-n`) only previews. They
write only where rgx owns the namespace, and edit shared files idempotently — never a blind append
(see [Bundles](#bundles)). MCP registration that belongs to a host's own CLI is printed for you to run
rather than executed, so nothing about your agent's config changes by surprise.

### Bundles

| Agent | Teach (skill/rules) | MCP | Default scope |
| --- | --- | --- | --- |
| **Claude Code** | `…/.claude/skills/rgx/SKILL.md` | prints `claude mcp add rgx -- rgx --agent mcp` | user |
| **Codex** | marked block in `…/.codex/AGENTS.md` | prints `codex mcp add rgx -- rgx --agent mcp` | user |
| **Gemini CLI** | `…/.gemini/extensions/rgx/GEMINI.md` | bundled in `gemini-extension.json` | user |
| **Cursor** | `.cursor/rules/rgx.mdc` (with frontmatter) | `"rgx"` in `.cursor/mcp.json` (`mcpServers`) | project |
| **VS Code** | block in `.github/copilot-instructions.md` | `"rgx"` in `.vscode/mcp.json` (`servers`) | project |

Scope defaults to user-global where the tool supports it; pass `--project` to commit it into the repo
or `--user` to keep Cursor/VS Code out of the tree (Cursor is project-only — it has no file-based user
rules). For any other MCP client, register `rgx --agent mcp` as a stdio server by hand:

```jsonc
{ "mcpServers": { "rgx": { "command": "rgx", "args": ["--agent", "mcp"] } } }
// VS Code's .vscode/mcp.json uses "servers" instead of "mcpServers".
```

The MCP server keys off the working directory it's launched in, so run it from (or point it at) the
project root you want searched. The index builds on first use and stays fresh on its own. The skill
text is version-controlled in [`assets/skill.md`](../assets/skill.md).

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
- **Paging:** results are paged via an **opaque cursor** the agent echoes back — the cursor records the
  exact query and a keyset resume position, so the next page can't drift to a different search, and a
  result set that changed between pages is reported with a `note:` line. The cursor is a short id the
  daemon holds for ~2 min and is single-use; an expired one returns `pagination expired — re-run the
  search`. Paging is cheap (the index is warm); nothing is dropped, so every match is reachable.
- **Freshness inline, only when actionable:** if a returned line no longer matches what's on disk,
  it's flagged so the agent re-reads that file rather than trusting stale text.

## Why ripgrep-style text

Same shape as the CLI, so the two surfaces are interchangeable — the same query yields the same bytes
either way, with zero new parsing for the agent.

## Skill

`rgx --agent install` installs the rgx bundle (the skill that teaches a model to prefer these tools
over raw `rg`/`grep`/`find`/`fd`, plus the MCP wiring) for each agent — previewing the exact changes
and asking before it touches anything; `uninstall` reverses it and `list` shows status (see
[Setup](#setup)). `rgx --agent skill` prints the raw markdown so you can paste it into any agent's
instructions. The skill is version-controlled in [`assets/skill.md`](../assets/skill.md) and kept in
sync with behavior (see `CLAUDE.md`).

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
