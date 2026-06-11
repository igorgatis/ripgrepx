//! Candidate-selection soundness over real files: build an index of this repo, then for a set of
//! patterns confirm that every file the real `regex` engine matches is present in the candidate set
//! (no missed files). The oracle scans the same files the index walked.

use std::path::PathBuf;

use regex::Regex;
use rgx::index::Index;
use rgx::query::{Options, Query};

fn indexed_files(root: &str) -> Vec<PathBuf> {
    // Mirror the index walk (ignore-aware) so the oracle and the index see the same files.
    let mut files = Vec::new();
    for r in ignore::WalkBuilder::new(root).build().flatten() {
        if r.file_type().is_some_and(|t| t.is_file()) {
            files.push(r.into_path());
        }
    }
    files
}

#[test]
fn candidates_never_miss_a_matching_file() {
    let root = env!("CARGO_MANIFEST_DIR");
    let idx = Index::build(root);
    let files = indexed_files(root);

    let patterns = [
        "Trigram",
        "candidates",
        "RoaringBitmap",
        "Query|Index|Options",
        "fn .*build",
        "BinaryDetection",
        "fjall",
        "EXPORT_SYMBOL_GPL",
    ];

    for p in patterns {
        let re = Regex::new(p).unwrap();
        let query = Query::for_pattern(p, Options::default());
        // candidates() always returns a (sound) superset — for fallback patterns, every file.
        let cand: Vec<PathBuf> = idx
            .candidates(&query)
            .into_iter()
            .map(|p| p.to_path_buf())
            .collect();

        for f in &files {
            let Ok(bytes) = std::fs::read(f) else {
                continue;
            };
            // skip files the index treats as binary-from-start (NUL in first 1KB)
            if memchr_nul(&bytes) {
                continue;
            }
            let hay = String::from_utf8_lossy(&bytes);
            if re.is_match(&hay) {
                assert!(
                    cand.contains(f),
                    "SOUNDNESS VIOLATION: pattern={p:?} matched {f:?} but it was not a candidate"
                );
            }
        }
    }
}

fn memchr_nul(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)].contains(&0)
}
