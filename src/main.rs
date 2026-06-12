//! rgx CLI. A bare `rgx <pattern>` is an (accelerated) ripgrep content search; the `--server` gate
//! holds daemon management, `--agent` the AI surface (MCP/skill), and `--find` does fd/find-style
//! name lookup. See `docs/cli.md`.
//!
//! Flags are recognized only as the leading token (rgx adds as few as possible to rg's surface).
//! The rg flag passthrough is a deliberate subset for now (-i, -s, -w, -F, -U, -A/-B/-C, `--`).

use std::io::Write;
use std::process::ExitCode;

use rgx::compact::{self, CompactOpts};
use rgx::confirm::SearchOptions;
use rgx::cursor::{self, Mode};
use rgx::paths::resolve_root;
use rgx::proto::Request;
use rgx::{client, mcp, server};

// Heap profiling (cargo run --release --features dhat-heap ...): captures allocations for the whole
// run (e.g. run the daemon foreground over a repo to profile a cold build), writes dhat-heap.json.
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() -> ExitCode {
    #[cfg(feature = "dhat-heap")]
    let _dhat = dhat::Profiler::new_heap();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {
            usage();
            ExitCode::from(2)
        }
        Some("--server") => server_cmd(&args[1..]),
        Some("--agent") => agent_cmd(&args[1..]),
        Some("--compact") => compact_cmd(&args[1..]),
        Some("--find") => find_cmd(&args[1..]),
        Some("-h" | "--help") => {
            // `rgx --help --server` / `--help --agent`: defer to the sub-surface guide.
            match args.get(1).map(String::as_str) {
                Some("--server") => print!("{SERVER_HELP}"),
                Some("--agent") => print!("{AGENT_HELP}"),
                _ => print!("{HELP}"),
            }
            ExitCode::SUCCESS
        }
        _ => content_cmd(&args),
    }
}

/// Brief usage, to stderr, for the error paths (no/invalid args). Points at `--help` for the rest.
fn usage() {
    eprintln!(
        "usage:\n  rgx [flags] <pattern> [path]            content search (accelerated ripgrep)\n  \
         rgx --compact [opts] <pattern> [path]   token-savings view: grouped + paged\n  \
         rgx --find <name|path> [path] [--after PATH]   find files/dirs by name\n  \
         rgx --server [start|stop|status|watch]\n  \
         rgx --agent [mcp|skill|install]\n\n\
         flags: -i -s -w -n -F -U -A<n> -B<n> -C<n> --\n\
         run `rgx --help` for the full guide (drop-in use, server, agent: MCP/skill)"
    );
}

/// The full guide, to stdout, for `-h`/`--help`. Lean: an agent reads it once and knows the whole
/// surface — drop-in ripgrep use, the index server, and the AI path (compaction, MCP, skill).
/// `--server`/`--agent` have their own deeper guides (`rgx --server --help`, `rgx --agent --help`).
const HELP: &str = "\
rgx — Instant ripgrep for codebases you search over and over.

  rgx [flags] <pattern> [path]           content search (accelerated ripgrep)
  rgx --compact [opts] <pattern> [path]  token-savings view: grouped + paged
  rgx --find <name|path> [path]          locate files/dirs by name (find/fd-style)
  rgx --server [start|stop|status|watch]   background index server      (rgx --server --help)
  rgx --agent [mcp|skill|install]          AI-agent integration         (rgx --agent --help)

DROP-IN FOR ripgrep — `rgx <pattern>` takes the same command line as `rg`, same output. Flags
(anywhere, like rg): -i -s -w -n -F -U -A<n> -B<n> -C<n> --. rgx's own modes are recognized only as the
first token. Examples:
    rgx 'fn \\w+_total' src/        rgx -i needle        rgx -- --server   (literal flag)

SERVER — the indexer starts on first use and stays fresh on its own; subcommands act on the cwd's
project. `status` reports readiness/counts/age, `watch` repaints live. Index + socket live under
$RGX_CACHE_DIR (else config `cache_dir`, else ~/.cache/rgx): a rebuildable cache, safe to delete.

