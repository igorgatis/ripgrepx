//! Differential walk parity: on randomly generated trees, `rgx::index::walk_files(root)` must equal
//! `rg --files` run from `root`, byte-for-byte. This is the broad safety net the `.rgignore` bug
//! slipped past — it exercises the real ripgrep binary as the oracle over `.gitignore`/`.ignore`/
//! `.rgignore`/hidden/nested/negated layouts, instead of hand-baked expectations.
//!
//! Skips when `rg` isn't on PATH (see `tests/common`); CI runs it via the pinned ripgrep in mise.toml.

mod common;

use std::fs;
use std::path::Path;

use rgx::index::walk_files_for;

const DIRS: &[&str] = &["a", "b", "src", "sub", "build", "pkg"];
const STEMS: &[&str] = &["foo", "bar", "baz", "main", "mod"];
const EXTS: &[&str] = &["txt", "rs", "log", "tmp", "md"];
const IGN_FILES: &[&str] = &[".gitignore", ".ignore", ".rgignore"];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &'a [&str]) -> &'a str {
        xs[self.below(xs.len())]
    }
    fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
}

fn rel_file(rng: &mut Rng) -> String {
    let depth = rng.below(3); // 0..=2 directory components
    let mut parts: Vec<&str> = (0..depth).map(|_| rng.pick(DIRS)).collect();
    let file = format!("{}.{}", rng.pick(STEMS), rng.pick(EXTS));
    parts.push(&file);
    parts.join("/")
}

fn pattern(rng: &mut Rng) -> String {
    let body = match rng.below(4) {
        0 => format!("*.{}", rng.pick(EXTS)),
        1 => format!("{}.{}", rng.pick(STEMS), rng.pick(EXTS)),
        2 => format!("{}/", rng.pick(DIRS)),
        _ => rng.pick(STEMS).to_string(),
    };
    if rng.chance(1, 5) {
        format!("!{body}")
    } else {
        body
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

/// Build a random tree; return a human-readable description for failure diagnostics.
fn gen_tree(rng: &mut Rng, root: &Path) -> String {
    let mut desc = String::new();
    if rng.chance(3, 5) {
        fs::create_dir_all(root.join(".git")).unwrap();
        desc.push_str(".git/ (gitignore active)\n");
        if rng.chance(2, 5) {
            let lines: Vec<String> = (0..(1 + rng.below(2))).map(|_| pattern(rng)).collect();
            write(root, ".git/info/exclude", &(lines.join("\n") + "\n"));
            desc.push_str(&format!(".git/info/exclude: {}\n", lines.join(" ")));
        }
    }
    for _ in 0..(8 + rng.below(9)) {
        let f = rel_file(rng);
        write(root, &f, "x\n");
    }
    for _ in 0..rng.below(4) {
        let where_ = if rng.chance(1, 3) {
            format!("{}/{}", rng.pick(DIRS), rng.pick(IGN_FILES))
        } else {
            rng.pick(IGN_FILES).to_string()
        };
        let lines: Vec<String> = (0..(1 + rng.below(3))).map(|_| pattern(rng)).collect();
        write(root, &where_, &(lines.join("\n") + "\n"));
        desc.push_str(&format!("{where_}: {}\n", lines.join(" ")));
    }
    desc
}

fn ours(root: &Path) -> Vec<String> {
    ours_for(root, false, false)
}

fn ours_for(root: &Path, hidden: bool, no_ignore: bool) -> Vec<String> {
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
fn walk_matches_rg_on_random_trees() {
    if common::rg().is_none() {
        eprintln!("rg not on PATH; skipping differential walk-parity fuzz");
        return;
    }
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for seed in 0..200u32 {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let desc = gen_tree(&mut rng, root);
        let ours = ours(root);
        let theirs = common::rg_files(root);
        assert_eq!(
            ours, theirs,
            "seed {seed}: rgx walk != `rg --files`\n--- tree ---\n{desc}--- rgx ---\n{ours:#?}\n--- rg ---\n{theirs:#?}"
        );
    }
}

#[test]
fn hidden_and_no_ignore_match_rg() {
    if common::rg().is_none() {
        eprintln!("rg not on PATH; skipping --hidden/--no-ignore parity");
        return;
    }
    // A tree mixing visible, hidden, and ignored files (`.ignore`, so no `.git` complication), at the
    // root and in a subdir. rgx's configured walk must match `rg --files` for every flag combo.
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    write(r, "visible.txt", "x\n");
    write(r, "sub/visible2.rs", "x\n");
    write(r, ".hidden.txt", "x\n");
    write(r, "sub/.hidden2.rs", "x\n");
    write(r, "skip.log", "x\n");
    write(r, "sub/skip2.log", "x\n");
    write(r, ".ignore", "*.log\n");

    for &(hidden, no_ignore) in &[(false, false), (true, false), (false, true), (true, true)] {
        let mut extra: Vec<&str> = Vec::new();
        if hidden {
            extra.push("--hidden");
        }
        if no_ignore {
            extra.push("--no-ignore");
        }
        let ours = ours_for(r, hidden, no_ignore);
        let theirs = common::rg_files_with(r, &extra);
        assert_eq!(
            ours,
            theirs,
            "hidden={hidden} no_ignore={no_ignore}: rgx walk != `rg --files {}`\n--- rgx ---\n{ours:#?}\n--- rg ---\n{theirs:#?}",
            extra.join(" ")
        );
    }
}
