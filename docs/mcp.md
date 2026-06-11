# MCP interface

`rgx --server mcp` runs a stdio MCP server that exposes `rgx`'s search to AI agents as tools. Each tool
proxies to the same shared index and background indexer the CLI uses, and returns **ripgrep-style
text** — the `path:line:text` shape models already know — rather than bespoke structured JSON.

## Tools

- **content search** — run a ripgrep query (regex by default, plus the usual literal / case /
  whole-word / glob / type / path-scope / context options). The index selects candidate files,
  ripgrep confirms; results are identical to a plain `rg` run.
- **file search** — locate files/directories by partial name or path (find/fd-style).
- **status** — what's indexed and whether an update is in flight.

## Response shape

- Content matches come back as `path:line:text` (context lines, when requested, in ripgrep's
  surrounding-line style).
- File search returns one path per line.
- **Paging:** large result sets are paged — the response reports the window and how to ask for the
  next page — so an agent pulls more on demand instead of receiving one giant dump.
- **Freshness inline, only when actionable:** if a returned line no longer matches what's on disk,
  it's flagged so the agent re-reads that file rather than trusting stale text.

## Why ripgrep-style text

Same shape as the CLI, so the two surfaces are interchangeable — the same query yields the same bytes
either way, with zero new parsing for the agent.

## Skill

`rgx --skill` installs a short agent skill that teaches a model to prefer these tools over raw
`rg`/`find`/`fd`, and how to read the freshness flags and paging footer.
