---
name: rgx
description: >-
  Fast code/content search for this repository. Use rgx instead of rg, grep, find, or fd when
  searching files in a project you query repeatedly: it keeps a fresh index so searches jump straight
  to candidate files, while ripgrep still does the matching — so results, regex semantics, flags, and
  output are byte-for-byte identical to rg, just faster. TRIGGER whenever you would reach for
  rg/grep/find/fd to search code or locate files.
---

# rgx — instant ripgrep for this repo

`rgx` is a drop-in for `rg` backed by a persistent candidate index. Prefer it over `rg`, `grep`,
`find`, and `fd` for searching this codebase.

## Content search (regex, ripgrep semantics)

```sh
rgx <pattern> [path]        # e.g. rgx 'fn \w+_total' src/
rgx -i needle               # case-insensitive (-s forces case-sensitive)
rgx -w word                 # whole-word
rgx -F 'literal.string'     # fixed string (no regex)
rgx -U 'a(?s:.)*b'          # multiline match
rgx -C 3 pattern            # 3 lines of context (also -A <n> / -B <n>)
```

- Output is exactly `rg`'s `path:line:text`. Anything you'd write as `rg <pattern>` works as
  `rgx <pattern>`. Supported flags: `-i` `-s` `-w` `-n` `-F` `-U` `-A<n>` `-B<n>` `-C<n>` `--`.
  Line numbers are always on, so `-n` is accepted as a no-op for drop-in parity.
- Patterns the index can't accelerate (e.g. `.`, `\w+`, very short patterns) transparently fall back
  to a full scan — still correct, just not faster.
- To search for text that looks like a flag, use ripgrep's escapes: `rgx -e --foo` or
  `rgx -- --foo` (everything after `--` is the pattern/path).

**Flag ordering (important):** rgx's own modes — `--compact`, `--find`, `--server`, `--agent` — are
recognized **only as the first token**. Put them right after `rgx`: `rgx --compact 'fn ' src/` works,
but `rgx 'fn ' --compact` is treated as a plain search and errors. All the search flags above can
follow in any position, like `rg`.

## Token-savings view (prefer this for broad searches)

```sh
rgx --compact <pattern> [path]            # grouped by file, paged, long lines trimmed
rgx --compact --page-size 20 <pattern>    # set the page size (default 50)
rgx --compact --cursor '<token>'          # next page (copy the token from the footer)
rgx --compact -l <pattern>                # matching file paths only (where?)
rgx --compact -c <pattern>                # per-file match counts (how many?)
```

- Use `--compact` when a search may return many matches. Output is grouped by file (path printed
  once) and **paged**, which is far cheaper on tokens than a raw dump.
- The header is `[matches 1-50 of 421 in 88 files]`, so you always know how much you have **not** seen
  — don't treat the first page as the whole answer. When more remain, the footer prints the exact next
  command: `next: rgx --compact --cursor '<token>'`. The cursor carries the whole query (pattern +
  every flag), so the next page is the same search; if the result set changed between pages, rgx prints
  a `note:` line.
- **Orient before paging:** for "which files" use `-l`; for "how many per file" use `-c`. One call,
  no page-walk.
- **Paging is cheap** — the index is warm. Narrow the pattern or `path` when you can, but when results
  are legitimately large, pull the next page (via the cursor) instead of widening into one giant
  search. Nothing is dropped: every match is reachable. Only very long lines are trimmed around the
  match (`…`) — open the file if you need the full line.

## File / directory lookup (find/fd-style)

```sh
rgx --find <name-or-path-substring> [path]   # e.g. rgx --find kubelet.go
```

Prints a `[files 1-1000 of N]` header then one matching path per line. It never silently truncates:
when more match than the page holds, a `next: rgx --find … --after '<path>'` footer fetches the rest.

## Index health

```sh
rgx --server status     # whether the index is ready, file/trigram counts, last sync, size
```

The background indexer starts on first use and keeps itself fresh as files change; you do not need
to start or manage it manually. It exits after an idle period (default 1 h) to free its RAM and
respawns on the next search; a small repo that is cheap to rebuild is kept in RAM only (no on-disk
snapshot), which `status` shows as `ram-only`. Both are tunable — see **Config**.

## Config

`rgx` reads an optional TOML config; edit it directly (there is no config CLI). Location, in order:
`$RGX_CONFIG`, else `$XDG_CONFIG_HOME/rgx/config.toml`, else `~/.config/rgx/config.toml` (create it if
missing). A malformed or invalid file is a hard error. Config is read once at startup, so to apply an
edit, run `rgx --server stop`; the next search respawns the daemon with the new values.

```toml
# Base directory for the rebuildable cache (index + socket). Absolute path. $RGX_CACHE_DIR overrides.
cache_dir = "/var/tmp/rgx-cache"

# Persist the index to disk only if the cold build took at least this long (ms); below it the index
# stays RAM-only and is rebuilt on each daemon start. 0 always persists. Default 1000.
persist_threshold_ms = 1000

# Exit the daemon after this many seconds with no search (the next search respawns it).
# Zero or negative keeps it resident forever. Default 3600.
idle_timeout_secs = 3600
```

To adjust a knob for the user: set the key in that file, then `rgx --server stop` so the change takes
effect on the next search. Examples — keep the daemon resident forever: `idle_timeout_secs = -1`;
always persist the snapshot: `persist_threshold_ms = 0`.

## Over MCP

If `rgx` is wired as an MCP server (`rgx --agent mcp`), the same search is exposed as three tools:

- **`content_search`** — args: `pattern` (required for a new search; omit it when paging via
  `cursor`), plus optional `case_insensitive`, `word`, `fixed_strings`, `multi_line`, `page_size`,
  `files_only`/`count`, and `cursor`. It returns the
  **compact view by default** (this is `--compact` — there is no raw mode over MCP): a
  `[matches 1-50 of 421 in 88 files]` header, matches grouped by file (path once, then `  line:
  text`), and a `(more: call content_search with cursor: "<token>")` line when further pages exist.
  Pass that `cursor` back to advance — it carries the exact query, so the page can't drift. Use
  `files_only`/`count` to orient cheaply instead of a page-walk. Don't assume page 1 is everything —
  the header's total tells you what remains.
- **`file_search`** — args: `query` (name/path substring), optional `limit` and `after`. Returns a
  `[files X-Y of N]` header then one path per line; when more remain it tells you the `after` key to
  fetch the next page.
- **`status`** — no args. Reports index readiness and file/trigram counts.

## Notes for agents

- A result is matched against the file on disk at query time, so a returned line reflects current
  content. If you've just edited a file, a follow-up search sees the change within a moment.
- For large result sets, use `--compact` (CLI) or `content_search` paging (MCP): narrow the pattern
  or `path` when you can, but pulling the next page is cheap — prefer it over a broad raw dump.
- The index lives outside the repo (default `~/.cache/rgx/<hash>/`, or `$RGX_CACHE_DIR/<hash>/` if
  set); it is a rebuildable cache, safe to delete, and never written into the indexed tree.
