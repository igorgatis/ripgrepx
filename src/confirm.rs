//! The confirm step: run ripgrep's own engine over a set of files and emit `path:line:text`.
//!
//! This is where correctness lives — it is literally ripgrep's matcher, searcher, and printer, so
//! output is byte-for-byte `rg`'s. We deliberately use `BinaryDetection::quit`, which reproduces
//! ripgrep's *recursive-traversal* binary behavior (search until the first NUL, then stop) — not
//! the explicit-file-argument behavior the `rg` binary would apply to a candidate list. See
//! `docs/querying.md` (the confirm step) and `docs/indexing.md` (binary files).

use std::path::Path;

use anyhow::Result;
use grep::printer::StandardBuilder;
use grep::regex::{RegexMatcher, RegexMatcherBuilder};
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder};
use ignore::WalkState;
use rayon::prelude::*;
use termcolor::NoColor;

/// User-facing search options (the subset of ripgrep flags rgx threads through so far). These
/// travel over the daemon protocol and drive both query extraction and the confirm step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_insensitive: bool,
    pub multi_line: bool,
    pub dot_matches_new_line: bool,
    /// `-w`: match only at word boundaries.
    pub word: bool,
    /// `-F`: treat the pattern as a literal string.
    pub fixed_strings: bool,
    /// `-v`: emit non-matching lines instead of matching ones.
    pub invert: bool,
    /// `--hidden`: also search hidden files/dirs (default skips them).
    pub hidden: bool,
    /// `--no-ignore`: don't honor `.gitignore`/`.ignore`/`.rgignore`/excludes.
    pub no_ignore: bool,
    /// `-B` / `-C`: lines of leading context.
    pub before_context: usize,
    /// `-A` / `-C`: lines of trailing context.
    pub after_context: usize,
}

/// Files searched per parallel batch; bounds peak memory and lets results stream out for huge
/// result sets instead of buffering the whole corpus.
const BATCH: usize = 512;

pub(crate) fn build_matcher(pattern: &str, opts: SearchOptions) -> Result<RegexMatcher> {
    Ok(RegexMatcherBuilder::new()
        .case_insensitive(opts.case_insensitive)
        .multi_line(opts.multi_line)
        .dot_matches_new_line(opts.dot_matches_new_line)
        .word(opts.word)
        .build(pattern)?)
}

fn build_searcher(opts: SearchOptions) -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0))
        .multi_line(opts.multi_line)
        .invert_match(opts.invert)
        .before_context(opts.before_context)
        .after_context(opts.after_context)
        .build()
}

/// The path to print for `path`: relative to `root` (so output matches `rg`'s cwd-relative paths and
/// cursors stay small) when `path` is under it, else `path` unchanged. The file is still read from the
/// real `path`.
fn display_path<'a>(path: &'a Path, root: &Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

/// Render one file's matches into `buf` (cleared first), exactly as `rg` would print them. The file is
/// read from `path` but printed relative to `root`.
fn search_one(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &Path,
    root: &Path,
    buf: &mut Vec<u8>,
) {
    buf.clear();
    let mut printer = StandardBuilder::new().build(NoColor::new(&mut *buf));
    let shown = display_path(path, root);
    let _ = searcher.search_path(matcher, path, printer.sink_with_path(matcher, shown));
}

/// Search a known `paths` set for `pattern` (already made effective — escaped for `-F` by the
/// caller), emitting each file's rendered output via `emit`, in the order the paths are given
/// (callers pass them sorted, so output is deterministic). Paths are printed relative to `root`.
/// Memory stays bounded to one batch.
pub fn search_streaming(
    pattern: &str,
    paths: &[&Path],
    root: &Path,
    opts: SearchOptions,
    mut emit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let matcher = build_matcher(pattern, opts)?;
    for batch in paths.chunks(BATCH) {
        let chunks: Vec<Vec<u8>> = batch
            .par_iter()
            .map_init(
                || (build_searcher(opts), Vec::new()),
                |(searcher, buf), path| {
                    search_one(searcher, &matcher, path, root, buf);
                    std::mem::take(buf)
                },
            )
            .collect();
        for c in &chunks {
            emit(c)?;
        }
    }
    Ok(())
}

/// Pipelined full-tree scan, matching ripgrep's own model: a parallel `ignore` walk feeds per-file
/// search, and each thread streams its output through `sink` as files are discovered — no upfront
/// walk-then-search split, no sort. Output order is therefore nondeterministic (like `rg` without
/// `--sort`). Used for fallback queries (no usable trigram) and the daemon's cold start, entirely
/// in-process — ripgrep's engine is linked in, so no `rg` binary is ever required.
pub fn full_scan(
    root: &Path,
    pattern: &str,
    opts: SearchOptions,
    filter: &crate::filter::FilterSpec,
    sink: impl Fn(&[u8]) + Sync,
) -> Result<()> {
    let matcher = build_matcher(pattern, opts)?;
    let matcher = &matcher;
    let sink = &sink;
    let mut wb = crate::index::walk_builder_for(root, opts.hidden, opts.no_ignore);
    filter.configure_walk(root, &mut wb)?;
    wb.build_parallel().run(|| {
        // Build the searcher and printer once per walk thread (not per file): for a match-everything
        // query over tens of thousands of files, per-file printer construction dominates otherwise.
        let mut searcher = build_searcher(opts);
        let mut printer = StandardBuilder::new().build(NoColor::new(Vec::<u8>::new()));
        Box::new(move |res| {
            if let Ok(entry) = res
                && entry.file_type().is_some_and(|t| t.is_file())
            {
                let path = entry.path();
                let shown = display_path(path, root);
                let _ = searcher.search_path(matcher, path, printer.sink_with_path(matcher, shown));
                let buf = printer.get_mut().get_mut();
                if !buf.is_empty() {
                    sink(buf);
                    buf.clear();
                }
            }
            WalkState::Continue
        })
    });
    Ok(())
}

/// Collecting convenience over [`search_streaming`] (used by tests and small in-process callers).
pub fn search(pattern: &str, paths: &[&Path], root: &Path, opts: SearchOptions) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    search_streaming(pattern, paths, root, opts, |c| {
        out.extend_from_slice(c);
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_path_line_text() {
        let tmp = std::env::temp_dir().join(format!("rgx_confirm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.join("f.txt");
        std::fs::write(&p, b"alpha\nbeta NEEDLE gamma\ndelta\n").unwrap();

        let out = search("NEEDLE", &[p.as_path()], &tmp, SearchOptions::default()).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with("f.txt:2:beta NEEDLE gamma"),
            "got: {text:?}"
        );
        assert!(!text.contains("alpha"));
        assert!(
            !text.contains(tmp.to_str().unwrap()),
            "path should be relative: {text:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn invert_emits_non_matching_lines() {
        let tmp = std::env::temp_dir().join(format!("rgx_invert_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.join("f.txt");
        std::fs::write(&p, b"alpha\nbeta NEEDLE gamma\ndelta\n").unwrap();

        let opts = SearchOptions {
            invert: true,
            ..Default::default()
        };
        let out = search("NEEDLE", &[p.as_path()], &tmp, opts).unwrap();
        let text = String::from_utf8(out).unwrap();
        // `-v` keeps the lines that do NOT match, dropping the NEEDLE line.
        assert!(text.contains("f.txt:1:alpha"), "got: {text:?}");
        assert!(text.contains("f.txt:3:delta"), "got: {text:?}");
        assert!(!text.contains("NEEDLE"), "got: {text:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
