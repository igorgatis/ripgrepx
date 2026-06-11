//! Token-savings presentation: reshape ripgrep's `path:line:text` stream into a compact, paginated
//! view for agents. This is a pure transform over already-rendered output — matching stays 100%
//! ripgrep (see `confirm`). The contract is deliberately weaker than the byte-for-byte CLI: the
//! *match set* is identical to `rg` (nothing is ever dropped — pagination is the only volume
//! control), but the *presentation* differs: the path is printed once per file, results are paged,
//! and pathologically long lines are center-truncated on the match (one `Read` from full content).

use grep::matcher::Matcher;

use crate::confirm::{SearchOptions, build_matcher};
use crate::effective_pattern;

/// Default matches per page. Generous: an agent pulls the next page cheaply (warm index).
pub const DEFAULT_PAGE_SIZE: usize = 50;
/// Default max rendered columns per line; normal code lines pass untouched, only long/minified
/// lines get center-truncated on the match.
pub const DEFAULT_MAX_COLS: usize = 200;

pub struct CompactOpts {
    /// 1-based page number.
    pub page: usize,
    pub page_size: usize,
    pub max_cols: usize,
}

impl Default for CompactOpts {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
            max_cols: DEFAULT_MAX_COLS,
        }
    }
}

/// A rendered page: surface-agnostic `header` + `body`, plus counts so each caller (CLI / MCP) can
/// compose its own "next page" hint.
pub struct Page {
    pub header: String,
    pub body: String,
    pub total_matches: usize,
    pub total_files: usize,
    /// 1-based page actually rendered (clamped into range).
    pub page: usize,
    pub pages: usize,
}

impl Page {
    /// Whether a further page exists after the one rendered.
    pub fn has_more(&self) -> bool {
        self.page < self.pages
    }
}

struct Row<'a> {
    path: &'a str,
    lineno: u64,
    is_match: bool,
    text: &'a str,
    /// Block id: a maximal run of consecutive rows with the same path, not crossing a `--` separator.
    block: usize,
}

