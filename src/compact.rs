//! Token-savings presentation: reshape ripgrep's `path:line:text` stream into a compact, paginated
//! view for agents. This is a pure transform over already-rendered output — matching stays 100%
//! ripgrep (see `confirm`). The contract is deliberately weaker than the byte-for-byte CLI: the
//! *match set* is identical to `rg` (nothing is ever dropped — pagination is the only volume
//! control), but the *presentation* differs: the path is printed once per file, results are paged,
//! and pathologically long lines are center-truncated on the match (one `Read` from full content).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;

use grep::matcher::Matcher;

use crate::confirm::{SearchOptions, build_matcher};
use crate::cursor::{self, Cursor, Mode};
use crate::effective_pattern;
use crate::rank::Ranker;
use crate::sort::{self, SortKey, SortSpec};

/// Default matches per page. Generous: an agent pulls the next page cheaply (warm index).
pub const DEFAULT_PAGE_SIZE: usize = 50;
/// Default max rendered columns per line; normal code lines pass untouched, only long/minified
/// lines get center-truncated on the match.
pub const DEFAULT_MAX_COLS: usize = 200;

pub struct CompactOpts {
    pub mode: Mode,
    /// Keyset resume position: render only entries strictly after this `(order, path, lineno)` key
    /// (lineno is ignored in files/count modes; `order` is the sort value — 0 for the default order).
    /// `None` starts from the beginning.
    pub start_after: Option<(i64, String, u64)>,
    pub page_size: usize,
    pub max_cols: usize,
    /// How to order files (`--sort`/`--sortr`); `SortKey::None` keeps the default `(path, lineno)`
    /// order. Reordering only — never changes the match set.
    pub sort: SortSpec,
    /// The ranker for `--sort=weight`; `None` for every other key.
    pub ranker: Option<Ranker>,
    /// The search root, needed to `stat` files for the `modified`/`accessed`/`created` keys.
    pub root: Option<PathBuf>,
}

impl Default for CompactOpts {
    fn default() -> Self {
        Self {
            mode: Mode::Matches,
            start_after: None,
            page_size: DEFAULT_PAGE_SIZE,
            max_cols: DEFAULT_MAX_COLS,
            sort: SortSpec::default(),
            ranker: None,
            root: None,
        }
    }
}

/// Per-file order value (max weight, file mtime, …) keyed by path. Empty for `path`/`none`, where the
/// order falls entirely to the path tiebreak in [`sort::cmp`]. Shared by the compact view and the bare
/// `--sort` path (see `lib::collect_search_sorted`).
pub(crate) fn file_order_map<'a>(
    rows: &[Row<'a>],
    sort: SortSpec,
    ranker: Option<&Ranker>,
    root: Option<&std::path::Path>,
) -> HashMap<&'a str, i64> {
    let mut m: HashMap<&str, i64> = HashMap::new();
    match sort.key {
        SortKey::Weight => {
            if let Some(ranker) = ranker {
                let mut score: HashMap<&str, f32> = HashMap::new();
                for row in rows.iter().filter(|r| r.is_match) {
                    let e = score.entry(row.path).or_insert(f32::MIN);
                    *e = e.max(ranker.score(row.text));
                }
                return score
                    .into_iter()
                    .map(|(p, s)| (p, sort::weight_to_order(s)))
                    .collect();
            }
        }
        SortKey::Modified | SortKey::Accessed | SortKey::Created => {
            if let Some(root) = root {
                for row in rows.iter().filter(|r| r.is_match) {
                    m.entry(row.path)
                        .or_insert_with(|| sort::fs_order_value(sort.key, root, row.path));
                }
            }
        }
        SortKey::Path | SortKey::None => {}
    }
    m
}

