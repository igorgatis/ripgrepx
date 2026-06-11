//! Byte trigrams: the atomic unit of the candidate index.
//!
//! A trigram is any three consecutive bytes of a file's contents. A file is represented in the
//! index by the *set* of distinct trigrams it contains; a query is represented by a boolean
//! formula over trigrams that every matching file must satisfy. See `docs/index-and-storage.md`.

use std::collections::BTreeSet;

/// A single trigram. Three raw bytes; works for arbitrary (incl. non-UTF-8) content.
pub type Trigram = [u8; 3];

/// Pack a trigram into the low 24 bits of a `u32`, for compact keys.
#[inline]
pub fn pack(t: Trigram) -> u32 {
    ((t[0] as u32) << 16) | ((t[1] as u32) << 8) | (t[2] as u32)
}

/// Invoke `f` once per (not-necessarily-distinct) trigram window over `bytes`.
#[inline]
pub fn for_each(bytes: &[u8], mut f: impl FnMut(Trigram)) {
    for w in bytes.windows(3) {
        f([w[0], w[1], w[2]]);
    }
}

/// The set of distinct trigrams in `bytes`. A `BTreeSet` keeps results deterministic for tests;
/// the indexer will use a faster set.
pub fn distinct(bytes: &[u8]) -> BTreeSet<Trigram> {
    let mut set = BTreeSet::new();
    for_each(bytes, |t| {
        set.insert(t);
    });
    set
}

/// The trigrams of a literal byte string, in order (with repeats). Empty if `lit.len() < 3`.
pub fn of_literal(lit: &[u8]) -> Vec<Trigram> {
    lit.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_and_literal_agree() {
        assert_eq!(of_literal(b"abcd"), vec![*b"abc", *b"bcd"]);
        assert!(of_literal(b"ab").is_empty());
        let d = distinct(b"abcabc");
        // "abc","bca","cab","abc" -> distinct {abc,bca,cab}
        assert_eq!(d.len(), 3);
        assert!(d.contains(b"abc"));
    }
}