/// Reshape `raw` (ripgrep's `path:line:text` output) into a compact paginated page. `pattern`/`opts`
/// are used only to locate the match within long lines for centered truncation.
pub fn format(raw: &[u8], pattern: &str, opts: SearchOptions, c: CompactOpts) -> Page {
    let text = String::from_utf8_lossy(raw);
    let rows = parse_rows(&text);

    let match_idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.is_match)
        .map(|(i, _)| i)
        .collect();
    let total_matches = match_idx.len();

    let mut files: Vec<&str> = rows.iter().filter(|r| r.is_match).map(|r| r.path).collect();
    files.sort_unstable();
    files.dedup();
    let total_files = files.len();

    let page_size = c.page_size.max(1);
    let pages = total_matches.div_ceil(page_size); // 0 when there are no matches
    let page = c.page.max(1).min(pages.max(1));

    let header = format!(
        "[page {page}/{} \u{b7} {total_matches} match{} in {total_files} file{}]",
        pages.max(1),
        if total_matches == 1 { "" } else { "es" },
        if total_files == 1 { "" } else { "s" },
    );

    if total_matches == 0 {
        return Page {
            header,
            body: String::new(),
            total_matches,
            total_files,
            page: 1,
            pages: 0,
        };
    }

    let start = (page - 1) * page_size;
    let window: std::collections::HashSet<usize> = match_idx
        .iter()
        .copied()
        .skip(start)
        .take(page_size)
        .collect();

    // A context row renders iff the nearest match (within its block) is in the window.
    let nearest = nearest_match_per_row(&rows);
    let render: Vec<bool> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if r.is_match {
                window.contains(&i)
            } else {
                nearest[i].is_some_and(|m| window.contains(&m))
            }
        })
        .collect();

    let matcher = build_matcher(&effective_pattern(pattern, opts), opts).ok();
    let mut body = String::new();
    let mut cur_path: Option<&str> = None;
    for (i, r) in rows.iter().enumerate() {
        if !render[i] {
            continue;
        }
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
        page,
        pages,
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

fn parse_rows(text: &str) -> Vec<Row<'_>> {
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

    #[test]
    fn groups_by_file_with_counts() {
        let p = format(RAW, "fn", SearchOptions::default(), CompactOpts::default());
        assert_eq!(p.total_matches, 3);
        assert_eq!(p.total_files, 2);
        assert_eq!(p.pages, 1);
        // Path printed once per file, lines indented under it.
        assert_eq!(p.body.matches("src/a.rs\n").count(), 1);
        assert!(
            p.body
                .contains("src/a.rs\n  1: fn one() {}\n  2: fn two() {}\n")
        );
        assert!(p.body.contains("src/b.rs\n  10: fn three() {}\n"));
        assert!(p.header.contains("3 matches in 2 files"));
    }

    #[test]
    fn paginates_without_dropping_matches() {
        let opts = CompactOpts {
            page: 1,
            page_size: 2,
            max_cols: DEFAULT_MAX_COLS,
        };
        let p1 = format(RAW, "fn", SearchOptions::default(), opts);
        assert_eq!(p1.pages, 2);
        assert!(p1.has_more());
        assert!(p1.body.contains("  1: fn one"));
        assert!(p1.body.contains("  2: fn two"));
        assert!(!p1.body.contains("three"));

        let opts2 = CompactOpts {
            page: 2,
            page_size: 2,
            max_cols: DEFAULT_MAX_COLS,
        };
        let p2 = format(RAW, "fn", SearchOptions::default(), opts2);
        assert!(!p2.has_more());
        assert!(p2.body.contains("src/b.rs\n  10: fn three"));
        assert!(!p2.body.contains("fn one"));
    }

    #[test]
    fn truncates_long_line_centered_on_match() {
        let long = format!("src/x.rs:1:{}NEEDLE{}\n", "a".repeat(400), "b".repeat(400));
        let opts = CompactOpts {
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
            max_cols: 60,
        };
        let p = format(long.as_bytes(), "NEEDLE", SearchOptions::default(), opts);
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
        let opts = CompactOpts {
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
            max_cols: 50,
        };
        let p = format(long.as_bytes(), "NEEDLE", SearchOptions::default(), opts);
        let line = p.body.lines().find(|l| l.contains("NEEDLE")).unwrap();
        assert!(line.contains('\u{2026}'));
        assert!(line.chars().count() < 90);
    }

    #[test]
    fn empty_input_has_no_body() {
        let p = format(b"", "fn", SearchOptions::default(), CompactOpts::default());
        assert_eq!(p.total_matches, 0);
        assert_eq!(p.pages, 0);
        assert!(p.body.is_empty());
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
        let opts = CompactOpts {
            page: 1,
            page_size: 1,
            max_cols: DEFAULT_MAX_COLS,
        };
        let p = format(raw, "MATCH", SearchOptions::default(), opts);
        // Two matches in one file; context lines never inflate the count.
        assert_eq!(p.total_matches, 2);
        assert_eq!(p.total_files, 1);
        assert_eq!(p.pages, 2);
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
        let opts = CompactOpts {
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
            max_cols: DEFAULT_MAX_COLS,
        };
        let p = format(raw, "TARGET", SearchOptions::default(), opts);
        assert_eq!(p.total_matches, 1, "{}", p.body);
        assert_eq!(p.total_files, 1, "{}", p.body);
        assert!(p.body.contains("f.txt\n  2- log at 12:34:56 here"));
        assert!(p.body.contains("  3: TARGET match"));
        assert!(!p.body.contains("12\n"));
    }

    #[test]
    fn colon_separator_wins_over_hyphen_in_path() {
        // A real `:N:` must split even when the path/text contains hyphens (and digits around them).
        let p = format(
            b"src/a-b-2.rs:42:let x-1 = y-2;\n",
            "let",
            SearchOptions::default(),
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
    fn page_past_end_clamps_to_last_page() {
        let opts = CompactOpts {
            page: 99,
            page_size: 2,
            max_cols: DEFAULT_MAX_COLS,
        };
        let p = format(RAW, "fn", SearchOptions::default(), opts);
        assert_eq!(p.page, 2);
        assert_eq!(p.pages, 2);
        assert!(!p.has_more());
        assert!(p.body.contains("fn three"));
    }
}
