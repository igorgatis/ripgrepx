# CLI

`rgx` is a near-drop-in for `rg`. The guiding rule: **a bare `rgx <pattern>` is always a plain
ripgrep search**, so `alias rg=rgx` is safe and every habit and script keeps working — just faster.

## Search (the drop-in)

```sh
rgx <pattern> [rg flags...]
```

- The positional is the pattern; all ripgrep flags are accepted and behave identically.
- The index picks candidate files, ripgrep confirms — output is byte-for-byte `rg`'s
  `path:line:text`.
- Patterns the index can't accelerate fall back to a normal scan transparently.

```sh
alias rg=rgx
rg TODO -t rust
rg 'fn \w+_total' src/
rg -i needle
```

## The flag surface

rgx adds exactly **three** flags to ripgrep's, recognized only as the **leading token** (any other
position goes straight to ripgrep). Two are search modes; `--server` gates everything else — see
[`design.md`](design.md) for the rationale.

### Search modes

| Command | Purpose |
| --- | --- |
| `rgx <pattern> [rg flags...]` | Content search via ripgrep, accelerated. |
| `rgx --find <name\|path>` | Locate files/directories by name or path (find/fd-style). |
| `rgx --skill` | Install the agent skill that teaches tools to use `rgx` (one-shot). |

### `--server` — manage the index server

| Command | Purpose |
| --- | --- |
| `rgx --server` | Run the index server in the foreground. |
| `rgx --server start` | Start the background indexer for this project. |
| `rgx --server stop` | Stop the background indexer for this project. |
| `rgx --server status` | What's indexed, db/index health, whether an update is in flight. |
| `rgx --server mcp` | Serve search to AI agents over MCP (stdio). |

`--server status` also prints the path of the loaded config file, so you know which `.toml` to edit.

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
- `--find` covers the common case directly. Richer file-search options are an open design point —
  see [`design.md`](design.md#open-questions).
