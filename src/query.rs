//! Turn a regex pattern into a boolean trigram query that every matching file must satisfy.
//!
//! Soundness is the whole game: the query may be satisfied by files that don't actually match
//! (ripgrep filters those out), but it must **never** exclude a file that does match. We get this
//! from `regex-syntax`'s literal extraction, which returns a *complete* over-approximation of a
//! regex's prefixes (or suffixes) or else reports that it cannot — in which case we fall back to
//! "match everything" (`Query::All`).
//!
//! Construction: every match starts with one of the extracted prefix literals AND ends with one of
//! the suffix literals. Each is an independently necessary condition, so their conjunction is
//! necessary. For a literal of length >= 3 we require all its trigrams (an AND); across the
//! alternatives we OR; a literal shorter than 3 (or an unbounded literal set) carries no trigram
//! constraint, collapsing that side to `All`. See `docs/index-and-storage.md` (section 2.3).

use crate::trigram::{self, Trigram};
use regex_syntax::ParserBuilder;
use regex_syntax::hir::literal::{ExtractKind, Extractor, Seq};

/// Options that affect how a pattern is parsed (mirroring the ripgrep flags that matter for
/// literal extraction).
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub case_insensitive: bool,
    pub multi_line: bool,
    pub dot_matches_new_line: bool,
}

/// A boolean formula over trigrams. `All` means "no constraint" — scan everything (the safe
/// fallback). Leaves are individual trigrams that must be present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    All,
    Tri(Trigram),
    And(Vec<Query>),
    Or(Vec<Query>),
}

impl Query {
    /// Build a trigram query for `pattern`. Any condition we can't reason about soundly yields
    /// `Query::All` (full scan) rather than risking a missed match.
    pub fn for_pattern(pattern: &str, opts: Options) -> Query {
        let hir = match ParserBuilder::new()
            .case_insensitive(opts.case_insensitive)
            .multi_line(opts.multi_line)
            .dot_matches_new_line(opts.dot_matches_new_line)
            .build()
            .parse(pattern)
        {
            Ok(h) => h,
            Err(_) => return Query::All, // unsupported syntax (lookaround, backrefs, ...) -> scan
        };
        let prefix = seq_query(&Extractor::new().kind(ExtractKind::Prefix).extract(&hir));
        let suffix = seq_query(&Extractor::new().kind(ExtractKind::Suffix).extract(&hir));
        Query::and(vec![prefix, suffix])
    }

    /// True if this query imposes no constraint (the engine must fall back to a full scan).
    pub fn is_fallback(&self) -> bool {
        matches!(self, Query::All)
    }

    /// Evaluate against a predicate telling whether a given trigram is present in a file.
    pub fn eval(&self, present: &impl Fn(Trigram) -> bool) -> bool {
        match self {
            Query::All => true,
            Query::Tri(t) => present(*t),
            Query::And(qs) => qs.iter().all(|q| q.eval(present)),
            Query::Or(qs) => qs.iter().any(|q| q.eval(present)),
        }
    }

    /// Collect the distinct trigrams referenced anywhere in the query (for index lookups).
    pub fn trigrams(&self, out: &mut Vec<Trigram>) {
        match self {
            Query::All => {}
            Query::Tri(t) => out.push(*t),
            Query::And(qs) | Query::Or(qs) => qs.iter().for_each(|q| q.trigrams(out)),
        }
    }

    /// AND with `All`-simplification: `All` is the identity, so it's dropped; if nothing remains,
    /// the result is `All`; a single term collapses to itself.
    fn and(parts: Vec<Query>) -> Query {
        let mut kept: Vec<Query> = parts.into_iter().filter(|q| !q.is_fallback()).collect();
        match kept.len() {
            0 => Query::All,
            1 => kept.pop().unwrap(),
            _ => Query::And(kept),
        }
    }

    /// OR with `All`-absorption: if any branch is `All`, the whole disjunction is `All`.
    fn or(parts: Vec<Query>) -> Query {
        if parts.is_empty() || parts.iter().any(Query::is_fallback) {
            return Query::All;
        }
        if parts.len() == 1 {
            return parts.into_iter().next().unwrap();
        }
        Query::Or(parts)
    }
}

/// Turn an extracted prefix/suffix literal sequence into a trigram query.
///
/// A `None` (infinite) sequence means the extractor couldn't bound the literal set — no constraint.
/// Otherwise every match must begin (or end) with one of the listed literals, so we OR them; each
/// literal of length >= 3 contributes the AND of its trigrams, and any shorter literal collapses
/// the disjunction to `All`.
fn seq_query(seq: &Seq) -> Query {
    let Some(lits) = seq.literals() else {
        return Query::All;
    };
    if lits.is_empty() {
        return Query::All;
    }
    let mut branches = Vec::with_capacity(lits.len());
    for lit in lits {
        let tris = trigram::of_literal(lit.as_bytes());
        if tris.is_empty() {
            return Query::All; // a literal shorter than a trigram imposes no constraint
        }
        let conj: Vec<Query> = tris.into_iter().map(Query::Tri).collect();
        branches.push(Query::and(conj));
    }
    Query::or(branches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigram;

    fn q(p: &str) -> Query {
        Query::for_pattern(p, Options::default())
    }

    #[test]
    fn literal_requires_all_its_trigrams() {
        let query = q("IndexWriter");
        let mut tris = Vec::new();
        query.trigrams(&mut tris);
        assert!(!query.is_fallback());
        // "IndexWriter" has 9 trigrams; prefix==suffix==exact so AND dedups to those.
        assert!(tris.contains(b"Ind"));
        assert!(tris.contains(b"ter"));
    }

    #[test]
    fn short_and_wildcard_patterns_fall_back() {
        assert!(q("ab").is_fallback()); // < 3 chars
        assert!(q(".").is_fallback());
        assert!(q("\\w+").is_fallback());
        assert!(q(".*").is_fallback());
    }

    #[test]
    fn alternation_with_an_unconstrained_branch_falls_back() {
        // "foo|.|bar": the "." branch matches anything -> whole query must scan.
        assert!(q("foo|.|bar").is_fallback());
    }

    #[test]
    fn alternation_of_literals_is_an_or() {
        let query = q("alpha|bravo|gamma");
        assert!(!query.is_fallback());
        // alpha present, bravo/gamma absent -> still matches (OR).
        let set = trigram::distinct(b"xx alpha xx");
        assert!(query.eval(&|t| set.contains(&t)));
        let none = trigram::distinct(b"nothing here");
        assert!(!query.eval(&|t| none.contains(&t)));
    }
}