FOR AI AGENTS — works with Claude Code, Codex, and any MCP client; see `rgx --agent --help`.
  Compaction — `--compact` groups matches by file, pages behind an opaque cursor, trims long lines.
  Nothing is dropped; the header reports the full total. `--page-size N` (default 50), `--cursor TOK`
  next page, `-l` files only, `-c` per-file counts. The cursor carries the whole query, so paging
  can't drift; a result set that changed gets a `note:`.
  MCP — `rgx --agent mcp` (stdio) exposes content_search (compact paged view), file_search, status.
  Skill — `rgx --agent skill` prints it; `rgx --agent install` installs it + prints MCP setup.

Docs: https://github.com/igorgatis/ripgrepx
";

/// `rgx --server --help` (or `--help --server`): the index-server subcommands in full.
const SERVER_HELP: &str = "\
rgx --server — the background index server. Subcommands act on the current directory's project; the
indexer also starts on first search, so you rarely manage it by hand.

  rgx --server          run the indexer in the foreground
  rgx --server start    start the background indexer for this project
  rgx --server stop     stop it
  rgx --server status   one-shot: readiness, file/trigram counts, memory, last-sync age
  rgx --server watch    live status, repaints on every change until interrupted

Index + socket live under $RGX_CACHE_DIR (else config `cache_dir`, else $XDG_CACHE_HOME/rgx, else
~/.cache/rgx): a rebuildable cache, safe to delete, never written into the indexed tree.
";

/// `rgx --agent --help` (or `--help --agent`): the AI-agent surface, with setup for the common hosts.
const AGENT_HELP: &str = "\
rgx --agent — integrate rgx with AI coding agents (Claude Code, Codex, or any MCP client).

  rgx --agent mcp       run the stdio MCP server: content_search, file_search, status
  rgx --agent skill     print the agent skill (teaches a model to prefer rgx over rg/grep/find/fd)
  rgx --agent install   install the skill into ~/.claude/skills (or $RGX_SKILL_DIR) + print MCP setup

MCP setup — register `rgx --agent mcp` as a stdio server:
  Claude Code   claude mcp add rgx -- rgx --agent mcp
  Codex         add to ~/.codex/config.toml:
                  [mcp_servers.rgx]
                  command = \"rgx\"
                  args = [\"--agent\", \"mcp\"]
  Other clients add to the client's MCP config:
                  \"rgx\": { \"command\": \"rgx\", \"args\": [\"--agent\", \"mcp\"] }

Skill — `rgx --agent skill` is plain markdown. Claude Code loads it from ~/.claude/skills/rgx/SKILL.md
(what `install` writes); for Codex or others, paste it into AGENTS.md or your agent's instructions.
";

fn server_cmd(rest: &[String]) -> ExitCode {
    if matches!(
        rest.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    ) {
        print!("{SERVER_HELP}");
        return ExitCode::SUCCESS;
    }
    let root = resolve_root(None);
    match rest.first().map(String::as_str) {
        None => match server::run(root) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rgx --server: {e}");
                ExitCode::from(2)
            }
        },
        Some("start") => match client::spawn_daemon(&root) {
            Ok(()) => {
                println!("rgx: daemon starting for {}", root.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("rgx: {e}");
                ExitCode::from(2)
            }
        },
        Some("stop") => match client::request_existing(&root, &Request::Shutdown) {
            Ok(Some(_)) => {
                println!("rgx: daemon stopped");
                ExitCode::SUCCESS
            }
            Ok(None) => {
                println!("rgx: no daemon running");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("rgx: {e}");
                ExitCode::from(2)
            }
        },
        Some("status") => match client::request_existing(&root, &Request::Status) {
            Ok(Some(bytes)) => {
                let _ = std::io::stdout().write_all(&bytes);
                ExitCode::SUCCESS
            }
            // No daemon: load the on-disk snapshot (if any) to report stats and its location.
            Ok(None) => {
                let snapshot = rgx::paths::snapshot_path(&root);
                let idx = rgx::index::Index::load(&snapshot).ok();
                let block = rgx::status::Status {
                    root: &root,
                    snapshot: &snapshot,
                    running: false,
                    state: None,
                    files: idx.as_ref().map(rgx::index::Index::file_count),
                    trigrams: idx.as_ref().map(rgx::index::Index::trigram_count),
                    memory_bytes: idx.as_ref().map(rgx::index::Index::memory_bytes),
                }
                .render();
                print!("{block}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("rgx: {e}");
                ExitCode::from(2)
            }
        },
        Some("watch") => {
            // Live status: clear+home before each frame so it repaints in place.
            let res = client::watch(&root, |frame| {
                let mut out = std::io::stdout();
                let _ = out.write_all(b"\x1b[2J\x1b[H");
                let _ = out.write_all(frame);
                let _ = out.flush();
            });
            match res {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("rgx: {e}");
                    ExitCode::from(2)
                }
            }
        }
        Some(other) => {
            eprintln!("rgx --server: unknown subcommand {other:?}");
            ExitCode::from(2)
        }
    }
}

