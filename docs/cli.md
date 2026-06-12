# CLI

`rgx` is a near-drop-in for `rg`. The guiding rule: **a bare `rgx <pattern>` is always a plain
ripgrep search**, taking the same command line as `rg` — every habit and script keeps working, just
faster.

## Search (the drop-in)

```sh
rgx <pattern> [rg flags...]
```

- The positional is the pattern; all ripgrep flags are accepted and behave identically.
- The index picks candidate files, ripgrep confirms — output is byte-for-byte `rg`'s
  `path:line:text`.
- Patterns the index can't accelerate fall back to a normal scan transparently.

```sh
rgx TODO -t rust
rgx 'fn \w+_total' src/
rgx -i needle
```

## The flag surface

rgx adds exactly **four** flags to ripgrep's, recognized only as the **leading token** (any other
position goes straight to ripgrep). Two are search modes (`--compact`, `--find`); `--server` and
`--agent` gate the daemon and AI-agent surfaces — see [`design.md`](design.md) for the rationale.

### Search modes

| Command | Purpose |
| --- | --- |
| `rgx <pattern> [rg flags...]` | Content search via ripgrep, accelerated. |
| `rgx --compact [opts] <pattern> [rg flags...]` | Same search, token-savings view: grouped by file, paged. |
| `rgx --find <name\|path> [path] [--after PATH]` | Locate files/directories by name or path (find/fd-style). |

### `--compact` — the token-savings view

`--compact` runs the same accelerated search but reshapes the output for agents (and anyone who wants
a denser view):

- **Grouped by file** — the path is printed once, then `  line: text` for each match under it.
- **Header reports the totals** — `[matches 1-50 of 421 in 88 files]`, so you always know how much you
  have *not* seen.
- **Paged by an opaque cursor** — a page of matches at a time; when more remain the footer prints the
  exact next command (`next: rgx --compact --cursor '<token>'`). The cursor carries the entire query
  (pattern + every flag) plus a keyset resume position, so the next page can't drift to a different
  search and a result set that changed between pages is reported with a `note:` line. Set the page
  size with `--page-size N` (default 50).
- **Orientation modes** — `-l` / `--files-with-matches` lists matching paths only; `-c` / `--count`
  lists `path:count` per file. Both answer "where / how many" in one call instead of a page-walk, and
  both page the same way (by file).
- **Long lines trimmed** — lines longer than the column budget are truncated around the match, marked
  with `…`; read the file for the full line.

This is the one rgx surface whose output is **not** byte-for-byte `rg`. The match set is still exactly
ripgrep's — nothing is added or silently dropped; pagination is the only volume control, so every
match is reachable. All the usual search flags (`-i`, `-w`, `-F`, `-C`, …) still apply (and a cursor
preserves them across pages).

### `--server` — manage the index server

| Command | Purpose |
| --- | --- |
| `rgx --server` | Run the index server in the foreground. |
| `rgx --server start` | Start the background indexer for this project. |
| `rgx --server stop` | Stop the background indexer for this project. |
| `rgx --server status` | One-shot snapshot: index state, file/trigram counts, memory, snapshot size and last-sync age. |
| `rgx --server watch` | Live status: repaints on every change (cold-build progress count, then each reconcile) until interrupted. |
| `rgx --server --help` | The server subcommands in full (also `rgx --help --server`). |

`--server` subcommands act on the **current directory's** project (run them from, or `cd` into, the
repo). `watch` is the interactive companion to `status` — e.g. run `rgx --server watch` in another
pane to see a cold index build climb `building N / M files` to `ready`, with no measurable cost to
the indexing itself.

### `--agent` — integrate with AI coding agents

| Command | Purpose |
| --- | --- |
| `rgx --agent mcp` | Serve search to AI agents over MCP (stdio): `content_search`, `file_search`, `status`. |
| `rgx --agent skill` | Print the agent skill markdown (teaches a model to prefer `rgx` over rg/grep/find/fd). |
| `rgx --agent install` | Write the skill to `~/.claude/skills/rgx/SKILL.md` (or `$RGX_SKILL_DIR`) and print MCP setup. |
| `rgx --agent --help` | The agent subcommands plus MCP setup for Claude Code, Codex, and other clients. |

Works with **Claude Code**, **Codex**, and any MCP client. Register `rgx --agent mcp` as a stdio
server: `claude mcp add rgx -- rgx --agent mcp` for Claude Code, a `[mcp_servers.rgx]` block in
`~/.codex/config.toml` for Codex, or the equivalent `"rgx": { "command": "rgx", "args": ["--agent",
"mcp"] }` entry in any other client's config. The skill is plain markdown: Claude Code loads it from
`~/.claude/skills/`, while for Codex or others you paste it into `AGENTS.md` or the agent's
instructions. See [`mcp.md`](mcp.md).

### Searching for a literal that looks like a flag

Use ripgrep's own escapes:

```sh
rgx -e --server        # search for the literal "--server"
rgx -- --server        # everything after -- is positional
```

## Notes

- Settings live in a project `.toml` you edit directly; there is no config-editing CLI. The indexer
  reloads its config without a restart.
- The background indexer **starts on first use**, so you rarely need `--server start`/`stop`
  directly; they exist for explicit control (CI, scripted warm-up, teardown).
- `--find` reports the true total (`[files 1-1000 of N]`) and never silently truncates: when more
  match than the page holds, the footer prints a `next: … --after '<path>'` command (keyset paging).
  Richer file-search options are an open design point — see [`design.md`](design.md#open-questions).