/// A rendered page: surface-agnostic `header` + `body`, plus the counts and keyset state each caller
/// (CLI / MCP) needs to mint the "next page" cursor and detect a changed result set.
pub struct Page {
    pub header: String,
    pub body: String,
    pub total_matches: usize,
    pub total_files: usize,
    /// 1-based index of the first/last entry on this page (in matches, or files for `-l`/`-c`); 0 when
    /// empty.
    pub first_index: usize,
    pub last_index: usize,
    /// Keyset key of the last rendered entry, to seed the next cursor; `None` when nothing remains.
    pub last_key: Option<(i64, String, u64)>,
    pub has_more: bool,
    /// Fingerprint of the full result set, for staleness detection across pages.
    pub fingerprint: u64,
}

impl Page {
    /// The cursor that fetches the page after this one, or `None` when this is the last page. Both the
    /// CLI and MCP surfaces mint it the same way; `root_hint` is the only per-surface input (the
    /// resolved root for the CLI, `None` for MCP where the server root is authoritative).
    #[allow(clippy::too_many_arguments)]
    pub fn next_cursor(
        &self,
        mode: Mode,
        pattern: String,
        opts: SearchOptions,
        filter: crate::filter::FilterSpec,
        page_size: usize,
        root_hint: Option<String>,
        sort: SortSpec,
        weights: Option<String>,
    ) -> Option<Cursor> {
        self.has_more.then(|| Cursor {
            mode,
            pattern,
            opts,
            filter,
            page_size,
            last_path: self.last_key.as_ref().map(|(_, p, _)| p.clone()),
            last_lineno: self.last_key.as_ref().map_or(0, |(_, _, l)| *l),
            last_order: self.last_key.as_ref().map_or(0, |(o, _, _)| *o),
            sort,
            weights,
            prev_total: self.total_matches,
            fingerprint: self.fingerprint as u32,
            root_hint,
        })
    }