/// `rgx --agent <mcp|skill|install>`: the AI-agent surface. `mcp` runs the stdio MCP server; `skill`
/// prints the agent skill; `install` writes it under the skills dir and prints MCP setup. `--help`
/// prints the agent guide; a missing subcommand is an error.
fn agent_cmd(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("mcp") => match mcp::run(resolve_root(None)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rgx --agent mcp: {e}");
                ExitCode::from(2)
            }
        },
        Some("skill") => {
            rgx::skill::print_skill();
            ExitCode::SUCCESS
        }
        Some("install") => match rgx::skill::install() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rgx --agent install: {e}");
                ExitCode::from(2)
            }
        },
        Some("-h" | "--help" | "help") => {
            print!("{AGENT_HELP}");
            ExitCode::SUCCESS
        }
        None => {
            eprintln!(
                "rgx --agent: pick a subcommand (mcp|skill|install); see `rgx --agent --help`"
            );
            ExitCode::from(2)
        }
        Some(other) => {
            eprintln!("rgx --agent: unknown subcommand {other:?}");
            ExitCode::from(2)
        }
    }
}

const FIND_LIMIT: u32 = 1000;

fn find_cmd(rest: &[String]) -> ExitCode {
    let mut needle: Option<&str> = None;
    let mut path: Option<&str> = None;
    let mut after: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        let a = &rest[i];
        if a == "--after" || a.starts_with("--after=") {
            let Some((v, consumed)) = long_value(rest, i, "--after") else {
                eprintln!("rgx: --after needs a value");
                return ExitCode::from(2);
            };
            after = Some(v.to_string());
            i += consumed;
            continue;
        }
        if needle.is_none() {
            needle = Some(a);
        } else if path.is_none() {
            path = Some(a);
        } else {
            eprintln!("rgx: unexpected extra argument {a:?}");
            return ExitCode::from(2);
        }
        i += 1;
    }
    let Some(needle) = needle else {
        eprintln!("usage: rgx --find <name|path> [path] [--after PATH]");
        return ExitCode::from(2);
    };
    let root = resolve_root(path);
    let bytes = match client::request(
        &root,
        &Request::Find {
            needle: needle.to_string(),
            after,
            limit: FIND_LIMIT,
        },
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("rgx: {e}");
            return ExitCode::from(2);
        }
    };

    let (header, body) = rgx::proto::parse_find_header(&bytes);
    let mut out = std::io::stdout();
    let Some(h) = header else {
        // Headerless blob (older daemon): emit paths as-is.
        let _ = out.write_all(body);
        return if body.is_empty() {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    };
    let first = if h.returned == 0 { 0 } else { h.start + 1 };
    let _ = writeln!(
        out,
        "[files {first}-{} of {}]",
        h.start + h.returned,
        h.total
    );
    let _ = out.write_all(body);
    if let Some(next) = h.next_after {
        let scope = path
            .map(|p| format!(" {}", shell_quote(p)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "next: rgx --find {}{scope} --after {}",
            shell_quote(needle),
            shell_quote(&next)
        );
    }
    if h.total == 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// The leading-token flag surface shared by content search and `--compact`. `compact` additionally
/// recognizes `--cursor TOK` (resume, supersedes the pattern), `--page-size N`, and the `-l`/`-c`
/// output modes. Errors are reported here; the `Err` carries the exit code so callers just propagate.
struct ParsedSearch<'a> {
    opts: SearchOptions,
    cursor: Option<&'a str>,
    page_size: Option<usize>,
    mode: Mode,
    positionals: Vec<&'a str>,
}

fn parse_search<'a>(args: &'a [String], compact: bool) -> Result<ParsedSearch<'a>, ExitCode> {
    let mut opts = SearchOptions::default();
    let mut positionals: Vec<&str> = Vec::new();
    let mut cursor: Option<&str> = None;
    let mut page_size: Option<usize> = None;
    let mut mode = Mode::Matches;
    let mut only_positional = false; // set by `--`
    let mut i = 0;

    while i < args.len() {
        let a = &args[i];
        // A flag is recognized anywhere (like rg), until `--`. A leading-`-` token after `--`, or
        // the lone `-`, is positional. (A pattern that starts with `-` must follow `--`.)
        if only_positional || !a.starts_with('-') || a == "-" {
            positionals.push(a);
            i += 1;
            continue;
        }
        match a.as_str() {
            "--" => only_positional = true,
            "-i" | "--ignore-case" => opts.case_insensitive = true,
            "-s" | "--case-sensitive" => opts.case_insensitive = false,
            "-w" | "--word-regexp" => opts.word = true,
            "-F" | "--fixed-strings" => opts.fixed_strings = true,
            "-U" | "--multiline" => opts.multi_line = true,
            "-n" | "--line-number" => {}
            "-l" | "--files-with-matches" if compact => mode = Mode::Files,
            "-c" | "--count" if compact => mode = Mode::Count,
            p if compact && (p == "--cursor" || p.starts_with("--cursor=")) => {
                let Some((v, consumed)) = long_value(args, i, "--cursor") else {
                    eprintln!("rgx: --cursor needs a value");
                    return Err(ExitCode::from(2));
                };
                cursor = Some(v);
                i += consumed;
                continue;
            }
            p if compact && (p == "--page-size" || p.starts_with("--page-size=")) => {
                let n = long_value(args, i, "--page-size")
                    .and_then(|(v, c)| v.parse().ok().map(|n: usize| (n, c)));
                let Some((n, consumed)) = n else {
                    eprintln!("rgx: --page-size needs a number");
                    return Err(ExitCode::from(2));
                };
                page_size = Some(n.max(1));
                i += consumed;
                continue;
            }
            ctx if ctx.starts_with("-A") || ctx.starts_with("-B") || ctx.starts_with("-C") => {
                let (n, consumed) = match context_value(args, i) {
                    Some(v) => v,
                    None => {
                        eprintln!("rgx: {ctx} needs a number");
                        return Err(ExitCode::from(2));
                    }
                };
                match &ctx[..2] {
                    "-A" => opts.after_context = n,
                    "-B" => opts.before_context = n,
                    _ => {
                        opts.before_context = n;
                        opts.after_context = n;
                    }
                }
                i += consumed;
                continue;
            }
            other => {
                eprintln!("rgx: unsupported flag {other:?} (drop-in flag surface is WIP)");
                return Err(ExitCode::from(2));
            }
        }
        i += 1;
    }
    Ok(ParsedSearch {
        opts,
        cursor,
        page_size,
        mode,
        positionals,
    })
}

