//! Microbenchmarks for rgx's hot paths, isolated from the daemon/socket so deltas are clean:
//! cold index build, candidate selection, the ripgrep confirm, and trigram extraction.
//!
//! Run: `cargo bench`. The corpus is a deterministic synthetic tree in the temp dir, generated once
//! and reused across runs (delete `$TMPDIR/rgx_bench_corpus_*` to regenerate).

use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};

use rgx::confirm::{self, SearchOptions};
use rgx::index::Index;
use rgx::query::{Options, Query};
use rgx::trigram;

/// Generate `files` files of `lines` lines each of pseudo-source text (deterministic LCG, no deps).
fn make_corpus(files: usize, lines: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgx_bench_corpus_{files}x{lines}"));
    if dir.join(".done").exists() {
        return dir;
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    const WORDS: [&str; 20] = [
        "fn",
        "let",
        "return",
        "value",
        "index",
        "trigram",
        "search",
        "pattern",
        "Result",
        "Vec",
        "match",
        "struct",
        "impl",
        "query",
        "file",
        "ripgrep",
        "candidate",
        "needle",
        "token",
        "alpha",
    ];
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    for f in 0..files {
        let mut s = String::with_capacity(lines * 48);
        for _ in 0..lines {
            for _ in 0..(4 + next() % 8) {
                s.push_str(WORDS[next() % WORDS.len()]);
                s.push(' ');
            }
            s.push_str(&format!("sym_{f}_{}\n", next() % 1000));
        }
        std::fs::write(dir.join(format!("file_{f:05}.rs")), s).unwrap();
    }
    std::fs::write(dir.join(".done"), b"").unwrap();
    dir
}

fn benches(c: &mut Criterion) {
    let corpus = make_corpus(2000, 80);

    c.bench_function("build_2000_files", |b| {
        b.iter(|| Index::build(black_box(&corpus)))
    });

    let idx = Index::build(&corpus);
    let q = Query::for_pattern("trigram", Options::default());
    c.bench_function("candidates", |b| {
        b.iter(|| black_box(idx.candidates(black_box(&q))).len())
    });

    // confirm over the resolved candidate set (owned, so it doesn't borrow `idx` in the loop).
    let cands: Vec<PathBuf> = idx
        .candidates(&q)
        .into_iter()
        .map(Path::to_path_buf)
        .collect();
    let refs: Vec<&Path> = cands.iter().map(PathBuf::as_path).collect();
    c.bench_function("confirm", |b| {
        b.iter(|| {
            let mut n = 0usize;
            confirm::search_streaming("trigram", &refs, SearchOptions::default(), |chunk| {
                n += chunk.len();
                Ok(())
            })
            .unwrap();
            n
        })
    });

    let blob = std::fs::read(corpus.join("file_00000.rs")).unwrap();
    c.bench_function("trigram_distinct_one_file", |b| {
        b.iter(|| trigram::distinct(black_box(&blob)).len())
    });
}

criterion_group!(g, benches);
criterion_main!(g);
