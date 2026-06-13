//! `rgx` — a candidate-file index in front of ripgrep.
//!
//! The crate is split so each piece is testable in isolation (see `CLAUDE.md`):
//! - [`trigram`] — the atomic index unit and extraction helpers.
//! - [`query`] — turning a regex into a sound boolean trigram query.
//! - [`index`] — the trigram inverted index: build, candidate selection, incremental update, snapshot.
//! - [`confirm`] — ripgrep's own engine over a candidate file set (the matching authority).
//! - [`proto`]/[`server`]/[`client`]/[`paths`] — the per-project daemon and its wire protocol.
//!
//! [`search`] ties the search path together: pattern → trigram query → candidate files → ripgrep
//! confirm, transparently falling back to a full scan when the query carries no usable constraint.

use std::path::Path;

use anyhow::Result;

pub mod client;
pub mod compact;
pub mod config;
pub mod confirm;
pub mod cursor;
pub mod filter;
pub mod index;
pub mod mcp;
pub mod pagination;
pub mod paths;
pub mod proto;
pub mod query;
pub mod rank;
pub mod server;
pub mod skill;
pub mod sort;
pub mod status;
pub mod transport;
pub mod trigram;

use confirm::SearchOptions;
use filter::FilterSpec;
use index::Index;
use query::{Options as QueryOptions, Query};

/// The pattern actually handed to the regex engine: escaped when `-F` (fixed strings) is set.
pub fn effective_pattern(pattern: &str, opts: SearchOptions) -> String {
    if opts.fixed_strings {
        regex_syntax::escape(pattern)
    } else {
        pattern.to_string()
    }
}

fn query_options(opts: SearchOptions) -> QueryOptions {
    QueryOptions {
        case_insensitive: opts.case_insensitive,
        multi_line: opts.multi_line,
        dot_matches_new_line: opts.dot_matches_new_line,
    }
}

/// Whether `pattern` has no usable trigram constraint (so every file is a candidate). The CLI uses
/// this to scan such queries in-process — one process streamed straight to stdout, like ripgrep —
/// instead of paying the daemon round-trip to ship a potentially huge result set back.
pub fn is_fallback(pattern: &str, opts: SearchOptions) -> bool {
    // `-v` wants the lines that DON'T match, so every file is a candidate, not just the trigram hits —
    // the index can't narrow it. (`--hidden`/`--no-ignore` are NOT fallback: the daemon serves them
    // with the trigram candidates plus a delta walk of the files the index doesn't cover — see
    // `candidate_and_delta_paths`. They only fall back when the pattern itself has no trigram.)
    opts.invert
        || Query::for_pattern(&effective_pattern(pattern, opts), query_options(opts)).is_fallback()
}

/// Resolve the candidate files for `pattern` as owned paths, so a caller holding the index lock can
/// release it before the (potentially long) ripgrep confirm + output streaming — never hold the
/// index lock across blocking I/O. A fallback pattern yields every live file.
pub fn candidate_paths(
    index: &Index,
    root: &Path,
    pattern: &str,
    opts: SearchOptions,
    filter: &FilterSpec,
) -> Result<Vec<std::path::PathBuf>> {
    let effective = effective_pattern(pattern, opts);
    let query = Query::for_pattern(&effective, query_options(opts));
    let mut paths: Vec<std::path::PathBuf> = index
        .candidates(&query)
        .into_iter()
        .map(Path::to_path_buf)
        .collect();
    // `-g`/`-t`/`-T` only remove files, so filter the candidate set down — exactly the files `rg`
    // would keep (these are ripgrep's own matchers).
    if !filter.is_empty() {
        let ff = filter.compile(root)?;
        paths.retain(|p| ff.matched(p));
    }
    Ok(paths)
}