fn content_cmd(args: &[String]) -> ExitCode {
    let parsed = match parse_search(args, false) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let opts = parsed.opts;

    let Some((pattern, rest)) = parsed.positionals.split_first() else {
        usage();
        return ExitCode::from(2);
    };
    let pattern = pattern.to_string();
    let path = rest.first().copied();
    if rest.len() > 1 {
        eprintln!("rgx: unexpected extra argument {:?}", rest[1]);
        return ExitCode::from(2);
    }
    let root = resolve_root(path);
    let mut stdout = std::io::stdout();

    // Fallback queries (no usable trigram) make every file a candidate, so the daemon can't narrow
    // anything and shipping a potentially huge result set back over the socket would be slower than
    // ripgrep. Scan in-process instead: a pipelined parallel walk+search streamed straight to stdout,
    // exactly like rg (and entirely self-contained — no daemon, no `rg` binary).
    if rgx::is_fallback(&pattern, opts) {
        use std::io::BufWriter;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU64, Ordering};
        // Block-buffered (not the default line-buffered Stdout) so a match-everything query doesn't
        // flush once per line; the mutex serializes the parallel walk threads' writes.
        let out = Mutex::new(BufWriter::with_capacity(64 * 1024, std::io::stdout()));
        let bytes = AtomicU64::new(0);
        let res = rgx::stream_full_scan(&root, &pattern, opts, |c| {
            bytes.fetch_add(c.len() as u64, Ordering::Relaxed);
            if let Ok(mut w) = out.lock() {
                let _ = w.write_all(c);
            }
        });
        let _ = out.lock().map(|mut w| w.flush());
        return match res {
            Ok(()) if bytes.load(Ordering::Relaxed) == 0 => ExitCode::from(1),
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rgx: {e}");
                ExitCode::from(2)
            }
        };
    }

    match client::request_stream(&root, &Request::Search { opts, pattern }, &mut stdout) {
        Ok(0) => ExitCode::from(1),
        Ok(_) => ExitCode::SUCCESS,
        // A closed stdout (e.g. `rgx pat | head`) is a clean exit for a grep-like tool, not an error.
        Err(e) if is_broken_pipe(&e) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rgx: {e}");
            ExitCode::from(2)
        }
    }
}