    /// A note for the caller to surface when resuming a cursor whose result set has since changed
    /// (fingerprint mismatch), or `None`. `prev` is the cursor's `(prev_total, fingerprint)`; the
    /// fingerprint is the low 32 bits, so compare against this page's truncated to match.
    pub fn staleness_note(&self, prev: Option<(usize, u32)>) -> Option<String> {
        match prev {
            Some((prev_total, prev_fp)) if prev_fp != self.fingerprint as u32 => Some(format!(
                "result set changed since the previous page ({prev_total} -> {} matches)",
                self.total_matches
            )),
            _ => None,
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

pub(crate) struct Row<'a> {
    pub(crate) path: &'a str,
    pub(crate) lineno: u64,
    pub(crate) is_match: bool,
    pub(crate) text: &'a str,
    /// Block id: a maximal run of consecutive rows with the same path, not crossing a `--` separator.
    block: usize,
}

/// Reshape `raw` (ripgrep's `path:line:text` output) into a compact page. `pattern`/`opts` are used
/// only to locate the match within long lines for centered truncation. Pagination is keyset
/// (resume after `start_after`), not offset, so it stays correct when the result set shifts between
/// calls; `mode` selects the matches / files (`-l`) / count (`-c`) shape.
pub fn format(raw: &[u8], pattern: &str, opts: SearchOptions, c: CompactOpts) -> Page {
    let text = String::from_utf8_lossy(raw);
    let rows = parse_rows(&text);

    // Per-file order value (weight, mtime, …) driving the sort; empty for the default order, where
    // every value is 0 so the ordering below collapses to the historical (path, lineno) order —
    // byte-identical to before `--sort`.
    let file_order = file_order_map(&rows, c.sort, c.ranker.as_ref(), c.root.as_deref());
    let order_of = |path: &str| file_order.get(path).copied().unwrap_or(0);
    let rev = c.sort.reverse;

    // Keyset paging compares match keys as (order, path, lineno), so match_idx MUST be in that exact
    // order. We cannot trust the input order: collect_search's daemon path emits in index file-id
    // order (fresh-build = `Path::cmp`, which differs from byte-string order at the `/` boundary; and
    // incrementally-added files are appended with the highest id), and the cold-scan path is
    // nondeterministic. Sorting here makes the window, skip count, and last_key self-consistent and
    // the output deterministic regardless of how the bytes arrived.
    let mut match_idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.is_match)
        .map(|(i, _)| i)
        .collect();
    match_idx.sort_by(|&a, &b| {
        sort::cmp(
            (order_of(rows[a].path), rows[a].path, rows[a].lineno),
            (order_of(rows[b].path), rows[b].path, rows[b].lineno),
            rev,
        )
    });
    let total_matches = match_idx.len();

    let mut files: Vec<&str> = rows.iter().filter(|r| r.is_match).map(|r| r.path).collect();
    files.sort_unstable();
    files.dedup();
    // Re-order distinct files by the sort key; equal keys keep the path order from above (sort_by is
    // stable), so the default order stays plain path order.
    files.sort_by(|a, b| sort::cmp((order_of(a), a, 0), (order_of(b), b, 0), rev));
    let total_files = files.len();

    // Fingerprint stays a property of the match *set*, independent of sort order, so it detects
    // add/remove across pages without churning when only the ordering shifts. In the default order
    // `match_idx` is already in `(path, lineno)` order, so the extra sort is only needed when a
    // `--sort` key reordered it.
    let fingerprint = if c.sort.is_noop() {
        cursor::fingerprint(match_idx.iter().map(|&i| (rows[i].path, rows[i].lineno)))
    } else {
        let mut fp_idx = match_idx.clone();
        fp_idx
            .sort_by(|&a, &b| (rows[a].path, rows[a].lineno).cmp(&(rows[b].path, rows[b].lineno)));
        cursor::fingerprint(fp_idx.iter().map(|&i| (rows[i].path, rows[i].lineno)))
    };
    let page_size = c.page_size.max(1);

    match c.mode {
        Mode::Matches => render_matches(
            &rows,
            &match_idx,
            &file_order,
            total_matches,
            total_files,
            pattern,
            opts,
            &c,
            page_size,
            fingerprint,
        ),
        Mode::Files | Mode::Count => render_by_file(
            &rows,
            &match_idx,
            &files,
            &file_order,
            total_matches,
            total_files,
            &c,
            page_size,
            fingerprint,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_matches(
    rows: &[Row],
    match_idx: &[usize],
    file_order: &HashMap<&str, i64>,
    total_matches: usize,
    total_files: usize,
    pattern: &str,
    opts: SearchOptions,
    c: &CompactOpts,
    page_size: usize,
    fingerprint: u64,
) -> Page {
    let order_of = |path: &str| file_order.get(path).copied().unwrap_or(0);
    let key = |i: usize| (order_of(rows[i].path), rows[i].path, rows[i].lineno);
    let rev = c.sort.reverse;
    // Keyset: count matches at or before the resume key, then take the next window.
    let skip = match &c.start_after {
        Some((o, p, l)) => match_idx
            .iter()
            .filter(|&&i| sort::cmp(key(i), (*o, p.as_str(), *l), rev) != Ordering::Greater)
            .count(),
        None => 0,
    };
    let window_matches: Vec<usize> = match_idx
        .iter()
        .copied()
        .skip(skip)
        .take(page_size)
        .collect();
    let rendered = window_matches.len();
    let window: std::collections::HashSet<usize> = window_matches.iter().copied().collect();
    let first_index = if rendered == 0 { 0 } else { skip + 1 };
    let last_index = if rendered == 0 { 0 } else { skip + rendered };
    let has_more = skip + rendered < total_matches;
    let last_key = window_matches.last().map(|&i| {
        (
            order_of(rows[i].path),
            rows[i].path.to_string(),
            rows[i].lineno,
        )
    });

    let header = if total_matches == 0 {
        "[no matches]".to_string()
    } else {
        format!(
            "[matches {first_index}-{last_index} of {total_matches} in {total_files} file{}]",
            plural(total_files)
        )
    };

    // A context row renders iff the nearest match (within its block) is in the window. Collect the
    // rows to show, then emit them in canonical (path, lineno) order — the same order the keyset
    // windows in — so the body is sorted and stable regardless of the input's arrival order.
    let nearest = nearest_match_per_row(rows);
    let mut to_render: Vec<usize> = (0..rows.len())
        .filter(|&i| {
            if rows[i].is_match {
                window.contains(&i)
            } else {
                nearest[i].is_some_and(|m| window.contains(&m))
            }
        })
        .collect();
    to_render.sort_by(|&a, &b| sort::cmp(key(a), key(b), rev));
    let matcher = build_matcher(&effective_pattern(pattern, opts), opts).ok();
    let mut body = String::new();
    let mut cur_path: Option<&str> = None;
    for &i in &to_render {
        let r = &rows[i];
        if cur_path != Some(r.path) {
            body.push_str(r.path);
            body.push('\n');
            cur_path = Some(r.path);
        }
        let center = if r.is_match {
            matcher
                .as_ref()
                .and_then(|m| m.find(r.text.as_bytes()).ok().flatten())
                .map(|mat| mat.start())
        } else {
            None
        };
        let sep = if r.is_match { ':' } else { '-' };
        body.push_str("  ");
        body.push_str(&r.lineno.to_string());
        body.push(sep);
        body.push(' ');
        body.push_str(&truncate_centered(r.text, c.max_cols, center));
        body.push('\n');
    }

    Page {
        header,
        body,
        total_matches,
        total_files,
        first_index,
        last_index,
        has_more,
        last_key,
        fingerprint,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_by_file(
    rows: &[Row],
    match_idx: &[usize],
    files: &[&str],
    file_order: &HashMap<&str, i64>,
    total_matches: usize,
    total_files: usize,
    c: &CompactOpts,
    page_size: usize,
    fingerprint: u64,
) -> Page {
    let order_of = |path: &str| file_order.get(path).copied().unwrap_or(0);
    let rev = c.sort.reverse;
    let skip = match &c.start_after {
        Some((o, p, _)) => files
            .iter()
            .filter(|&&f| {
                sort::cmp((order_of(f), f, 0), (*o, p.as_str(), 0), rev) != Ordering::Greater
            })
            .count(),
        None => 0,
    };
    let window: Vec<&str> = files.iter().copied().skip(skip).take(page_size).collect();
    let rendered = window.len();
    let first_index = if rendered == 0 { 0 } else { skip + 1 };
    let last_index = if rendered == 0 { 0 } else { skip + rendered };
    let has_more = skip + rendered < total_files;
    let last_key = window.last().map(|&p| (order_of(p), p.to_string(), 0));

    let counts: HashMap<&str, usize> = if matches!(c.mode, Mode::Count) {
        let mut m = HashMap::new();
        for &i in match_idx {
            *m.entry(rows[i].path).or_insert(0) += 1;
        }
        m
    } else {
        HashMap::new()
    };

    let body: String = match c.mode {
        Mode::Count => window
            .iter()
            .map(|&p| format!("{p}:{}\n", counts.get(p).copied().unwrap_or(0)))
            .collect(),
        _ => window.iter().map(|&p| format!("{p}\n")).collect(),
    };

    let header = if total_files == 0 {
        "[no matches]".to_string()
    } else if matches!(c.mode, Mode::Count) {
        format!(
            "[count {first_index}-{last_index} of {total_files} file{} \u{b7} {total_matches} match{}]",
            plural(total_files),
            if total_matches == 1 { "" } else { "es" }
        )
    } else {
        format!("[files {first_index}-{last_index} of {total_files}]")
    };

    Page {
        header,
        body,
        total_matches,
        total_files,
        first_index,
        last_index,
        has_more,
        last_key,
        fingerprint,
    }
}

/// One parsed line: `(path, lineno, is_match, text)`.
type Cand<'a> = (&'a str, u64, bool, &'a str);

enum Entry<'a> {
    /// ripgrep's `--` context-block separator.
    Break,
    /// A rendered line with its match (`path:N:text`) and/or context (`path-N-text`) candidate split.
    Row(Option<Cand<'a>>, Option<Cand<'a>>),
}

pub(crate) fn parse_rows(text: &str) -> Vec<Row<'_>> {
    let entries: Vec<Entry> = text
        .lines()
        .filter_map(|line| {
            if line == "--" {
                return Some(Entry::Break);
            }
            let m = split_on(line, b':').map(|(p, n, t)| (p, n, true, t));
            let c = split_on(line, b'-').map(|(p, n, t)| (p, n, false, t));
            match (m, c) {
                (None, None) => None, // not a rendered result line (e.g. a binary-file notice)
                (m, c) => Some(Entry::Row(m, c)),
            }
        })
        .collect();

    // ripgrep's text output is ambiguous: a context line's *text* can hold a `:N:` token (timestamps,
    // URLs, slices) and a match line's path/text a `-N-` token (version dirs like `v1-2-3`). A path
    // never contains the line-number separator, so an "anchor" — a line where only one split is
    // viable — reveals its block's true path; every line in a context block shares that one path.
    let anchors: Vec<Option<&str>> = entries
        .iter()
        .map(|e| match e {
            Entry::Row(Some(m), None) => Some(m.0),
            Entry::Row(None, Some(c)) => Some(c.0),
            _ => None,
        })
        .collect();

    let mut rows = Vec::new();
    let mut block = 0usize;
    let mut prev_path: Option<&str> = None;
    let mut pending_break = false;
    for (i, e) in entries.iter().enumerate() {
        let (path, lineno, is_match, body) = match e {
            Entry::Break => {
                pending_break = true;
                continue;
            }
            Entry::Row(Some(m), None) => *m,
            Entry::Row(None, Some(c)) => *c,
            Entry::Row(Some(m), Some(c)) => {
                // Ambiguous: pick the candidate whose path matches the nearest anchor; if neither
                // does, fall back to the match split (correct whenever the path holds no colon).
                let near = nearest_anchor(&anchors, i);
                if near == Some(c.0) && near != Some(m.0) {
                    *c
                } else {
                    *m
                }
            }
            Entry::Row(None, None) => continue,
        };
        if let Some(pp) = prev_path
            && (pp != path || pending_break)
        {
            block += 1;
        }
        pending_break = false;
        prev_path = Some(path);
        rows.push(Row {
            path,
            lineno,
            is_match,
            text: body,
            block,
        });
    }
    rows
}

/// The path of the nearest anchored line to index `i` (closest by row distance, earlier wins ties).
fn nearest_anchor<'a>(anchors: &[Option<&'a str>], i: usize) -> Option<&'a str> {
    (1..anchors.len()).find_map(|d| {
        i.checked_sub(d)
            .and_then(|j| anchors[j])
            .or_else(|| anchors.get(i + d).copied().flatten())
    })
}

fn split_on(line: &str, sep: u8) -> Option<(&str, u64, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == sep {
            let rest = &bytes[i + 1..];
            let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0 && rest.get(digits) == Some(&sep) {
                let lineno: u64 = line[i + 1..i + 1 + digits].parse().ok()?;
                return Some((&line[..i], lineno, &line[i + 1 + digits + 1..]));
            }
        }
        i += 1;
    }
    None
}

