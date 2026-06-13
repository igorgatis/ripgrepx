//! rgx CLI. A bare `rgx <pattern>` is an (accelerated) ripgrep content search; the `--server` gate
//! holds daemon management, `--agent` the AI surface (MCP/skill), and `--find` does fd/find-style
//! name lookup. See `docs/cli.md`.
//!
//! Flags are recognized only as the leading token (rgx adds as few as possible to rg's surface).
//! The rg flag passthrough is a deliberate subset for now (-i, -s, -w, -F, -U, -v, -o, -e/--regexp,
//! -A/-B/-C, -g/--glob, -t/--type, -T/--type-not, --hidden, --no-ignore, `--`, and `--sort`/`--sortr`);
//! `--weights` is rgx's own (feeds `--sort=weight`).

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use rgx::compact::{self, CompactOpts};
use rgx::confirm::SearchOptions;
use rgx::cursor::{self, Mode};
use rgx::filter::FilterSpec;
use rgx::paths::resolve_root;
use rgx::proto::Request;
use rgx::sort::SortSpec;
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
        Some("-V" | "--version") => {
            println!("rgx {}", env!("CARGO_PKG_VERSION"));
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
         rgx --server [start|stop|restart|status|watch]\n  \
         rgx --agent [mcp|skill|install|uninstall|list]\n\n\
         flags: -i -s -w -n -F -U -v -o -e<pat> -A<n> -B<n> -C<n> -g<glob> -t<type> -T<type> --hidden --no-ignore --sort=KEY --\n\
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
  rgx --server [start|stop|restart|status|watch]   background index server  (rgx --server --help)
  rgx --agent [mcp|skill|install|uninstall|list]   AI-agent integration     (rgx --agent --help)
  rgx --version                                    print the rgx version (also -V)

DROP-IN FOR ripgrep — `rgx <pattern>` takes the same command line as `rg`, same output. Flags
(anywhere, like rg): -i -s -w -n -F -U -v -o -e/--regexp -A<n> -B<n> -C<n> -g/--glob -t/--type
-T/--type-not --hidden --no-ignore --. rgx's own modes are recognized only as the first token.
Examples:
    rgx 'fn \\w+_total' src/    rgx -t rust TODO    rgx -e foo -e bar    rgx -e --server (literal)

ORDER results like `rg --sort` — `--sort=KEY` (asc) / `--sortr=KEY` (desc), KEY = path | modified |
accessed | created (file metadata) | weight (relevance). For weight, declare branch weights with
`--weights=label:weight,...` and tag regex branches in the pattern with <label>; each file ranks by its
highest matched weight (relative numbers, larger first; tags are stripped, so matches are unchanged):
    rgx --sortr=modified TODO src/     rgx --sort=weight --weights=a:0.9,b:0.1 '(foo<a>|bar<b>)'

SERVER — the indexer starts on first use and stays fresh on its own; subcommands act on the cwd's
project. `status` reports readiness/counts/age, `watch` repaints live. Index + socket live under
$RGX_CACHE_DIR (else config `cache_dir`, else ~/.cache/rgx): a rebuildable cache, safe to delete.

FOR AI AGENTS — works with Claude Code, Codex, and any MCP client; see `rgx --agent --help`.
  Compaction — `--compact` groups matches by file, pages behind an opaque cursor, trims long lines.
  Nothing is dropped; the header reports the full total. `--page-size N` (default 50), `--cursor TOK`
  next page, `-l` files only, `-c` per-file counts. The cursor carries the whole query, so paging
  can't drift; a result set that changed gets a `note:`.
  MCP — `rgx --agent mcp` (stdio) exposes content_search (compact paged view), file_search, status.
  Install — `rgx --agent install [claude|codex|cursor|gemini|vscode]` writes a per-agent bundle.

Docs: https://github.com/igorgatis/ripgrepx
";

/// `rgx --server --help` (or `--help --server`): the index-server subcommands in full.
const SERVER_HELP: &str = "\
rgx --server — the background index server. Subcommands act on the current directory's project; the
indexer also starts on first search, so you rarely manage it by hand.

  rgx --server          run the indexer in the foreground
  rgx --server start    start the background indexer for this project
  rgx --server stop     stop it
  rgx --server restart  stop it (if running) and start a fresh daemon — e.g. after upgrading rgx
  rgx --server status   one-shot: readiness, file/trigram counts, memory, last-sync age
  rgx --server watch    live status, repaints on every change until interrupted

Index + socket live under $RGX_CACHE_DIR (else config `cache_dir`, else $XDG_CACHE_HOME/rgx, else
~/.cache/rgx): a rebuildable cache, safe to delete, never written into the indexed tree.

The daemon exits after `idle_timeout_secs` of no searches (default 1 h; zero or negative stays
resident forever) and respawns on the next one. A repo whose cold build is cheap (under
`persist_threshold_ms`, default 1 s) is kept in RAM only, with no snapshot — `status` shows
`ram-only`. Both are configurable; see docs/cli.md.
";

/// `rgx --agent --help` (or `--help --agent`): the AI-agent surface, with setup for the common hosts.
const AGENT_HELP: &str = "\
rgx --agent — integrate rgx with AI coding agents.

  rgx --agent mcp                       run the stdio MCP server: content_search, file_search, status
  rgx --agent skill                     print the agent skill (markdown) to stdout
  rgx --agent install   [TARGET...]     install the rgx bundle for each agent
  rgx --agent uninstall [TARGET...]     remove what install wrote
  rgx --agent list                      show detected agents and install status

TARGET (omit to auto-detect installed agents): claude  codex  cursor  gemini  vscode
Scope:   --user (default for claude/codex/gemini) or --project (default for cursor/vscode).
Confirm: install/uninstall print the exact changes and ask before touching anything. --yes (-y)
         applies without prompting (required when stdin is not a TTY); --dry-run (-n) only previews.

install writes only where rgx owns the namespace — Claude skill dir, a Gemini extension — or edits
shared files idempotently (a removable marked block in AGENTS.md / copilot-instructions, a merged
\"rgx\" key in .cursor/mcp.json / .vscode/mcp.json). MCP registration that belongs to a host's own CLI
(claude/codex mcp add) is printed for you to run, never executed.
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
        Some("start") => spawn_and_report(&root, "starting"),
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
        Some("restart") => {
            match client::request_existing(&root, &Request::Shutdown) {
                Ok(Some(_)) => {
                    if !client::wait_until_stopped(&root) {
                        eprintln!("rgx: previous daemon is still shutting down; try again");
                        return ExitCode::from(2);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("rgx: {e}");
                    return ExitCode::from(2);
                }
            }
            spawn_and_report(&root, "restarting")
        }
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
                    ram_only: false,
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

/// `rgx --agent <mcp|skill|install|uninstall|list>`: the AI-agent surface. `mcp` runs the stdio MCP
/// server; `skill` prints the agent skill; `install`/`uninstall` manage per-agent bundles; `list`
/// shows status. `--help` prints the agent guide; a missing subcommand is an error.
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
        Some("install") => agent_result("install", rgx::skill::install_cli(&rest[1..])),
        Some("uninstall") => agent_result("uninstall", rgx::skill::uninstall_cli(&rest[1..])),
        Some("list") => agent_result("list", rgx::skill::list()),
        Some("-h" | "--help" | "help") => {
            print!("{AGENT_HELP}");
            ExitCode::SUCCESS
        }
        None => {
            eprintln!(
                "rgx --agent: pick a subcommand (mcp|skill|install|uninstall|list); \
                 see `rgx --agent --help`"
            );
            ExitCode::from(2)
        }
        Some(other) => {
            eprintln!("rgx --agent: unknown subcommand {other:?}");
            ExitCode::from(2)
        }
    }
}

fn spawn_and_report(root: &Path, verb: &str) -> ExitCode {
    match client::spawn_daemon(root) {
        Ok(()) => {
            println!("rgx: daemon {verb} for {}", root.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rgx: {e}");
            ExitCode::from(2)
        }
    }
}

fn agent_result(name: &str, r: anyhow::Result<()>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rgx --agent {name}: {e}");
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
    sort: SortSpec,
    weights: Option<&'a str>,
    filter: FilterSpec,
    /// `-e`/`--regexp` patterns (repeatable, OR'd). When non-empty, the positionals are all paths and
    /// there is no positional pattern (ripgrep's rule).
    patterns: Vec<&'a str>,
    mode: Mode,
    positionals: Vec<&'a str>,
}

fn parse_search<'a>(args: &'a [String], compact: bool) -> Result<ParsedSearch<'a>, ExitCode> {
    let mut opts = SearchOptions::default();
    let mut positionals: Vec<&str> = Vec::new();
    let mut cursor: Option<&str> = None;
    let mut page_size: Option<usize> = None;
    let mut sort = SortSpec::default();
    let mut weights: Option<&str> = None;
    let mut filter = FilterSpec::default();
    let mut patterns: Vec<&str> = Vec::new();
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
            "-v" | "--invert-match" => opts.invert = true,
            "-o" | "--only-matching" => opts.only_matching = true,
            "--hidden" => opts.hidden = true,
            "--no-ignore" => opts.no_ignore = true,
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
            p if p == "--sort" || p.starts_with("--sort=") => {
                sort = parse_sort_flag(args, i, "--sort", false)?;
                i += long_value(args, i, "--sort").map_or(1, |(_, c)| c);
                continue;
            }
            p if p == "--sortr" || p.starts_with("--sortr=") => {
                sort = parse_sort_flag(args, i, "--sortr", true)?;
                i += long_value(args, i, "--sortr").map_or(1, |(_, c)| c);
                continue;
            }
            p if p == "--weights" || p.starts_with("--weights=") => {
                let Some((v, consumed)) = long_value(args, i, "--weights") else {
                    eprintln!("rgx: --weights needs a value");
                    return Err(ExitCode::from(2));
                };
                weights = Some(v);
                i += consumed;
                continue;
            }
            g if is_value_flag(g, "-g", "--glob") => {
                let Some((v, consumed)) = take_value_flag(args, i, "-g", "--glob") else {
                    eprintln!("rgx: -g/--glob needs a value");
                    return Err(ExitCode::from(2));
                };
                filter.globs.push(v.to_string());
                i += consumed;
                continue;
            }
            t if is_value_flag(t, "-t", "--type") => {
                let Some((v, consumed)) = take_value_flag(args, i, "-t", "--type") else {
                    eprintln!("rgx: -t/--type needs a value");
                    return Err(ExitCode::from(2));
                };
                filter.types.push(v.to_string());
                i += consumed;
                continue;
            }
            tn if is_value_flag(tn, "-T", "--type-not") => {
                let Some((v, consumed)) = take_value_flag(args, i, "-T", "--type-not") else {
                    eprintln!("rgx: -T/--type-not needs a value");
                    return Err(ExitCode::from(2));
                };
                filter.type_nots.push(v.to_string());
                i += consumed;
                continue;
            }
            e if is_value_flag(e, "-e", "--regexp") => {
                let Some((v, consumed)) = take_value_flag(args, i, "-e", "--regexp") else {
                    eprintln!("rgx: -e/--regexp needs a value");
                    return Err(ExitCode::from(2));
                };
                patterns.push(v);
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
        sort,
        weights,
        filter,
        patterns,
        mode,
        positionals,
    })
}

