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
  `rgx <pattern>`. Supported flags: `-i` `-s` `-w` `-F` `-U` `-A<n>` `-B<n>` `-C<n>` `--`.
- Patterns the index can't accelerate (e.g. `.`, `\w+`, very short patterns) transparently fall back
  to a full scan — still correct, just not faster.
- To search for text that looks like a flag, use ripgrep's escapes: `rgx -e --foo` or
  `rgx -- --foo` (everything after `--` is the pattern/path).

**Flag ordering (important):** rgx's own modes — `--compact`, `--find`, `--server`, `--skill` (and
`--page`) — are recognized **only as the first token**. Put them right after `rgx`: `rgx --compact
'fn ' src/` works, but `rgx 'fn ' --compact` is treated as a plain search and errors. All the search
flags above can follow in any position, like `rg`.

## Token-savings view (prefer this for broad searches)

```sh
rgx --compact <pattern> [path]       # grouped by file, paged, long lines trimmed
rgx --compact --page 2 <pattern>     # next page (also -p 2)
```

- Use `--compact` when a search may return many matches. Output is grouped by file (path printed
  once) and **paged**, which is far cheaper on tokens than a raw dump.
- **Paging is cheap** — the index is warm, so re-running for the next page costs almost nothing.
  Narrow the pattern or `path` when you can, but when results are legitimately large, **pull the next
  page** (the footer prints the exact command) instead of widening into one giant search.
- Nothing is dropped: the match set is identical to `rg`; every match is reachable by paging. Only
  very long lines are trimmed around the match (`…`) — open the file if you need the full line.
- The header is `[page X/Y · N matches in M files]`; when more remain, a footer prints the exact
  next command (`next: rgx --compact --page 2 '<pattern>' <path>`).

## File / directory lookup (find/fd-style)

```sh
rgx --find <name-or-path-substring> [path]   # e.g. rgx --find kubelet.go
```

Returns one matching path per line.

## Index health

```sh
rgx --server status     # whether the index is ready, file/trigram counts, last sync, size
```

The background indexer starts on first use and keeps itself fresh as files change; you do not need
to start or manage it manually.

## Over MCP

If `rgx` is wired as an MCP server (`rgx --server mcp`), the same search is exposed as three tools:

- **`content_search`** — args: `pattern` (required), plus optional `case_insensitive`, `word`,
  `fixed_strings`, `multi_line`, and `page` (1-based). It returns the **compact view by default**
  (this is `--compact` — there is no raw mode over MCP): a `[page X/Y · N matches in M files]`
  header, matches grouped by file (path once, then `  line: text`), and a `(more: call
  content_search with page: 2)` line when further pages exist. Pass `page` to advance — paging is
  cheap, so prefer it over a broad dump.
- **`file_search`** — arg: `query` (name/path substring). Returns one path per line.
- **`status`** — no args. Reports index readiness and file/trigram counts.

## Notes for agents

- A result is matched against the file on disk at query time, so a returned line reflects current
  content. If you've just edited a file, a follow-up search sees the change within a moment.
- For large result sets, use `--compact` (CLI) or `content_search` paging (MCP): narrow the pattern
  or `path` when you can, but pulling the next page is cheap — prefer it over a broad raw dump.
- The index lives outside the repo (default `~/.cache/rgx/<hash>/`, or `$RGX_CACHE_DIR/<hash>/` if
  set); it is a rebuildable cache, safe to delete, and never written into the indexed tree.