/// For each row, the index of the nearest match row in the same block (by row distance, ties favor
/// the following match). Match rows map to themselves.
fn nearest_match_per_row(rows: &[Row]) -> Vec<Option<usize>> {
    let n = rows.len();
    let mut out = vec![None; n];
    // Nearest match scanning forward, then backward, staying within the same block.
    let mut last: Option<usize> = None;
    for i in 0..n {
        if rows[i].is_match {
            last = Some(i);
        }
        out[i] = last.filter(|&m| rows[m].block == rows[i].block);
    }
    let mut next: Option<usize> = None;
    for i in (0..n).rev() {
        if rows[i].is_match {
            next = Some(i);
        }
        let fwd = next.filter(|&m| rows[m].block == rows[i].block);
        out[i] = match (out[i], fwd) {
            (Some(b), Some(f)) => Some(if i - b <= f - i { b } else { f }),
            (b, f) => b.or(f),
        };
    }
    out
}

/// Truncate `text` to `max_cols` columns, centered on byte offset `center` (the match start) when
/// given, else head-anchored. UTF-8 safe; adds `…` on trimmed sides.
fn truncate_centered(text: &str, max_cols: usize, center: Option<usize>) -> String {
    let char_count = text.chars().count();
    if char_count <= max_cols {
        return text.to_string();
    }
    let center_char = match center {
        Some(byte) => {
            // The match offset comes from a byte search; snap it down to a char boundary so slicing
            // a multi-byte line never panics.
            let mut b = byte.min(text.len());
            while b > 0 && !text.is_char_boundary(b) {
                b -= 1;
            }
            text[..b].chars().count()
        }
        None => 0,
    };
    let before = max_cols / 3;
    let start = center_char
        .saturating_sub(before)
        .min(char_count - max_cols);
    let end = start + max_cols;

    let char_byte = |ci: usize| {
        text.char_indices()
            .nth(ci)
            .map(|(b, _)| b)
            .unwrap_or(text.len())
    };
    let slice = &text[char_byte(start)..char_byte(end)];
    let mut out = String::new();
    if start > 0 {
        out.push('\u{2026}');
    }
    out.push_str(slice);
    if end < char_count {
        out.push('\u{2026}');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &[u8] = b"src/a.rs:1:fn one() {}\n\
src/a.rs:2:fn two() {}\n\
src/b.rs:10:fn three() {}\n";

    fn page(raw: &[u8], pattern: &str, c: CompactOpts) -> Page {
        format(raw, pattern, SearchOptions::default(), c)
    }

    #[test]
    fn groups_by_file_with_counts() {
        let p = page(RAW, "fn", CompactOpts::default());
        assert_eq!(p.total_matches, 3);
        assert_eq!(p.total_files, 2);
        assert!(!p.has_more);
        // Path printed once per file, lines indented under it.
        assert_eq!(p.body.matches("src/a.rs\n").count(), 1);
        assert!(
            p.body
                .contains("src/a.rs\n  1: fn one() {}\n  2: fn two() {}\n")
        );
        assert!(p.body.contains("src/b.rs\n  10: fn three() {}\n"));
        assert!(p.header.contains("matches 1-3 of 3 in 2 files"));
    }

    #[test]
    fn paginates_without_dropping_matches() {
        let p1 = page(
            RAW,
            "fn",
            CompactOpts {
                page_size: 2,
                ..Default::default()
            },
        );
        assert!(p1.has_more);
        assert_eq!((p1.first_index, p1.last_index), (1, 2));
        assert!(p1.body.contains("  1: fn one"));
        assert!(p1.body.contains("  2: fn two"));
        assert!(!p1.body.contains("three"));

        let p2 = page(
            RAW,
            "fn",
            CompactOpts {
                page_size: 2,
                start_after: p1.last_key.clone(),
                ..Default::default()
            },
        );
        assert!(!p2.has_more);
        assert_eq!((p2.first_index, p2.last_index), (3, 3));
        assert!(p2.body.contains("src/b.rs\n  10: fn three"));
        assert!(!p2.body.contains("fn one"));
    }

    #[test]
    fn keyset_survives_unsorted_input_without_dropping_matches() {
        // The daemon emits matches in index file-id order (Path::cmp / append order), which is NOT
        // byte-string order: `src/a/b.rs` sorts before `src/a.rs` as a Path but after it as a string.
        // Feed that daemon-style order and walk every page; all three matches must be reachable.
        const UNSORTED: &[u8] = b"src/a/b.rs:1:fn x\n\
src/a.rs:1:fn y\n\
src/ab.rs:1:fn z\n";
        let mut seen = Vec::new();
        let mut start_after = None;
        for _ in 0..5 {
            let p = page(
                UNSORTED,
                "fn",
                CompactOpts {
                    page_size: 1,
                    start_after: start_after.clone(),
                    ..Default::default()
                },
            );
            assert_eq!(p.total_matches, 3);
            for line in p.body.lines().filter(|l| l.starts_with("  ")) {
                seen.push(line.to_string());
            }
            if !p.has_more {
                break;
            }
            start_after = p.last_key.clone();
        }
        // Every match rendered exactly once, in canonical path order, none dropped or duplicated.
        assert_eq!(seen, vec!["  1: fn y", "  1: fn x", "  1: fn z"]);
    }

    #[test]
    fn keyset_resume_after_last_key() {
        // Resume mid-file: page_size 1 over two matches in src/a.rs.
        let p1 = page(
            RAW,
            "fn",
            CompactOpts {
                page_size: 1,
                ..Default::default()
            },
        );
        assert_eq!(p1.last_key, Some((0, "src/a.rs".to_string(), 1)));
        let p2 = page(
            RAW,
            "fn",
            CompactOpts {
                page_size: 1,
                start_after: p1.last_key.clone(),
                ..Default::default()
            },
        );
        assert!(p2.body.contains("  2: fn two"));
        assert!(!p2.body.contains("fn one"));
        assert_eq!((p2.first_index, p2.last_index), (2, 2));
    }

    #[test]
    fn files_mode_lists_paths() {
        let p = page(
            RAW,
            "fn",
            CompactOpts {
                mode: Mode::Files,
                ..Default::default()
            },
        );
        assert_eq!(p.total_files, 2);
        assert!(p.header.contains("files 1-2 of 2"));
        assert!(p.body.contains("src/a.rs\n"));
        assert!(p.body.contains("src/b.rs\n"));
        assert!(!p.body.contains("fn one")); // no match text in files mode
    }

    #[test]
    fn count_mode_tallies_per_file() {
        let p = page(
            RAW,
            "fn",
            CompactOpts {
                mode: Mode::Count,
                ..Default::default()
            },
        );
        assert!(p.body.contains("src/a.rs:2\n"));
        assert!(p.body.contains("src/b.rs:1\n"));
        assert!(p.header.contains("count 1-2 of 2 files"));
        assert!(p.header.contains("3 matches"));
    }

    #[test]
    fn fingerprint_stable_across_calls_and_pages() {
        let full = page(RAW, "fn", CompactOpts::default());
        let paged = page(
            RAW,
            "fn",
            CompactOpts {
                page_size: 1,
                ..Default::default()
            },
        );
        assert_eq!(full.fingerprint, paged.fingerprint);
        assert_ne!(full.fingerprint, 0);
    }

    #[test]
    fn truncates_long_line_centered_on_match() {
        let long = format!("src/x.rs:1:{}NEEDLE{}\n", "a".repeat(400), "b".repeat(400));
        let p = page(
            long.as_bytes(),
            "NEEDLE",
            CompactOpts {
                max_cols: 60,
                ..Default::default()
            },
        );
        let line = p.body.lines().find(|l| l.contains("NEEDLE")).unwrap();
        assert!(line.contains('\u{2026}'), "expected ellipsis: {line}");
        assert!(line.chars().count() < 100);
    }

    #[test]
    fn truncates_long_multibyte_line_without_panicking() {
        let long = format!(
            "src/x.rs:1:{}café NEEDLE {}\n",
            "é".repeat(300),
            "ü".repeat(300)
        );
        let p = page(
            long.as_bytes(),
            "NEEDLE",
            CompactOpts {
                max_cols: 50,
                ..Default::default()
            },
        );
        let line = p.body.lines().find(|l| l.contains("NEEDLE")).unwrap();
        assert!(line.contains('\u{2026}'));
        assert!(line.chars().count() < 90);
    }

    #[test]
    fn empty_input_has_no_body() {
        let p = page(b"", "fn", CompactOpts::default());
        assert_eq!(p.total_matches, 0);
        assert!(!p.has_more);
        assert!(p.body.is_empty());
        assert_eq!(p.header, "[no matches]");
    }

    #[test]
    fn context_lines_attach_to_their_match_and_dont_count() {
        // ripgrep `-C` shape: `path-N-` context lines, `path:N:` match lines, `--` block separators.
        let raw = b"f.rs-4-before a\n\
f.rs:5:MATCH a\n\
f.rs-6-after a\n\
--\n\
f.rs-9-before b\n\
f.rs:10:MATCH b\n\
f.rs-11-after b\n";
        let p = page(
            raw,
            "MATCH",
            CompactOpts {
                page_size: 1,
                ..Default::default()
            },
        );
        // Two matches in one file; context lines never inflate the count.
        assert_eq!(p.total_matches, 2);
        assert_eq!(p.total_files, 1);
        assert!(p.has_more);
        // Page 1 carries match a with its surrounding context, and nothing from match b's block.
        assert!(p.body.contains("  5: MATCH a"));
        assert!(p.body.contains("  4- before a"));
        assert!(p.body.contains("  6- after a"));
        assert!(!p.body.contains("MATCH b"));
        assert!(!p.body.contains("before b"));
    }

    #[test]
    fn context_line_with_colon_digits_in_text_is_not_misparsed() {
        // A before-context line whose text holds a `:N:` token (timestamp) must stay context, not be
        // mistaken for a match — otherwise it inflates counts and invents a phantom file.
        let raw = b"f.txt-2-log at 12:34:56 here\n\
f.txt:3:TARGET match\n\
f.txt-4-after line\n";
        let p = page(raw, "TARGET", CompactOpts::default());
        assert_eq!(p.total_matches, 1, "{}", p.body);
        assert_eq!(p.total_files, 1, "{}", p.body);
        assert!(p.body.contains("f.txt\n  2- log at 12:34:56 here"));
        assert!(p.body.contains("  3: TARGET match"));
        assert!(!p.body.contains("12\n"));
    }

    #[test]
    fn colon_separator_wins_over_hyphen_in_path() {
        // A real `:N:` must split even when the path/text contains hyphens (and digits around them).
        let p = page(
            b"src/a-b-2.rs:42:let x-1 = y-2;\n",
            "let",
            CompactOpts::default(),
        );
        assert!(
            p.body.contains("src/a-b-2.rs\n  42: let x-1 = y-2;"),
            "{}",
            p.body
        );
        assert_eq!(p.total_matches, 1);
    }

    #[test]
    fn start_after_past_end_is_empty() {
        let p = page(
            RAW,
            "fn",
            CompactOpts {
                start_after: Some((0, "zzz".to_string(), 0)),
                ..Default::default()
            },
        );
        assert!(!p.has_more);
        assert_eq!(p.first_index, 0);
        assert_eq!(p.last_index, 0);
        assert!(p.body.is_empty());
        assert_eq!(p.last_key, None);
    }

    #[test]
    fn sort_weight_orders_and_pages_by_descending_weight() {
        use crate::rank;
        // a.rs matches the low-weight branch (earth, 0.3), b.rs the high-weight one (world, 0.7).
        let raw = b"src/a.rs:1:hello earth\nsrc/b.rs:1:hello world\n";
        let mk = |start_after| {
            let ranking = rank::parse(
                "world<w1>|earth<w2>",
                Some("w1:0.7,w2:0.3"),
                SearchOptions::default(),
            )
            .unwrap();
            format(
                raw,
                &ranking.plain,
                SearchOptions::default(),
                CompactOpts {
                    page_size: 1,
                    start_after,
                    sort: SortSpec {
                        key: SortKey::Weight,
                        reverse: false,
                    },
                    ranker: ranking.ranker,
                    ..Default::default()
                },
            )
        };
        // Page 1 is the higher-weighted file; the order value rides in the keyset key.
        let p1 = mk(None);
        assert_eq!(p1.total_matches, 2);
        assert!(p1.body.contains("src/b.rs"), "{}", p1.body);
        assert!(!p1.body.contains("src/a.rs"));
        assert!(p1.has_more);
        assert_eq!(
            p1.last_key.as_ref().map(|(o, _, _)| *o),
            Some(sort::weight_to_order(0.7))
        );
        // Page 2 resumes after it and yields the lower-weighted file, nothing dropped.
        let p2 = mk(p1.last_key.clone());
        assert!(p2.body.contains("src/a.rs"), "{}", p2.body);
        assert!(!p2.body.contains("src/b.rs"));
        assert!(!p2.has_more);
    }
}
