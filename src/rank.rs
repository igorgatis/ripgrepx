//! Weighted match (`--sort=weight --weights=…`): a model-supplied relevance signal that *reorders* the
//! view without ever changing the match set. The user declares named weights and annotates regex
//! branches with `<label>`:
//!
//! ```text
//! rgx --sort=weight --weights=w1:0.7,w2:0.3 'Hello (world<w1>|earth<w2>)!'
//! ```
//!
//! Two artifacts come out of one annotated pattern:
//! - the **plain pattern** (`Hello (world|earth)!`), with annotations stripped, which flows through
//!   the normal search path untouched — so the match set stays byte-for-byte ripgrep's;
//! - a **ranker**: a capture-instrumented copy of the regex (each annotated branch wrapped in a named
//!   capture group) used only by `compact` to score a matching line by which branch participated.
//!
//! A file's score is the max weight of any branch matched in it (per-file aggregation); unattributed
//! matches score 0 and sink last. Scoring is presentation-only and never gates correctness: if the
//! instrumented regex misbehaves, the worst case is a worse ordering, never a wrong or missing match.

use std::collections::HashMap;

use anyhow::{Result, bail};
use grep::matcher::{Captures, Matcher};
use grep::regex::RegexMatcher;

use crate::confirm::{SearchOptions, build_matcher};

/// The result of resolving an annotated pattern: the plain pattern to actually search, and an
/// optional ranker to score matches by. `ranker` is `None` when there is nothing to rank (no
/// `--weights`, or no labeled branch resolved), in which case the view orders as it always has.
pub struct Ranking {
    pub plain: String,
    pub ranker: Option<Ranker>,
}

/// Scores a matching line by which labeled branch participated. Holds the instrumented matcher plus
/// the capture-group index of each labeled branch and its weight.
pub struct Ranker {
    matcher: RegexMatcher,
    group_weight: Vec<(usize, f32)>,
}

impl Ranker {
    /// The weight of `line`: the max weight over labeled branches that participated in the match, or
    /// 0.0 when none did (or the line doesn't match the instrumented form). Presentation-only, so any
    /// engine hiccup degrades to 0.0 rather than erroring.
    pub fn score(&self, line: &str) -> f32 {
        let Ok(mut caps) = self.matcher.new_captures() else {
            return 0.0;
        };
        if !self
            .matcher
            .captures(line.as_bytes(), &mut caps)
            .unwrap_or(false)
        {
            return 0.0;
        }
        let mut best = 0.0f32;
        for &(idx, w) in &self.group_weight {
            if caps.get(idx).is_some() {
                best = best.max(w);
            }
        }
        best
    }
}

/// Resolve `pattern` against an optional `--weights` spec. With no spec, the pattern passes through
/// unchanged and there is no ranker. With a spec, declared `<label>` annotations are stripped to form
/// the plain pattern and an instrumented matcher is built to attribute matches to weights.
pub fn parse(pattern: &str, weights: Option<&str>, opts: SearchOptions) -> Result<Ranking> {
    let Some(spec) = weights else {
        return Ok(Ranking {
            plain: pattern.to_string(),
            ranker: None,
        });
    };
    if opts.fixed_strings {
        bail!("--sort=weight cannot be combined with -F (fixed strings has no branches to weight)");
    }
    let weights = parse_weights(spec)?;
    let ins = instrument(pattern, &weights)?;
    if ins.group_weights.is_empty() {
        // A spec with no matching annotation in the pattern: nothing to rank, order as usual.
        return Ok(Ranking {
            plain: ins.plain,
            ranker: None,
        });
    }
    let matcher = build_matcher(&ins.instrumented, opts)?;
    let group_weight = ins
        .group_weights
        .iter()
        .filter_map(|(name, w)| matcher.capture_index(name).map(|idx| (idx, *w)))
        .collect();
    let plain = ins.plain;
    Ok(Ranking {
        plain,
        ranker: Some(Ranker {
            matcher,
            group_weight,
        }),
    })
}

/// Parse `w1:0.7,w2:0.3` into a label→weight map. Labels are `[A-Za-z_][A-Za-z0-9_]*`; weights are
/// finite floats. Errors on a malformed term so a typo surfaces instead of silently dropping a weight.
fn parse_weights(spec: &str) -> Result<HashMap<String, f32>> {
    let mut out = HashMap::new();
    for term in spec.split(',') {
        let term = term.trim();
        if term.is_empty() {
            continue;
        }
        let Some((label, weight)) = term.split_once(':') else {
            bail!("--weights term {term:?} must be label:weight");
        };
        if !is_label(label) {
            bail!("--weights label {label:?} must be a word (letters, digits, underscore)");
        }
        let w: f32 = weight.parse().map_err(|_| {
            anyhow::anyhow!("--weights value {weight:?} for {label:?} is not a number")
        })?;
        if !w.is_finite() {
            bail!("--weights value for {label:?} must be finite");
        }
        out.insert(label.to_string(), w);
    }
    if out.is_empty() {
        bail!("--weights needs at least one label:weight");
    }
    Ok(out)
}