/// Resolve the search pattern + the paths from the parsed flags, applying ripgrep's `-e` rule: when
/// any `-e`/`--regexp` is given the positionals are all paths (no positional pattern), and the
/// patterns are OR'd into one regex. A single pattern passes through unchanged (byte-identical to a
/// bare search); multiple are joined as `(?:p1)|(?:p2)|…`, with `-F` escaping applied per-pattern and
/// fixed-strings then cleared (the joined form is a real regex). Returns `(pattern, opts, paths)`.
fn resolve_pattern<'a>(
    parsed: &ParsedSearch<'a>,
) -> Result<(String, SearchOptions, Vec<&'a str>), ExitCode> {
    let (raw_patterns, paths): (Vec<&str>, Vec<&str>) = if parsed.patterns.is_empty() {
        match parsed.positionals.split_first() {
            Some((pat, rest)) => (vec![pat], rest.to_vec()),
            None => {
                usage();
                return Err(ExitCode::from(2));
            }
        }
    } else {
        (parsed.patterns.clone(), parsed.positionals.clone())
    };
    if paths.len() > 1 {
        eprintln!("rgx: unexpected extra argument {:?}", paths[1]);
        return Err(ExitCode::from(2));
    }
    let mut opts = parsed.opts;
    let pattern = if raw_patterns.len() == 1 {
        raw_patterns[0].to_string()
    } else {
        let joined = raw_patterns
            .iter()
            .map(|p| format!("(?:{})", rgx::effective_pattern(p, opts)))
            .collect::<Vec<_>>()
            .join("|");
        opts.fixed_strings = false; // already escaped per-pattern; the join is a real regex
        joined
    };
    Ok((pattern, opts, paths))
}

