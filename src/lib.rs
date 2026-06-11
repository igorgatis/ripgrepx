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
pub mod confirm;
pub mod index;
pub mod mcp;
pub mod paths;
pub mod proto;
pub mod query;
pub mod server;
pub mod skill;
pub mod status;
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

/// Cold-start scan, used only while the daemon's index isn't built yet: walk the tree ripgrep would
/// pipelined walk+search (matching ripgrep), streaming through `sink`. Used by the CLI for fallback
/// queries (no usable trigram) and by the daemon's cold start, both fully in-process. Once the index
/// is ready, [`stream_search`] handles trigram-accelerable patterns.
pub fn stream_full_scan(
    root: impl AsRef<Path>,
    pattern: &str,
    opts: SearchOptions,
    sink: impl Fn(&[u8]) + Sync,
) -> Result<()> {
    let effective = effective_pattern(pattern, opts);
    confirm::full_scan(root.as_ref(), &effective, opts, sink)
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