/// `--compact`: the token-savings view. Unlike a bare search, this must see the whole result set to
/// count, group by file, and paginate — so it buffers instead of streaming, then renders one page.
/// The match set is identical to `rg`; only presentation differs (see `compact`).
fn compact_cmd(args: &[String]) -> ExitCode {
    let parsed = match parse_search(args, true) {
        Ok(p) => p,
        Err(code) => return code,
    };

    // Resume from the cursor (which carries the whole query) when given, else build a fresh query
    // from the flags + positionals. `prev` is the (total, fingerprint) at mint time, for the
    // staleness check below.
    let (pattern, opts, mode, start_after, page_size, root_hint, prev) = if let Some(tok) =
        parsed.cursor
    {
        // The cursor is self-contained, so any co-supplied query flag would be silently dropped.
        // Reject the combination explicitly rather than ignoring the flag.
        let stray_flags = parsed.page_size.is_some()
            || parsed.mode != Mode::Matches
            || parsed.opts != SearchOptions::default();
        if !parsed.positionals.is_empty() || stray_flags {
            eprintln!("rgx: --cursor is self-contained; don't combine it with a pattern or flags");
            return ExitCode::from(2);
        }
        let c = match cursor::decode(tok) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rgx: invalid --cursor ({e})");
                return ExitCode::from(2);
            }
        };
        let start_after = c.last_path.clone().map(|p| (p, c.last_lineno));
        (
            c.pattern,
            c.opts,
            c.mode,
            start_after,
            c.page_size,
            c.root_hint,
            Some((c.prev_total, c.fingerprint)),
        )
    } else {
        let Some((pattern, rest)) = parsed.positionals.split_first() else {
            usage();
            return ExitCode::from(2);
        };
        if rest.len() > 1 {
            eprintln!("rgx: unexpected extra argument {:?}", rest[1]);
            return ExitCode::from(2);
        }
        (
            pattern.to_string(),
            parsed.opts,
            parsed.mode,
            None,
            parsed.page_size.unwrap_or(compact::DEFAULT_PAGE_SIZE),
            rest.first().map(|s| s.to_string()),
            None,
        )
    };

    let root = resolve_root(root_hint.as_deref());
    let raw = match rgx::collect_search(&root, &pattern, opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rgx: {e}");
            return ExitCode::from(2);
        }
    };
    let page = compact::format(
        &raw,
        &pattern,
        opts,
        CompactOpts {
            mode,
            start_after,
            page_size,
            max_cols: compact::DEFAULT_MAX_COLS,
        },
    );

    let mut out = std::io::stdout();
    let _ = writeln!(out, "{}", page.header);
    let _ = out.write_all(page.body.as_bytes());
    if let Some(note) = page.staleness_note(prev) {
        let _ = writeln!(out, "note: {note}");
    }
    // Store the RESOLVED (absolute) root, not the raw positional, so following the cursor from a
    // different working directory resolves the same tree (resolve_root is idempotent on an absolute
    // path) rather than re-interpreting a relative path against the new cwd.
    let root_hint = Some(root.to_string_lossy().into_owned());
    if let Some(next) = page.next_cursor(mode, pattern, opts, page_size, root_hint) {
        let _ = writeln!(
            out,
            "next: rgx --compact --cursor {}",
            shell_quote(&cursor::encode(&next))
        );
    }
    let empty = match mode {
        Mode::Matches => page.total_matches == 0,
        _ => page.total_files == 0,
    };
    if empty {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Single-quote `s` for the next-page shell hint, escaping embedded single quotes.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// True if `e` is (or wraps) a broken-pipe I/O error.
fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
}