/// Whether `a` is a repeatable value flag in any of its four forms: `-x` / `-xVAL` (short) or
/// `--name` / `--name=VAL` (long).
fn is_value_flag(a: &str, short: &str, long: &str) -> bool {
    a == short
        || a == long
        || a.starts_with(&format!("{long}="))
        || (a.starts_with(short) && a.len() > short.len())
}

/// Extract a repeatable value flag's value and the number of argv tokens consumed (`-xVAL`/`--name=VAL`
/// inline = 1, `-x VAL`/`--name VAL` separate = 2).
fn take_value_flag<'a>(
    args: &'a [String],
    i: usize,
    short: &str,
    long: &str,
) -> Option<(&'a str, usize)> {
    let a = args[i].as_str();
    if a == short || a == long {
        return args.get(i + 1).map(|v| (v.as_str(), 2));
    }
    if let Some(v) = a.strip_prefix(short).filter(|v| !v.is_empty()) {
        return Some((v, 1));
    }
    a.strip_prefix(&format!("{long}=")).map(|v| (v, 1))
}

/// Parse a `--sort`/`--sortr` flag value into a [`SortSpec`], reporting a usage error as an exit code.
fn parse_sort_flag(
    args: &[String],
    i: usize,
    name: &str,
    reverse: bool,
) -> Result<SortSpec, ExitCode> {
    let Some((v, _)) = long_value(args, i, name) else {
        eprintln!("rgx: {name} needs a value (none|path|modified|accessed|created|weight)");
        return Err(ExitCode::from(2));
    };
    rgx::sort::parse(v, reverse).map_err(|e| {
        eprintln!("rgx: {e}");
        ExitCode::from(2)
    })
}

