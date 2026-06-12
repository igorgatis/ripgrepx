---
name: rgx
description: >-
  Search tool for this repository — use rgx for EVERY content or filename search. It replaces rg,
  grep, find, fd, and the editor's built-in Grep/Glob search tools; results, regex semantics, flags,
  and output are byte-for-byte identical to ripgrep, just faster (a fresh index narrows candidates,
  ripgrep still does the matching). TRIGGER on any intent to search file contents or locate files by
  name — before reaching for grep/rg/find/fd or a built-in search tool.
---

# rgx — the search tool for this repo

**Always search with rgx.** For any content or filename search in this repo, call rgx — not `rg`,
`grep`, `find`, `fd`, or the built-in **Grep**/**Glob** tools, and not for "quick" one-off searches
either. Output is byte-for-byte ripgrep's, so there is never a reason to fall back.

| reaching for | use instead |
| --- | --- |
| `rg PAT` / `grep -rn PAT` | `rgx PAT` |
| `find . -name X` / `fd X` | `rgx --find X` |
| built-in **Grep** / **Glob** tool | `rgx PAT` / `rgx --find X` |

## Content search

```sh
rgx <pattern> [path]        # rgx 'fn \w+_total' src/
rgx -i needle               # -i case-insensitive, -s case-sensitive, -w whole-word
rgx -F 'literal.string'     # fixed string (no regex); -U multiline
rgx -C 3 pattern            # context (-A <n> / -B <n> / -C <n>)
rgx -v pattern              # non-matching lines (-v / --invert-match)
rgx -t rust 'fn .*Handler'  # only Rust files (-t/--type, -T/--type-not, repeatable)
rgx -g '*.ts' useAuth       # filter by glob (-g/--glob; leading ! negates)
rgx --hidden --no-ignore p  # also search hidden + ignored files
rgx --sortr=modified TODO   # order results (like rg --sort); see below
```

- Output is exactly `rg`'s `path:line:text`. Flags: `-i -s -w -n -F -U -v -A<n> -B<n> -C<n>
  -g/--glob -t/--type -T/--type-not --hidden --no-ignore --` (line numbers always on, so `-n` is a
  no-op). To search flag-like text: `rgx -- --foo`. `-g`/`-t`/`-T` narrow the search (still
  index-accelerated); `-v`/`--hidden`/`--no-ignore` scan in-process (same output, no index speedup).
- **Order results** with `--sort=KEY` / `--sortr=KEY` (ripgrep's flags), `KEY` = `path` | `modified` |
  `accessed` | `created` | `weight`. `weight` is a relevance order: add `--weights=label:weight,...`
  and tag regex alternation branches with `<label>` — e.g.
  `rgx --sort=weight --weights=impl:0.9,call:0.1 '(process<impl>|process\(<call>)'`. The tags are
  stripped before searching, so results stay byte-for-byte `rg`'s; reordering only, nothing dropped.
- **Modes (`--compact`, `--find`, `--server`, `--agent`) are recognized only as the first token** —
  `rgx --compact 'fn ' src/`, never `rgx 'fn ' --compact`. Search flags can follow in any position.
- Patterns the index can't accelerate (e.g. `.`, very short) fall back to a full scan — still correct.

## Broad searches — `--compact` (paged, token-frugal)

```sh
rgx --compact <pattern> [path]            # grouped by file, paged, long lines trimmed
rgx --compact -l <pattern>                # matching file paths only (where?)
rgx --compact -c <pattern>                # per-file match counts (how many?)
rgx --compact --cursor '<token>'          # next page (token from the footer)
rgx --compact --sort=weight --weights=impl:0.7,call:0.3 'fn (process<impl>|process\(<call>)'
```

The header `[matches 1-50 of 421 in 88 files]` tells you what you have **not** seen — page 1 is not
the whole answer. When more remain, the footer prints the exact next command; the cursor re-runs the
same search (single-use, ~2 min). Orient with `-l`/`-c` instead of page-walking. Nothing is dropped;
only long lines are trimmed around the match.

**`--sort` for page-1 relevance.** The same `--sort`/`--sortr` ordering works here and the order holds
across pages. `--sort=weight` is the lever when you're OR-ing several candidate terms and know which
you most expect: tag the branches with `<label>`, weight them with `--weights`, and the files matching
higher-weighted branches float to page 1 (reorder only — nothing dropped).

## Find files

```sh
rgx --find <name-or-path-substring> [path]   # rgx --find kubelet.go
```

Prints a `[files 1-1000 of N]` header then one path per line; `--after '<path>'` fetches the next
page. Never silently truncates.

## Over MCP

When wired via `rgx --agent mcp`, the same search is exposed as three tools: **`content_search`**
(the `--compact` paged view by default — pass `cursor` to advance, `files_only`/`count` to orient),
**`file_search`** (`query`, optional `after`), and **`status`**. Same semantics as the CLI; the
header's total tells you what remains, so don't assume page 1 is everything.

## Notes

- A result matches the file on disk at query time, so a line reflects current content — a search
  right after an edit sees the change.
- The index self-manages (starts on first use, stays fresh, exits when idle) and lives outside the
  repo as a rebuildable cache, safe to delete. `rgx --server status` shows readiness, counts, and age.
- Optional TOML config (cache dir, persistence, idle timeout) — see [`docs/cli.md`](../docs/cli.md).