fn is_label(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// The plain (searched) pattern, the capture-instrumented pattern, and each generated group's weight.
struct Instrumented {
    plain: String,
    instrumented: String,
    group_weights: Vec<(String, f32)>,
}

/// Walk `pattern`, stripping `<label>` annotations (where `label` is declared) to build the plain
/// pattern, and emit an instrumented copy with each annotated branch wrapped in a named capture group
/// `(?P<__rgxwN>...)`. A branch is the run since the nearest unescaped `(`, `|`, `)`, or start, at
/// top level (class contents are opaque).
fn instrument(pattern: &str, weights: &HashMap<String, f32>) -> Result<Instrumented> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut plain = String::new();
    let mut instr = String::new();
    let mut names: Vec<(String, f32)> = Vec::new();
    let mut bstart = 0usize; // byte offset in `instr` where the current branch began
    let mut escaped = false;
    let mut in_class = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            plain.push(c);
            instr.push(c);
            escaped = false;
            i += 1;
            continue;
        }
        match c {
            '\\' => {
                plain.push(c);
                instr.push(c);
                escaped = true;
            }
            _ if in_class => {
                plain.push(c);
                instr.push(c);
                if c == ']' {
                    in_class = false;
                }
            }
            '[' => {
                in_class = true;
                plain.push(c);
                instr.push(c);
            }
            '(' | ')' | '|' => {
                plain.push(c);
                instr.push(c);
                bstart = instr.len();
            }
            '<' => {
                if let Some((label, consumed)) = read_annotation(&chars, i, weights) {
                    if instr.len() == bstart {
                        bail!("--weights: <{label}> has no preceding branch to weight");
                    }
                    let name = format!("__rgxw{}", names.len());
                    instr.insert_str(bstart, &format!("(?P<{name}>"));
                    instr.push(')');
                    names.push((name, weights[&label]));
                    bstart = instr.len();
                    i += consumed;
                    continue;
                }
                plain.push(c);
                instr.push(c);
            }
            _ => {
                plain.push(c);
                instr.push(c);
            }
        }
        i += 1;
    }
    Ok(Instrumented {
        plain,
        instrumented: instr,
        group_weights: names,
    })
}

/// If `chars[i]` opens `<label>` for a declared `label`, return `(label, chars_consumed)`. Otherwise
/// the `<` is an ordinary literal (returns `None`), so normal patterns never need to escape it.
fn read_annotation(
    chars: &[char],
    i: usize,
    weights: &HashMap<String, f32>,
) -> Option<(String, usize)> {
    debug_assert_eq!(chars[i], '<');
    let mut j = i + 1;
    let mut label = String::new();
    // Only label characters belong inside an annotation; stop at anything else (e.g. a nested `<`),
    // so a literal `<` ahead of a real `<label>` can't be swallowed into a bogus label span.
    while j < chars.len() && (chars[j] == '_' || chars[j].is_ascii_alphanumeric()) {
        label.push(chars[j]);
        j += 1;
    }
    if j < chars.len() && chars[j] == '>' && weights.contains_key(&label) {
        Some((label, j - i + 1))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> SearchOptions {
        SearchOptions::default()
    }

    #[test]
    fn weighted_pattern_strips_annotations_and_scores_by_branch() {
        let r = parse(
            "Hello (world<w1>|earth<w2>)!",
            Some("w1:0.7,w2:0.3"),
            opts(),
        )
        .unwrap();
        // The searched pattern is the plain regex — exactly what ripgrep matches.
        assert_eq!(r.plain, "Hello (world|earth)!");
        let ranker = r.ranker.unwrap();
        assert!((ranker.score("Hello world!") - 0.7).abs() < 1e-6);
        assert!((ranker.score("Hello earth!") - 0.3).abs() < 1e-6);
        assert_eq!(ranker.score("Hello mars!"), 0.0);
    }

    #[test]
    fn alternation_branch_order_decides_the_match() {
        // Which branch "matched" is the regex engine's call (leftmost-first), not ours: `foo` is
        // listed first, so it wins even where `foobar` would also fit.
        let r = parse("(foo<lo>|foobar<hi>)", Some("lo:0.2,hi:0.9"), opts()).unwrap();
        let ranker = r.ranker.unwrap();
        assert!((ranker.score("xfoobar") - 0.2).abs() < 1e-6);
        assert!((ranker.score("xfoo") - 0.2).abs() < 1e-6);
    }

    #[test]
    fn max_weight_wins_when_labels_co_participate() {
        // Both labeled groups participate in a single match; the file takes the larger weight.
        let r = parse("a<lo>.*b<hi>", Some("lo:0.2,hi:0.9"), opts()).unwrap();
        assert_eq!(r.plain, "a.*b");
        let ranker = r.ranker.unwrap();
        assert!((ranker.score("xaYYbz") - 0.9).abs() < 1e-6);
    }

    #[test]
    fn no_weights_is_passthrough() {
        let r = parse("plain|pattern", None, opts()).unwrap();
        assert_eq!(r.plain, "plain|pattern");
        assert!(r.ranker.is_none());
    }

    #[test]
    fn undeclared_angle_token_stays_literal() {
        // `<x>` is not a declared label, so it is left in the pattern verbatim (no escaping needed).
        let r = parse("a<x>b", Some("w1:0.5"), opts()).unwrap();
        assert_eq!(r.plain, "a<x>b");
        assert!(r.ranker.is_none());
    }

    #[test]
    fn rejects_fixed_strings() {
        assert!(
            parse(
                "foo",
                Some("w:1.0"),
                SearchOptions {
                    fixed_strings: true,
                    ..opts()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_malformed_weights() {
        assert!(parse("foo<w>", Some("w"), opts()).is_err());
        assert!(parse("foo<w>", Some("w:abc"), opts()).is_err());
        assert!(parse("foo<w>", Some(""), opts()).is_err());
    }

    #[test]
    fn rejects_annotation_with_empty_branch() {
        assert!(parse("(<w1>foo)", Some("w1:0.5"), opts()).is_err());
    }
}
