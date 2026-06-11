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
rgx -i needle               # case-insensitive
rgx -w word                 # whole-word
rgx -F 'literal.string'     # fixed string (no regex)
rgx -C 3 pattern            # 3 lines of context (also -A / -B)
```

- Output is exactly `rg`'s `path:line:text`. Anything you'd write as `rg <pattern>` works as
  `rgx <pattern>`.
- Patterns the index can't accelerate (e.g. `.`, `\w+`, very short patterns) transparently fall back
  to a full scan — still correct, just not faster.

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

## Notes for agents

- A result is matched against the file on disk at query time, so a returned line reflects current
  content. If you've just edited a file, a follow-up search sees the change within a moment.
- Results may be paged for large result sets; narrow the pattern or scope by `path` rather than
  pulling everything.
