//! Soundness verification for regex -> trigram query extraction (spec hypothesis H-8 + the core
//! correctness contract). The bar: for every (pattern, text) where the real `regex` engine reports
//! a match, the trigram query derived from that pattern MUST also accept the text's trigram set.
//! A single violation here means rgx could drop a real result, which is not allowed.
//!
//! False positives (query accepts but regex doesn't match) are fine and expected — ripgrep filters
//! them out — so we only measure them as a precision signal, never assert on them.

use std::collections::BTreeSet;

use regex::Regex;
use rgx::query::{Options, Query};
use rgx::trigram::{self, Trigram};

/// Tiny deterministic PRNG (xorshift64*) so the fuzz run is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick(&mut self, s: &[u8]) -> u8 {
        s[self.below(s.len())]
    }
}

const PAT_ALPHA: &[u8] = b"abcXY9_";
const TXT_ALPHA: &[u8] = b"abcXY9_dZ .";

fn rand_run(rng: &mut Rng) -> String {
    let n = 1 + rng.below(4);
    (0..n).map(|_| rng.pick(PAT_ALPHA) as char).collect()
}

/// Generate a random (usually-valid) regex string, depth-limited.
fn rand_pattern(rng: &mut Rng, depth: u32) -> String {
    if depth == 0 {
        return rand_run(rng);
    }
    match rng.below(7) {
        0 => format!(
            "{}{}",
            rand_pattern(rng, depth - 1),
            rand_pattern(rng, depth - 1)
        ),
        1 => format!(
            "(?:{}|{})",
            rand_pattern(rng, depth - 1),
            rand_pattern(rng, depth - 1)
        ),
        2 => {
            let q = [b"*", b"+", b"?"][rng.below(3)];
            format!("(?:{}){}", rand_pattern(rng, depth - 1), q[0] as char)
        }
        3 => format!(
            "(?:{}){{{},{}}}",
            rand_run(rng),
            rng.below(3),
            3 + rng.below(3)
        ),
        4 => {
            let n = 1 + rng.below(3);
            let cls: String = (0..n).map(|_| rng.pick(PAT_ALPHA) as char).collect();
            format!("[{cls}]")
        }
        5 => {
            // anchored literal
            let anch = if rng.below(2) == 0 { "^" } else { "$" };
            if anch == "^" {
                format!("^{}", rand_run(rng))
            } else {
                format!("{}$", rand_run(rng))
            }
        }
        _ => rand_run(rng),
    }
}

fn rand_text(rng: &mut Rng) -> String {
    let n = rng.below(24);
    (0..n).map(|_| rng.pick(TXT_ALPHA) as char).collect()
}

fn present_in(set: &BTreeSet<Trigram>) -> impl Fn(Trigram) -> bool + '_ {
    move |t| set.contains(&t)
}

#[test]
fn fuzz_no_missed_matches() {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut checks = 0u64;
    let mut matches = 0u64;
    let mut false_pos = 0u64;
    let mut accepted = 0u64;

    for _ in 0..4000 {
        // 20% case-insensitive, via inline flag so both engines agree.
        let ci = rng.below(5) == 0;
        let raw = rand_pattern(&mut rng, 3);
        let pattern = if ci { format!("(?i){raw}") } else { raw };

        // Oracle: only test patterns the real engine accepts (default flags == Options::default).
        let re = match Regex::new(&pattern) {
            Ok(re) => re,
            Err(_) => continue,
        };
        let query = Query::for_pattern(
            &pattern,
            Options {
                case_insensitive: ci,
                ..Default::default()
            },
        );

        // Texts: random, plus strings seeded from the pattern's own literals to force matches.
        let mut texts: Vec<String> = (0..24).map(|_| rand_text(&mut rng)).collect();
        texts.push(pattern.replace(
            [
                '(', ')', '?', ':', '|', '*', '+', '^', '$', '[', ']', '{', '}',
            ],
            "",
        ));
        texts.push(format!(
            "pre {} post",
            texts.last().cloned().unwrap_or_default()
        ));

        for text in &texts {
            let set = trigram::distinct(text.as_bytes());
            let predicted = query.eval(&present_in(&set));
            let actual = re.is_match(text);
            checks += 1;
            if actual {
                matches += 1;
                assert!(
                    predicted,
                    "SOUNDNESS VIOLATION: pattern={pattern:?} matched text={text:?} but trigram query rejected it\nquery={query:?}"
                );
            }
            if predicted {
                accepted += 1;
                if !actual {
                    false_pos += 1;
                }
            }
        }
    }

    eprintln!(
        "fuzz: checks={checks} matches={matches} accepted={accepted} false_pos={false_pos} \
         (precision among accepted = {:.1}%)",
        100.0 * (accepted - false_pos) as f64 / accepted.max(1) as f64
    );
    assert!(
        matches > 1000,
        "too few positive cases to trust the run: {matches}"
    );
}

/// Spot-check against real source content with realistic literal/alternation/anchor patterns.
#[test]
fn real_content_no_missed_matches() {
    let content = std::fs::read("src/query.rs").expect("read own source");
    let lines: Vec<&[u8]> = content.split(|&b| b == b'\n').collect();
    let patterns = [
        "Query",
        "trigram",
        "for_pattern",
        "Extractor",
        "fallback",
        "Query|Extractor|Options",
        "pub fn .*Query",
        "is_fallback",
        "Trigram",
        "(?i)QUERY",
    ];
    for p in patterns {
        let re = Regex::new(p).unwrap();
        let query = Query::for_pattern(
            p,
            Options {
                case_insensitive: p.contains("(?i)"),
                ..Default::default()
            },
        );
        for line in &lines {
            if re.is_match(std::str::from_utf8(line).unwrap_or("")) {
                let set = trigram::distinct(line);
                assert!(
                    query.eval(&present_in(&set)),
                    "SOUNDNESS VIOLATION on real content: pattern={p:?} line={:?}",
                    String::from_utf8_lossy(line)
                );
            }
        }
    }
}