/// Validate `-g`/`-t`/`-T` before dispatch (a bad glob or unknown type name), so the error surfaces
/// here rather than being swallowed on the daemon path. The daemon recompiles its own copy.
fn check_filter(filter: &FilterSpec, root: &Path) -> Result<(), ExitCode> {
    filter.compile(root).map(|_| ()).map_err(|e| {
        eprintln!("rgx: {e}");
        ExitCode::from(2)
    })
}

/// Validate the `--sort`/`--weights`/`-F` coherence: `weight` needs weights, weights apply only to it,
/// and `-F` (fixed strings) can't drive weighted match (which needs `<label>` regex annotations).
/// `fixed_strings` is the *original* flag — `-e` combining may later clear it, so check it here, before
/// that, or the rejection is silently bypassed by multiple `-e` patterns.
fn check_sort(sort: SortSpec, weights: Option<&str>, fixed_strings: bool) -> Result<(), ExitCode> {
    if sort.needs_weights() && weights.is_none() {
        eprintln!("rgx: --sort=weight needs --weights=label:weight,...");
        return Err(ExitCode::from(2));
    }
    if !sort.needs_weights() && weights.is_some() {
        eprintln!("rgx: --weights applies only to --sort=weight");
        return Err(ExitCode::from(2));
    }
    if sort.needs_weights() && fixed_strings {
        eprintln!(
            "rgx: --sort=weight cannot be combined with -F (fixed strings has no branches to weight)"
        );
        return Err(ExitCode::from(2));
    }
    Ok(())
}

