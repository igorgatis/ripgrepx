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
pub mod confirm;
pub mod cursor;
pub mod index;
pub mod mcp;
pub mod paths;
pub mod proto;
pub mod query;
pub mod server;
pub mod skill;
pub mod status;
pub mod transport;
pub mod trigram;

use confirm::SearchOptions;
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
    Query::for_pattern(&effective_pattern(pattern, opts), query_options(opts)).is_fallback()
}

/// Resolve the candidate files for `pattern` as owned paths, so a caller holding the index lock can
/// release it before the (potentially long) ripgrep confirm + output streaming — never hold the
/// index lock across blocking I/O. A fallback pattern yields every live file.
pub fn candidate_paths(
    index: &Index,
    pattern: &str,
    opts: SearchOptions,
) -> Vec<std::path::PathBuf> {
    let effective = effective_pattern(pattern, opts);
    let query = Query::for_pattern(&effective, query_options(opts));
    index
        .candidates(&query)
        .into_iter()
        .map(Path::to_path_buf)
        .collect()
}

/// Stream a content search against a (ready) index, emitting `path:line:text` chunks via `emit`.
///
/// One path for everything: the index turns the pattern into a candidate file set (a precise subset
/// for trigram-accelerable patterns, or *every* file for a fallback pattern with no usable trigram),
/// and ripgrep confirms over exactly that set. There is no separate "scan the tree" branch.
pub fn stream_search(
    index: &Index,
    pattern: &str,
    opts: SearchOptions,
    emit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let effective = effective_pattern(pattern, opts);
    let query = Query::for_pattern(&effective, query_options(opts));
    let paths = index.candidates(&query);
    confirm::search_streaming(&effective, &paths, opts, emit)
}

/// Pipelined full-tree walk+search (matching ripgrep's model), streaming through `sink`. Used by the
/// CLI for fallback queries (no usable trigram) and by the daemon's cold start before the first build
/// finishes — both fully in-process. Once the index is ready, [`stream_search`] handles
/// trigram-accelerable patterns.
pub fn stream_full_scan(
    root: impl AsRef<Path>,
    pattern: &str,
    opts: SearchOptions,
    sink: impl Fn(&[u8]) + Sync,
) -> Result<()> {
    let effective = effective_pattern(pattern, opts);
    confirm::full_scan(root.as_ref(), &effective, opts, sink)
}

/// Run a content search and buffer the whole `path:line:text` output, for callers that need the
/// entire result at once (the compact/paged view) rather than a stream. Trigram-accelerable patterns
/// go through the daemon (emitted in index file-id order, NOT path order); fallback patterns scan
/// in-process in nondeterministic order. Neither is guaranteed sorted, so the compact view sorts the
/// matches itself (see `compact::format`); the fallback block-sort here is a cheap extra that keeps
/// even the raw buffered bytes deterministic across runs.
pub fn collect_search(root: &Path, pattern: &str, opts: SearchOptions) -> Result<Vec<u8>> {
    if is_fallback(pattern, opts) {
        let chunks = std::sync::Mutex::new(Vec::<Vec<u8>>::new());
        stream_full_scan(root, pattern, opts, |c| {
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
            },
        )
    }
}

/// Collecting convenience over [`stream_search`] (used in tests).
pub fn search(index: &Index, pattern: &str, opts: SearchOptions) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    stream_search(index, pattern, opts, |c| {
        out.extend_from_slice(c);
        Ok(())
    })?;
    Ok(out)
}
