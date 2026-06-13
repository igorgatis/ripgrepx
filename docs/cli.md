# CLI

`rgx` is a near-drop-in for `rg`. The guiding rule: **a bare `rgx <pattern>` is always a plain
ripgrep search**, taking the same command line as `rg` — every habit and script keeps working, just
faster.

## Search (the drop-in)

```sh
rgx <pattern> [rg flags...]
```

- The positional is the pattern; the supported ripgrep flags (see [the flag surface](#the-flag-surface))
  are accepted and behave identically.
- The index picks candidate files, ripgrep confirms — output is byte-for-byte `rg`'s: paths relative
  to the search argument, line numbers per `rg` (on for a TTY, off when piped; `-n`/`-N` to force), and
  a single named file prints with no path prefix.
- Patterns the index can't accelerate fall back to a normal scan transparently.

```sh
rgx 'fn \w+_total' src/
rgx -i needle
rgx -v TODO                       # lines that do NOT match (-v / --invert-match)
rgx --hidden --no-ignore secret   # also search hidden + ignored files
rgx --sortr=modified TODO src/    # newest-changed files first (like rg --sortr)
```

`-v`, `--hidden`, and `--no-ignore` ask for results the trigram index can't serve — non-matching
lines, or files the index never indexed — so they transparently fall back to an in-process scan (the
same engine, just no index narrowing). Output stays byte-for-byte `rg`'s.

### Ordering — `--sort` / `--sortr`

`--sort=KEY` (ascending) and `--sortr=KEY` (descending) reorder results, matching ripgrep's flags and
vocabulary: `KEY` is `path`, `modified`, `accessed`, or `created` (file metadata), plus rgx's own
`weight` (relevance — see below). Reordering needs the whole result set, so like `rg --sort` it
buffers (single command, still no `rg` binary); without `--sort`, output streams as before. rgx orders
files exactly as `rg --sort` does. Works on the bare search and in `--compact`/MCP.

**Weighted match** (`--sort=weight`) is a model-supplied relevance order: declare branch weights with
`--weights=label:weight,...` and tag regex alternation branches in the pattern with `<label>`. The
tags are stripped before searching, so the match set stays `rg`'s; each file is ranked by its
highest-weighted matched branch (highest first), unattributed matches last. Weights are relative ranks
— any finite numbers, larger first; they needn't sum to 1 and aren't probabilities. Not combinable with
`-F`.

```sh
rgx --sort=weight --weights=impl:0.7,call:0.3 'fn (process<impl>|process\(<call>)'
```

## The flag surface

rgx adds exactly **four** modes, recognized only as the **leading token** (any other position goes
straight to ripgrep). Two are search modes (`--compact`, `--find`); `--server` and `--agent` gate the
daemon and AI-agent surfaces — see [`design.md`](design.md) for the rationale. The ripgrep flags rgx
passes through (anywhere, like rg): `-i -s -w -n -N -F -U -v -o -e/--regexp -A<n> -B<n> -C<n> -g/--glob
-t/--type -T/--type-not --hidden --no-ignore`, plus ripgrep's `--sort`/`--sortr` and rgx's own
`--weights` (for `--sort=weight`; see [Ordering](#ordering--sort--sortr)).

`-e/--regexp PATTERN` is repeatable; multiple patterns match a line if any of them does (`rgx -e foo
-e bar`). As in ripgrep, once any `-e` is given the positionals are all paths, so `-e` is also how you
search for a pattern that begins with `-` (`rgx -e --server`).

### File filters — `-g` / `-t` / `-T`

`-g/--glob GLOB` (a leading `!` negates), `-t/--type TYPE`, and `-T/--type-not TYPE` narrow *which*
files are searched, exactly as ripgrep does (they're ripgrep's own glob/type matchers, so the file set
matches `rg`'s). All are repeatable. Unlike `--hidden`/`--no-ignore`, filters only *remove* files, so
the trigram index stays in play — the candidate set is filtered down, keeping the speedup.

```sh
rgx -t rust 'fn .*Handler'        # only Rust files
rgx -g '*.ts' -g '!*.d.ts' useAuth   # .ts but not .d.ts
rgx -T lock 'version'             # everything except lockfiles
```

### Search modes

| Command | Purpose |
| --- | --- |
| `rgx <pattern> [rg flags...]` | Content search via ripgrep, accelerated. |
| `rgx --compact [opts] <pattern> [rg flags...]` | Same search, token-savings view: grouped by file, paged. |
| `rgx --find <name\|path> [path] [--after PATH]` | Locate files/directories by name or path (find/fd-style). |
| `rgx --version` (`-V`) | Print the rgx version. |

### `--compact` — the token-savings view

`--compact` runs the same accelerated search but reshapes the output for agents (and anyone who wants
a denser view):

- **Grouped by file** — the path is printed once, then `  line: text` for each match under it.
- **Header reports the totals** — `[matches 1-50 of 421 in 88 files]`, so you always know how much you
  have *not* seen.
- **Paged by an opaque cursor** — a page of matches at a time; when more remain the footer prints the
  exact next command (`next: rgx --compact --cursor '<token>'`). The cursor records the entire query
  (pattern + every flag) plus a keyset resume position, so the next page can't drift to a different
  search and a result set that changed between pages is reported with a `note:` line. The token itself
  is a short id: the daemon parks the cursor for ~2 minutes and hands you the id in its place, so it's
  tiny. Follow it from the same directory; it's single-use, and if it expires (or the daemon was
  stopped) you get `pagination expired — re-run the search`. Set the page size with `--page-size N`
  (default 50).
- **Orientation modes** — `-l` / `--files-with-matches` lists matching paths only; `-c` / `--count`
  lists `path:count` per file. Both answer "where / how many" in one call instead of a page-walk, and
  both page the same way (by file).
- **Ordering** — `--sort`/`--sortr` reorder the view (by `path`/`modified`/`accessed`/`created`, or
  `weight` with `--weights`) exactly as on the bare search above; the order is carried in the cursor,
  so pages stay stable. See [Ordering](#ordering--sort--sortr) and
  [`querying.md`](querying.md#ordering--sort--sortr).
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
| `rgx --server restart` | Stop it (if running) and start a fresh daemon. Useful after upgrading rgx so the new binary serves. |
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
| `rgx --agent install [TARGET...]` | Install the rgx bundle for each named agent (or auto-detect). `--user`/`--project` pick scope. |
| `rgx --agent uninstall [TARGET...]` | Remove exactly what `install` wrote. |
| `rgx --agent list` | Show detected agents and install status. |
| `rgx --agent --help` | The agent subcommands and the install model. |

`TARGET` is one of `claude`, `codex`, `cursor`, `gemini`, `vscode`. Both `install` and `uninstall`
**print the exact changes first and ask before touching anything**; `--yes` (`-y`) applies without
prompting (required when stdin is not a TTY, e.g. in a script), and `--dry-run` (`-n`) only previews.
`install` writes only where rgx owns the namespace (Claude skill dir, Gemini extension) and edits
shared files idempotently — a removable marked block, or a merged `"rgx"` key — never a blind append;
`uninstall` reverses it. MCP registration via a host's own CLI (`claude`/`codex mcp add`) is printed
for you to run, not executed. See [`mcp.md`](mcp.md) for the per-agent bundle table.

### Searching for a literal that looks like a flag

Use ripgrep's own escapes:

```sh
rgx -e --server        # search for the literal "--server"
rgx -- --server        # everything after -- is positional
```

## Notes

- Settings live in an optional user TOML you edit directly; there is no config-editing CLI. It is
  read from `$RGX_CONFIG`, else `$XDG_CONFIG_HOME/rgx/config.toml`, else `~/.config/rgx/config.toml`.
  A missing file is fine; a malformed or invalid one is a hard error. Config is read once at process
  start, so after editing, stop the daemon (`rgx --server stop`); the next search respawns it with
  the new config. Keys:
  - `cache_dir` — base directory for the rebuildable cache (index + socket). Must be an absolute
    path. `$RGX_CACHE_DIR` overrides it.
  - `persist_threshold_ms` (default `1000`) — persist the index only if the cold build took at least
    this long; below it the index stays RAM-only and is rebuilt on each daemon start. `0` always
    persists. See [`indexing.md`](indexing.md#storage).
  - `idle_timeout_secs` (default `3600`) — exit the daemon after this long with no search, freeing
    its RAM; the next search respawns it. Zero or negative stays resident forever.
- The background indexer **starts on first use**, so you rarely need `--server start`/`stop`
  directly; they exist for explicit control (CI, scripted warm-up, teardown). It also exits on its
  own after `idle_timeout_secs` of no searches.
- `--find` reports the true total (`[files 1-1000 of N]`) and never silently truncates: when more
  match than the page holds, the footer prints a `next: … --after '<path>'` command (keyset paging).
  Richer file-search options are an open design point — see [`design.md`](design.md#open-questions).
