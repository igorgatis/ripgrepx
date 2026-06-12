//! Result ordering — ripgrep's `--sort`/`--sortr` vocabulary, plus an rgx-only `weight` key.
//!
//! All keys are **file-level**: a per-file order value decides the order files appear in; matches
//! within a file stay in line order. `--sortr` reverses the *file* order only (lines stay ascending),
//! matching ripgrep. The order value is a single `i64` so one comparator serves every key and both
//! surfaces (bare flat output and the `--compact`/MCP paged view):
//!
//! - `modified` / `accessed` / `created` — filesystem time in nanoseconds (via `stat`, like rg).
//! - `weight` — `-(score · 1e6)`, so a higher weighted-match score sorts first (best-first; the one
//!   documented departure from rg's ascending convention — `--sortr=weight` flips it).
//! - `path` / `none` — `0`, so the order falls to the path tiebreak (rg's lexical file order).
//!
//! `none` (the default) means "don't reorder": the bare path streams as usual and the compact view
//! keeps its historical `(path, lineno)` order, so absence of `--sort` is byte-for-byte today.

use std::cmp::Ordering;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::{Result, bail};

/// What to order results by. Mirrors ripgrep's `--sort`/`--sortr` values, plus `Weight`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Don't reorder (ripgrep's default; bare output streams, compact keeps path order).
    #[default]
    None,
    Path,
    Modified,
    Accessed,
    Created,
    /// rgx-only: weighted match (`--weights`), highest score first.
    Weight,
}

/// A resolved `--sort`/`--sortr` request: the key and whether to reverse file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SortSpec {
    pub key: SortKey,
    /// `true` for `--sortr` (descending file order); lines within a file stay ascending regardless.
    pub reverse: bool,
}

impl SortSpec {
    pub fn is_noop(&self) -> bool {
        self.key == SortKey::None
    }

    /// Whether this sort needs a weighted-match ranker (`--sort=weight`).
    pub fn needs_weights(&self) -> bool {
        self.key == SortKey::Weight
    }

    pub fn encode_key(&self) -> u8 {
        match self.key {
            SortKey::None => 0,
            SortKey::Path => 1,
            SortKey::Modified => 2,
            SortKey::Accessed => 3,
            SortKey::Created => 4,
            SortKey::Weight => 5,
        }
    }

    pub fn decode(byte: u8, reverse: bool) -> Result<SortSpec> {
        let key = match byte {
            0 => SortKey::None,
            1 => SortKey::Path,
            2 => SortKey::Modified,
            3 => SortKey::Accessed,
            4 => SortKey::Created,
            5 => SortKey::Weight,
            other => bail!("unknown sort key {other}"),
        };
        Ok(SortSpec { key, reverse })
    }
}

/// Parse a `--sort`/`--sortr` value. `reverse` is set by the caller from which flag was used.
pub fn parse(value: &str, reverse: bool) -> Result<SortSpec> {
    let key = match value {
        "none" => SortKey::None,
        "path" => SortKey::Path,
        "modified" => SortKey::Modified,
        "accessed" => SortKey::Accessed,
        "created" => SortKey::Created,
        "weight" => SortKey::Weight,
        other => bail!(
            "unknown sort key {other:?} (expected none, path, modified, accessed, created, weight)"
        ),
    };
    // `none` has no order to reverse — normalize so `--sortr=none` stays the historical default
    // rather than reversing the path order in the compact/MCP view.
    let reverse = reverse && key != SortKey::None;
    Ok(SortSpec { key, reverse })
}

/// The filesystem order value for `path` under `key`: time in ns since the epoch, or 0 when the stat
/// or the requested timestamp is unavailable (so such files sort as oldest). Only meaningful for the
/// time keys; returns 0 otherwise.
pub fn fs_order_value(key: SortKey, root: &Path, path: &str) -> i64 {
    let meta = match std::fs::metadata(root.join(path)) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let time = match key {
        SortKey::Modified => meta.modified().ok(),
        SortKey::Accessed => meta.accessed().ok(),
        SortKey::Created => meta.created().ok(),
        _ => None,
    };
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// The order value for a weighted-match score: negated and scaled so a higher score sorts *first*
/// under ascending order (best-first). Micro-weight resolution is ample for relevance ranking.
pub fn weight_to_order(score: f32) -> i64 {
    (-(score as f64) * 1_000_000.0).round() as i64
}

/// Total order over `(order_value, path, lineno)`. File order is `(order_value, path)` ascending,
/// reversed for `--sortr`; line order within a file is always ascending (ripgrep's behavior). A
/// deterministic total order is required for stable keyset paging.
pub fn cmp(a: (i64, &str, u64), b: (i64, &str, u64), reverse: bool) -> Ordering {
    let file = a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1));
    let file = if reverse { file.reverse() } else { file };
    file.then_with(|| a.2.cmp(&b.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rg_vocabulary_and_rejects_unknown() {
        assert_eq!(parse("modified", false).unwrap().key, SortKey::Modified);
        assert_eq!(
            parse("weight", true).unwrap(),
            SortSpec {
                key: SortKey::Weight,
                reverse: true
            }
        );
        assert!(parse("bogus", false).is_err());
    }

    #[test]
    fn sortr_none_is_not_reversed() {
        // `none` has no order to reverse, so `--sortr=none` must equal the default (no reorder).
        assert_eq!(parse("none", true).unwrap(), SortSpec::default());
    }

    #[test]
    fn key_roundtrips_through_encode_decode() {
        for key in [
            SortKey::None,
            SortKey::Path,
            SortKey::Modified,
            SortKey::Accessed,
            SortKey::Created,
            SortKey::Weight,
        ] {
            let s = SortSpec { key, reverse: true };
            assert_eq!(SortSpec::decode(s.encode_key(), true).unwrap(), s);
        }
        assert!(SortSpec::decode(99, false).is_err());
    }

    #[test]
    fn reverse_flips_files_but_not_lines() {
        // Ascending: file A (order 1) before file B (order 2); within a file, lines ascending.
        let a1 = (1i64, "a", 1u64);
        let a2 = (1i64, "a", 2u64);
        let b1 = (2i64, "b", 1u64);
        assert_eq!(cmp(a1, b1, false), Ordering::Less);
        assert_eq!(cmp(a1, a2, false), Ordering::Less);
        // Reversed: file B before file A, but a's lines still ascending among themselves.
        assert_eq!(cmp(a1, b1, true), Ordering::Greater);
        assert_eq!(cmp(a1, a2, true), Ordering::Less);
    }

    #[test]
    fn weight_orders_best_first() {
        // Higher score -> smaller order value -> sorts first under ascending cmp.
        assert!(weight_to_order(0.9) < weight_to_order(0.1));
        let hi = (weight_to_order(0.9), "z", 1u64);
        let lo = (weight_to_order(0.1), "a", 1u64);
        assert_eq!(cmp(hi, lo, false), Ordering::Less);
    }
}
