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
    // `-v` wants the lines that DON'T match (so every file is a candidate, not just the trigram
    // hits), and `--hidden`/`--no-ignore` want files the index never indexed. None can be served from
    // the trigram index, so they scan the tree in-process with the adjusted walk/searcher.
    opts.invert
        || opts.hidden
        || opts.no_ignore
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
    let raw = collect_search(root, &ranking.plain, opts, filter)?;
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
        out.extend_from_slice(format!("{}{sep}{}{sep}{}\n", r.path, r.lineno, r.text).as_bytes());
    }
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