/// The files to search for a `--hidden`/`--no-ignore` query, accelerated: the trigram candidates from
/// the index (the default-walk files) **plus** the *delta* — the `toggled` walk's files that the index
/// doesn't cover (the hidden/ignored extras). Sound: the index half is trigram-narrowed (never drops a
/// match), the delta half carries no trigram constraint and is searched in full. `toggled` is passed
/// in (walked by the caller *outside* the index lock) so this stays a pure in-memory step. The same
/// `-g`/`-t`/`-T` filter is applied to the delta, exactly as `rg` applies it to every file.
pub fn candidate_and_delta_paths(
    index: &Index,
    root: &Path,
    pattern: &str,
    opts: SearchOptions,
    filter: &FilterSpec,
    toggled: Vec<std::path::PathBuf>,
) -> Result<Vec<std::path::PathBuf>> {
    let effective = effective_pattern(pattern, opts);
    let query = Query::for_pattern(&effective, query_options(opts));
    // Compile the `-g`/`-t`/`-T` matcher once and apply it to both halves (candidates and delta).
    let ff = (!filter.is_empty())
        .then(|| filter.compile(root))
        .transpose()?;
    let keep = |p: &Path| ff.as_ref().is_none_or(|f| f.matched(p));
    let mut paths: Vec<std::path::PathBuf> = index
        .candidates(&query)
        .into_iter()
        .filter(|p| keep(p))
        .map(Path::to_path_buf)
        .collect();
    for p in toggled {
        if !index.is_indexed_live(&p) && keep(&p) {
            paths.push(p);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Stream a content search against a (ready) index, emitting `path:line:text` chunks via `emit`.
///
/// One path for everything: the index turns the pattern into a candidate file set (a precise subset
/// for trigram-accelerable patterns, or *every* file for a fallback pattern with no usable trigram),
/// and ripgrep confirms over exactly that set. There is no separate "scan the tree" branch.
pub fn stream_search(
    index: &Index,
    root: &Path,
    pattern: &str,
    opts: SearchOptions,
    filter: &FilterSpec,
    emit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let effective = effective_pattern(pattern, opts);
    let query = Query::for_pattern(&effective, query_options(opts));
    let mut paths = index.candidates(&query);
    if !filter.is_empty() {
        let ff = filter.compile(root)?;
        paths.retain(|p| ff.matched(p));
    }
    confirm::search_streaming(&effective, &paths, root, opts, emit)
}

/// Pipelined full-tree walk+search (matching ripgrep's model), streaming through `sink`. Used by the
/// CLI for fallback queries (no usable trigram) and by the daemon's cold start before the first build
/// finishes — both fully in-process. Once the index is ready, [`stream_search`] handles
/// trigram-accelerable patterns.
pub fn stream_full_scan(
    root: impl AsRef<Path>,
    pattern: &str,
    opts: SearchOptions,
    filter: &FilterSpec,
    sink: impl Fn(&[u8]) + Sync,
) -> Result<()> {
    let effective = effective_pattern(pattern, opts);
    confirm::full_scan(root.as_ref(), &effective, opts, filter, sink)
}

/// Run a content search and buffer the whole `path:line:text` output, for callers that need the
/// entire result at once (the compact/paged view) rather than a stream. Trigram-accelerable patterns
/// go through the daemon (emitted in index file-id order, NOT path order); fallback patterns scan
/// in-process in nondeterministic order. Neither is guaranteed sorted, so the compact view sorts the
/// matches itself (see `compact::format`); the fallback block-sort here is a cheap extra that keeps
/// even the raw buffered bytes deterministic across runs.
pub fn collect_search(
    root: &Path,
    pattern: &str,
    opts: SearchOptions,
    filter: &FilterSpec,
) -> Result<Vec<u8>> {
    if is_fallback(pattern, opts) {
        let chunks = std::sync::Mutex::new(Vec::<Vec<u8>>::new());
        stream_full_scan(root, pattern, opts, filter, |c| {
            if let Ok(mut v) = chunks.lock() {
                v.push(c.to_vec());
            }
        })?;
        let mut chunks = chunks.into_inner().unwrap_or_default();
        chunks.sort_unstable(); // each chunk is one file's block, so this orders by path
        Ok(chunks.concat())
    } else {
        client::request(
            root,
            &proto::Request::Search {
                opts,
                pattern: pattern.to_string(),
                filter: filter.clone(),
            },
        )
    }
}

/// Run a content search and return ripgrep-style `path:line:text` bytes ordered by `sort` (the bare
/// `--sort`/`--sortr` path). `weights` supplies the `--sort=weight` map (the pattern's `<label>` tags
/// are stripped before searching, so the match set stays ripgrep's). Files are ordered by the sort
/// key; lines within a file keep ripgrep's order. `--` context separators are not reconstructed.
/// Callers must only reach here when `sort` actually reorders (`!sort.is_noop()`); the no-op case
/// belongs on the streaming path.
pub fn collect_search_sorted(
    root: &Path,
    pattern: &str,
    opts: SearchOptions,
    filter: &FilterSpec,
    sort: sort::SortSpec,
    weights: Option<&str>,
) -> Result<Vec<u8>> {
    let ranking = rank::parse(pattern, weights, opts)?;
    // Always collect WITH line numbers so `parse_rows` can key on `(path, lineno)`; whether the line
    // number is *printed* below follows `opts.line_number` (the bare path's `-n`/`-N`/TTY decision).
    let canonical = SearchOptions {
        line_number: true,
        ..opts
    };
    let raw = collect_search(root, &ranking.plain, canonical, filter)?;
    let text = String::from_utf8_lossy(&raw);
    let rows = compact::parse_rows(&text);
    let order = compact::file_order_map(&rows, sort, ranking.ranker.as_ref(), Some(root));
    let order_of = |path: &str| order.get(path).copied().unwrap_or(0);

    let mut idx: Vec<usize> = (0..rows.len()).collect();
    idx.sort_by(|&a, &b| {
        sort::cmp(
            (order_of(rows[a].path), rows[a].path, rows[a].lineno),
            (order_of(rows[b].path), rows[b].path, rows[b].lineno),
            sort.reverse,
        )
    });

    let mut out = Vec::with_capacity(raw.len());
    for &i in &idx {
        let r = &rows[i];
        let sep = if r.is_match { ':' } else { '-' };
        let line = if opts.line_number {
            format!("{}{sep}{}{sep}{}\n", r.path, r.lineno, r.text)
        } else {
            format!("{}{sep}{}\n", r.path, r.text)
        };
        out.extend_from_slice(line.as_bytes());
    }
    Ok(out)
}

/// Search a single explicitly-named file, the way `rg <pattern> <file>` does: the file is searched
/// directly — no walk, no ignore rules, and no `-g`/`-t`/`-T` filter (ripgrep searches an explicitly
/// named file unconditionally) — and printed under `file` exactly as given. No index or daemon is
/// involved; an indexed tree doesn't need one file's worth of acceleration. `emit` gets the rendered
/// `path:line:text` chunks.
pub fn stream_file_search(
    file: &str,
    pattern: &str,
    opts: SearchOptions,
    emit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let effective = effective_pattern(pattern, opts);
    confirm::search_file_streaming(&effective, Path::new(file), opts, emit)
}

/// Buffered form of [`stream_file_search`], for the compact/paged view.
pub fn collect_file_search(file: &str, pattern: &str, opts: SearchOptions) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    stream_file_search(file, pattern, opts, |c| {
        out.extend_from_slice(c);
        Ok(())
    })?;
    Ok(out)
}

/// Collecting convenience over [`stream_search`] (used in tests).
pub fn search(index: &Index, root: &Path, pattern: &str, opts: SearchOptions) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    stream_search(index, root, pattern, opts, &FilterSpec::default(), |c| {
        out.extend_from_slice(c);
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_file_search_omits_path_like_rg() {
        let tmp = std::env::temp_dir().join(format!("rgx_file_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.join("only.txt");
        std::fs::write(&p, b"alpha\nNEEDLE here\ngamma\n").unwrap();
        let given = p.to_str().unwrap();

        // Like `rg <pattern> <file>`: no path prefix. Line numbers follow opts (default on here).
        let out = collect_file_search(given, "NEEDLE", SearchOptions::default()).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "2:NEEDLE here\n");

        // With line numbers off (piped `rg`, no -n): bare text only.
        let no_ln = SearchOptions {
            line_number: false,
            ..SearchOptions::default()
        };
        let out = collect_file_search(given, "NEEDLE", no_ln).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "NEEDLE here\n");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn is_fallback_routing() {
        let plain = SearchOptions::default();
        // A trigram-accelerable pattern is not a fallback...
        assert!(!is_fallback("needle", plain));
        // ...and `--hidden`/`--no-ignore` keep it accelerated (served by the delta walk), so they must
        // NOT be fallback — a regression re-adding them here would silently full-scan.
        assert!(!is_fallback(
            "needle",
            SearchOptions {
                hidden: true,
                ..plain
            }
        ));
        assert!(!is_fallback(
            "needle",
            SearchOptions {
                no_ignore: true,
                ..plain
            }
        ));
        // `-v` needs every file, and a no-trigram pattern can't be narrowed: both fall back.
        assert!(is_fallback(
            "needle",
            SearchOptions {
                invert: true,
                ..plain
            }
        ));
        assert!(is_fallback(
            ".",
            SearchOptions {
                hidden: true,
                ..plain
            }
        ));
    }
}