fn content_cmd(args: &[String]) -> ExitCode {
    let parsed = match parse_search(args, false) {
        Ok(p) => p,
        Err(code) => return code,
    };
    // Validate sort/weights/-F coherence before resolve_pattern — using the original `parsed.opts`, so
    // `-e` combining (which clears fixed_strings) can't bypass the -F+weight rejection. Doing it first
    // also keeps error precedence consistent with compact_cmd.
    if let Err(code) = check_sort(parsed.sort, parsed.weights, parsed.opts.fixed_strings) {
        return code;
    }
    let (pattern, opts, paths) = match resolve_pattern(&parsed) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let path = paths.first().copied();
    let root = resolve_root(path);
    if let Err(code) = check_filter(&parsed.filter, &root) {
        return code;
    }

    // `--sort`/`--sortr`: reorder results, the way `rg --sort` does. Reordering requires seeing the
    // whole result set, so it leaves the streaming fast path and buffers (still single command, no
    // `rg` binary). Absence of `--sort` keeps today's byte-for-byte streaming below.
    if !parsed.sort.is_noop() {
        return match rgx::collect_search_sorted(
            &root,
            &pattern,
            opts,
            &parsed.filter,
            parsed.sort,
            parsed.weights,
        ) {
            Ok(bytes) => {
                match std::io::stdout().write_all(&bytes) {
                    Ok(()) if bytes.is_empty() => ExitCode::from(1),
                    Ok(()) => ExitCode::SUCCESS,
                    // `rgx --sortr=modified | head`: a closed pipe is a clean exit.
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("rgx: {e}");
                        ExitCode::from(2)
                    }
                }
            }
            Err(e) => {
                eprintln!("rgx: {e}");
                ExitCode::from(2)
            }
        };
    }

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
        let res = rgx::stream_full_scan(&root, &pattern, opts, &parsed.filter, |c| {
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

    let req = Request::Search {
        opts,
        pattern,
        filter: parsed.filter,
    };
    match client::request_stream(&root, &req, &mut stdout) {
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
/// The resolved compact query — built fresh from flags/positionals or unpacked from a `--cursor`.
/// A named struct (vs a positional tuple) so adding a field can't silently swap two same-typed ones.
struct CompactQuery {
    pattern: String,
    opts: SearchOptions,
    mode: Mode,
    start_after: Option<(i64, String, u64, u32)>,
    page_size: usize,
    root_hint: Option<String>,
    sort: SortSpec,
    weights: Option<String>,
    filter: FilterSpec,
    /// `(total, fingerprint)` at mint time when resuming a cursor, for the staleness note.
    prev: Option<(usize, u32)>,
}

/// The match set is identical to `rg`; only presentation differs (see `compact`).
fn compact_cmd(args: &[String]) -> ExitCode {
    let parsed = match parse_search(args, true) {
        Ok(p) => p,
        Err(code) => return code,
    };

    // The cursor blob lives in the cwd's daemon (the pagination home), keyed by the printed token.
    // Resuming from the same directory finds it; from elsewhere it cleanly misses.
    let cwd = resolve_root(None);

    // Resume from the cursor (which carries the whole query) when given, else build a fresh query
    // from the flags + positionals. `prev` is the (total, fingerprint) at mint time, for the
    // staleness check below.
    let plan = if let Some(tok) = parsed.cursor {
        // The cursor is self-contained, so any co-supplied query flag would be silently dropped.
        // Reject the combination explicitly rather than ignoring the flag.
        let stray_flags = parsed.page_size.is_some()
            || !parsed.sort.is_noop()
            || parsed.weights.is_some()
            || !parsed.filter.is_empty()
            || !parsed.patterns.is_empty()
            || parsed.mode != Mode::Matches
            || parsed.opts != SearchOptions::default();
        if !parsed.positionals.is_empty() || stray_flags {
            eprintln!("rgx: --cursor is self-contained; don't combine it with a pattern or flags");
            return ExitCode::from(2);
        }
        let blob = match rgx::client::take_cursor(&cwd, tok) {
            Ok(Some(blob)) => blob,
            Ok(None) => {
                eprintln!("rgx: pagination expired — re-run the search");
                return ExitCode::from(2);
            }
            Err(e) => {
                eprintln!("rgx: {e}");
                return ExitCode::from(2);
            }
        };
        let c = match cursor::decode(&blob) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rgx: invalid --cursor ({e})");
                return ExitCode::from(2);
            }
        };
        let start_after = c
            .last_path
            .clone()
            .map(|p| (c.last_order, p, c.last_lineno, c.last_ordinal));
        CompactQuery {
            pattern: c.pattern,
            opts: c.opts,
            mode: c.mode,
            start_after,
            page_size: c.page_size,
            root_hint: c.root_hint,
            sort: c.sort,
            weights: c.weights,
            filter: c.filter,
            prev: Some((c.prev_total, c.fingerprint)),
        }
    } else {
        if let Err(code) = check_sort(parsed.sort, parsed.weights, parsed.opts.fixed_strings) {
            return code;
        }
        let (pattern, opts, paths) = match resolve_pattern(&parsed) {
            Ok(t) => t,
            Err(code) => return code,
        };
        CompactQuery {
            pattern,
            opts,
            mode: parsed.mode,
            start_after: None,
            page_size: parsed.page_size.unwrap_or(compact::DEFAULT_PAGE_SIZE),
            root_hint: paths.first().map(|s| s.to_string()),
            sort: parsed.sort,
            weights: parsed.weights.map(str::to_string),
            filter: parsed.filter.clone(),
            prev: None,
        }
    };
    let CompactQuery {
        pattern,
        opts,
        mode,
        start_after,
        page_size,
        root_hint,
        sort,
        weights,
        filter,
        prev,
    } = plan;

    // `--sort=weight`: strip `<label>` annotations to get the plain pattern that is actually searched
    // (so the match set stays ripgrep's), and build a ranker. For every other key `weights` is None,
    // so the pattern passes through unchanged and there is no ranker.
    let ranking = match rgx::rank::parse(&pattern, weights.as_deref(), opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rgx: {e}");
            return ExitCode::from(2);
        }
    };
    let root = resolve_root(root_hint.as_deref());
    if let Err(code) = check_filter(&filter, &root) {
        return code;
    }
    let raw = match rgx::collect_search(&root, &ranking.plain, opts, &filter) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rgx: {e}");
            return ExitCode::from(2);
        }
    };
    let page = compact::format(
        &raw,
        &ranking.plain,
        opts,
        CompactOpts {
            mode,
            start_after,
            page_size,
            max_cols: compact::DEFAULT_MAX_COLS,
            sort,
            ranker: ranking.ranker,
            root: Some(root.clone()),
        },
    );

    let mut out = std::io::stdout();
    let _ = writeln!(out, "{}", page.header);
    let _ = out.write_all(page.body.as_bytes());
    if let Some(note) = page.staleness_note(prev) {
        let _ = writeln!(out, "note: {note}");
    }
    // Carry the original positional scope (a short relative path, or None for the cwd) rather than the
    // resolved absolute root: the caller pages from the same directory, so it re-resolves the same
    // tree. `root_hint` is already that scope from the branches above. Park the blob in the cwd daemon
    // and print its short token.
    if let Some(next) = page.next_cursor(
        mode, pattern, opts, filter, page_size, root_hint, sort, weights,
    ) {
        match rgx::client::store_cursor(&cwd, cursor::encode(&next)) {
            Ok(token) => {
                let _ = writeln!(out, "next: rgx --compact --cursor {}", shell_quote(&token));
            }
            Err(e) => {
                let _ = writeln!(out, "note: could not store pagination cursor ({e})");
            }
        }
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
    fn parses_invert_hidden_no_ignore() {
        let args = argv(&["-v", "-o", "--hidden", "--no-ignore", "needle"]);
        let p = parse_search(&args, false).unwrap();
        assert!(p.opts.invert && p.opts.only_matching && p.opts.hidden && p.opts.no_ignore);
        assert_eq!(p.positionals, vec!["needle"]);
    }

    #[test]
    fn check_sort_rejects_fixed_strings_with_weight() {
        let weight = rgx::sort::parse("weight", false).unwrap();
        let modified = rgx::sort::parse("modified", false).unwrap();
        // -F is incompatible with weighted match (the guard `-e` combining must not bypass).
        assert!(check_sort(weight, Some("w:1"), true).is_err());
        assert!(check_sort(weight, Some("w:1"), false).is_ok());
        // -F is fine with any other sort key.
        assert!(check_sort(modified, None, true).is_ok());
    }

    #[test]
    fn dash_e_collects_patterns_and_treats_positionals_as_paths() {
        let args = argv(&["-e", "foo", "-e", "bar", "src/"]);
        let p = parse_search(&args, false).unwrap();
        assert_eq!(p.patterns, vec!["foo", "bar"]);
        assert_eq!(p.positionals, vec!["src/"]);
        // Multiple -e OR into one regex; with -e present the positional is a path, not the pattern.
        let (pat, _opts, paths) = resolve_pattern(&p).unwrap();
        assert_eq!(pat, "(?:foo)|(?:bar)");
        assert_eq!(paths, vec!["src/"]);
    }

    #[test]
    fn single_pattern_passes_through_and_fixed_strings_escapes_each() {
        // A single -e (or bare positional) is byte-identical to today — no wrapping.
        let one_args = argv(&["-e", "fn .*"]);
        let one = parse_search(&one_args, false).unwrap();
        assert_eq!(resolve_pattern(&one).unwrap().0, "fn .*");
        // -F with multiple -e escapes each branch and clears fixed-strings (the join is real regex).
        let many_args = argv(&["-F", "-e", "a.b", "-e", "c+d"]);
        let many = parse_search(&many_args, false).unwrap();
        let (pat, opts, _) = resolve_pattern(&many).unwrap();
        assert_eq!(pat, r"(?:a\.b)|(?:c\+d)");
        assert!(!opts.fixed_strings);
    }

    #[test]
    fn parses_glob_and_type_flags_repeatable() {
        // Mix attached/separate short forms and the long forms; all repeatable.
        let args = argv(&[
            "-trust",
            "--type",
            "py",
            "-g",
            "*.rs",
            "--glob=!*_test.rs",
            "-Tlock",
            "needle",
        ]);
        let p = parse_search(&args, false).unwrap();
        assert_eq!(p.filter.types, vec!["rust", "py"]);
        assert_eq!(p.filter.globs, vec!["*.rs", "!*_test.rs"]);
        assert_eq!(p.filter.type_nots, vec!["lock"]);
        assert_eq!(p.positionals, vec!["needle"]);
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
