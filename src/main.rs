//! rgx CLI. A bare `rgx <pattern>` is an (accelerated) ripgrep content search; the `--server` gate
//! holds daemon management, and `--find` does fd/find-style name lookup. See `docs/cli.md`.
//!
//! Flags are recognized only as the leading token (rgx adds as few as possible to rg's surface).
//! The rg flag passthrough is a deliberate subset for now (-i, -s, -w, -F, -U, -A/-B/-C, `--`).

use std::io::Write;
use std::process::ExitCode;

use rgx::confirm::SearchOptions;
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
        Some("--find") => find_cmd(&args[1..]),
        Some("--skill") => match rgx::skill::install() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rgx --skill: {e}");
                ExitCode::from(2)
            }
        },
        Some("-h" | "--help") => {
            usage();
            ExitCode::SUCCESS
        }
        _ => content_cmd(&args),
    }
}

fn usage() {
    eprintln!(
        "usage:\n  rgx [flags] <pattern> [path]     content search (accelerated ripgrep)\n  \
         rgx --find <name|path> [path]    find files/dirs by name\n  \
         rgx --server [start|stop|status|watch|mcp]\n\nflags: -i -s -w -F -U -A<n> -B<n> -C<n> --"
    );
}

fn server_cmd(rest: &[String]) -> ExitCode {
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
        Some("mcp") => match mcp::run(root) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rgx --server mcp: {e}");
                ExitCode::from(2)
            }
        },
        Some(other) => {
            eprintln!("rgx --server: unknown subcommand {other:?}");
            ExitCode::from(2)
        }
    }
}

fn find_cmd(rest: &[String]) -> ExitCode {
    let Some(needle) = rest.first() else {
        eprintln!("usage: rgx --find <name|path> [path]");
        return ExitCode::from(2);
    };
    let root = resolve_root(rest.get(1).map(String::as_str));
    match client::request(
        &root,
        &Request::Find {
            needle: needle.clone(),
            limit: 1000,
        },
    ) {
        Ok(out) => emit(out),
        Err(e) => {
            eprintln!("rgx: {e}");
            ExitCode::from(2)
        }
    }
}

fn content_cmd(args: &[String]) -> ExitCode {
    let mut opts = SearchOptions::default();
    let mut positionals: Vec<&str> = Vec::new();
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
            ctx if ctx.starts_with("-A") || ctx.starts_with("-B") || ctx.starts_with("-C") => {
                let (n, consumed) = match context_value(args, i) {
                    Some(v) => v,
                    None => {
                        eprintln!("rgx: {ctx} needs a number");
                        return ExitCode::from(2);
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
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    let Some((pattern, rest)) = positionals.split_first() else {
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

fn emit(out: Vec<u8>) -> ExitCode {
    let _ = std::io::stdout().write_all(&out);
    if out.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
