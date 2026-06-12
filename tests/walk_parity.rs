//! Walk parity: `rgx::index::walk_files` must yield exactly the files `rg` would for the same tree.
//! Confirm searches the candidate list directly without re-applying ignore rules, so an extra file is
//! a phantom match and a missing one is a dropped match — the walk *is* the ignore contract.
//!
//! These mirror the scenarios in ripgrep's own `ignore` crate (`walk.rs` tests, same author), but
//! drive rgx's `walk_builder` — its crate-default config plus the `.rgignore` name the `rg` binary
//! adds — which is the surface rgx owns. Expected sets are baked from ripgrep's documented behavior
//! (verified against `rg` 15.1.0), so the suite needs no `rg` binary. See `docs/indexing.md`.
//!
//! Caveat: the tests that create a `.git` enable ripgrep's global-gitignore stack, so a developer's
//! own global gitignore (`core.excludesFile` / `~/.config/git/ignore`) matching a fixture name (e.g.
//! `*.txt`/`*.rs`) could perturb a result — rare in practice. The differential fuzzer in
//! `walk_parity_fuzz.rs` is the authoritative, environment-immune check (it diffs against the real
//! `rg`, so any global ignore applies to both sides and cancels); these fixtures are the fast,
//! `rg`-free floor that pinpoints which rule broke.

use std::fs;
use std::path::Path;

use rgx::index::{walk_files, walk_files_for};

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

/// Files rgx would index under `root`, as sorted `/`-separated paths relative to `root`.
fn walked(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = walk_files(root)
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    v.sort();
    v
}

/// A `.git` dir is enough to activate the gitignore stack (the crate only checks for its presence).
fn git_init(root: &Path) {
    fs::create_dir(root.join(".git")).unwrap();
}

#[test]
fn honors_every_ignore_source_like_ripgrep() {
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    git_init(r); // activates .gitignore / .git-info-exclude
    write(r, "keep.txt", "x");
    write(r, "by_gitignore.txt", "x");
    write(r, "by_ignore.txt", "x");
    write(r, "by_rgignore.txt", "x");
    write(r, ".gitignore", "by_gitignore.txt\n");
    write(r, ".ignore", "by_ignore.txt\n");
    write(r, ".rgignore", "by_rgignore.txt\n"); // the one rgx adds on top of the crate
    // The ignore files are dotfiles, so they're hidden-skipped too; only keep.txt survives.
    assert_eq!(walked(r), vec!["keep.txt"]);
}

#[test]
fn gitignore_is_inert_without_a_git_dir() {
    // ripgrep's surprising rule: a `.gitignore` does nothing outside a git repo.
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    write(r, "a.txt", "x");
    write(r, "b.txt", "x");
    write(r, ".gitignore", "b.txt\n"); // no .git -> not applied
    assert_eq!(walked(r), vec!["a.txt", "b.txt"]);
}

#[test]
fn parent_gitignore_applies_in_subdirs() {
    // The root .gitignore is read for files in subdirectories (parent walk-up to the git root).
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    git_init(r);
    write(r, ".gitignore", "*.log\n");
    write(r, "src/a.rs", "x");
    write(r, "src/debug.log", "x");
    assert_eq!(walked(r), vec!["src/a.rs"]);
}

#[test]
fn hidden_files_are_skipped() {
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    write(r, "visible.txt", "x");
    write(r, ".hidden.txt", "x");
    assert_eq!(walked(r), vec!["visible.txt"]);
}

/// Files rgx walks with the `--hidden`/`--no-ignore` toggles, sorted relative paths.
fn walked_for(root: &Path, hidden: bool, no_ignore: bool) -> Vec<String> {
    let mut v: Vec<String> = walk_files_for(root, hidden, no_ignore)
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    v.sort();
    v
}

#[test]
fn hidden_flag_includes_dotfiles() {
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    write(r, "visible.txt", "x");
    write(r, ".hidden.txt", "x");
    // Default skips the dotfile; `--hidden` includes it (the rest of the walk is unchanged).
    assert_eq!(walked_for(r, false, false), vec!["visible.txt"]);
    assert_eq!(
        walked_for(r, true, false),
        vec![".hidden.txt", "visible.txt"]
    );
}

#[test]
fn no_ignore_flag_includes_ignored_files() {
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    write(r, "keep.txt", "x");
    write(r, "skip.log", "x");
    write(r, ".ignore", "*.log\n"); // `.ignore` applies without a `.git` dir
    // Default honors `.ignore`; `--no-ignore` re-includes the ignored file. The `.ignore` dotfile
    // stays hidden in both (no `--hidden`).
    assert_eq!(walked_for(r, false, false), vec!["keep.txt"]);
    assert_eq!(walked_for(r, false, true), vec!["keep.txt", "skip.log"]);
}

#[test]
fn ignore_whitelist_overrides_gitignore() {
    // Precedence: `.ignore` outranks `.gitignore`, so its `!foo` re-includes a gitignored file.
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    git_init(r);
    write(r, ".gitignore", "foo.txt\n");
    write(r, ".ignore", "!foo.txt\n");
    write(r, "foo.txt", "x");
    assert_eq!(walked(r), vec!["foo.txt"]);
}