/// Parse `-A5` (attached) or `-A 5` (separate); returns (value, args_consumed).
fn context_value(args: &[String], i: usize) -> Option<(usize, usize)> {
    let a = &args[i];
    if a.len() > 2 {
        a[2..].parse().ok().map(|n| (n, 1))
    } else {
        args.get(i + 1).and_then(|v| v.parse().ok()).map(|n| (n, 2))
    }
}

/// Parse a long flag's value: `--name=VALUE` (inline) or `--name VALUE` (separate). Returns
/// `(value, args_consumed)`.
fn long_value<'a>(args: &'a [String], i: usize, name: &str) -> Option<(&'a str, usize)> {
    let prefix = format!("{name}=");
    if let Some(v) = args[i].strip_prefix(&prefix) {
        Some((v, 1))
    } else {
        args.get(i + 1).map(|v| (v.as_str(), 2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_flags_pattern_and_path() {
        let args = argv(&["-i", "-w", "-n", "needle", "src/"]);
        let p = parse_search(&args, false).unwrap();
        assert!(p.opts.case_insensitive && p.opts.word);
        assert!(p.cursor.is_none());
        assert_eq!(p.positionals, vec!["needle", "src/"]);
    }

    #[test]
    fn compact_accepts_cursor_page_size_and_modes() {
        for args in [
            argv(&["--page-size", "20", "needle"]),
            argv(&["--page-size=20", "needle"]),
        ] {
            let p = parse_search(&args, true).unwrap();
            assert_eq!(p.page_size, Some(20), "args: {args:?}");
            assert_eq!(p.positionals, vec!["needle"]);
        }
        let cursor_args = argv(&["--cursor=ABC", "-l"]);
        let c = parse_search(&cursor_args, true).unwrap();
        assert_eq!(c.cursor, Some("ABC"));
        assert_eq!(c.mode, Mode::Files);
        let count_args = argv(&["-c", "needle"]);
        let count = parse_search(&count_args, true).unwrap();
        assert_eq!(count.mode, Mode::Count);
    }

    #[test]
    fn compact_only_flags_are_rejected_outside_compact() {
        assert!(parse_search(&argv(&["--cursor", "x", "needle"]), false).is_err());
        assert!(parse_search(&argv(&["--page-size", "2", "needle"]), false).is_err());
        assert!(parse_search(&argv(&["-l", "needle"]), false).is_err());
    }

    #[test]
    fn double_dash_makes_flaglike_pattern_positional() {
        let args = argv(&["--", "--cursor"]);
        let p = parse_search(&args, true).unwrap();
        assert_eq!(p.positionals, vec!["--cursor"]);
        assert!(p.cursor.is_none());
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("fn x"), "'fn x'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
