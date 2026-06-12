//! Opaque pagination cursor for the compact view. A cursor carries the entire query (pattern +
//! options) plus a keyset resume position, so page N is provably the same search as page 1 (no flag
//! can be dropped between pages) and a changed result set is detectable via the stored fingerprint.
//! The encoded blob is parked in the daemon's [`crate::pagination`] store, which hands the caller a
//! short token to echo back, so the blob itself never has to be small or text-safe. Encoding mirrors
//! the hand-rolled, length-prefixed style in `proto` and reuses its exact options bit-layout.

use std::hash::Hasher;

use anyhow::{Result, bail};
use rustc_hash::FxHasher;

use crate::confirm::SearchOptions;
use crate::proto::{pack_opts, unpack_opts};
use crate::sort::SortSpec;

const KIND: u8 = 0x01;
// 0x03: the packed-options byte grew from 5 to 8 flag bits (invert/hidden/no_ignore). Bumping the
// version makes a cross-version binary reject a cursor it would otherwise misread (decoding the new
// flags as unset) rather than silently serving the wrong result set.
const VERSION: u8 = 0x03;

/// The compact output shape a cursor paginates over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// `path` + `  line: text` rows, paged by match (default).
    #[default]
    Matches,
    /// One matching path per line (`-l`), paged by file.
    Files,
    /// `path:count` per file (`-c`), paged by file.
    Count,
}

/// Everything needed to reproduce a query and resume where the previous page ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    pub mode: Mode,
    pub pattern: String,
    pub opts: SearchOptions,
    pub page_size: usize,
    /// Keyset position: resume after this `(order, path, lineno)` (lineno is 0 in files/count modes;
    /// `order` is the sort value — 0 for the default order).
    pub last_path: Option<String>,
    pub last_lineno: u64,
    pub last_order: i64,
    /// How the view is ordered (`--sort`/`--sortr`), so the next page reproduces the exact order.
    pub sort: SortSpec,
    /// The `--weights` spec for `--sort=weight`, so the next page rebuilds the same ranker. `None`
    /// otherwise.
    pub weights: Option<String>,
    /// `total_matches` when the cursor was minted, so a resume can report "N -> M matches".
    pub prev_total: usize,
    /// Low 32 bits of the result-set fingerprint when minted, for (advisory) staleness detection.
    pub fingerprint: u32,
    /// The positional path the query was scoped to, relative to the cwd (None = the cwd itself). The
    /// caller pages from the same directory, so a short relative scope re-resolves the same tree.
    pub root_hint: Option<String>,
}

/// Fold every `(path, lineno)` match key into a stable fingerprint; any add/remove/reorder changes it.
pub fn fingerprint<'a>(keys: impl Iterator<Item = (&'a str, u64)>) -> u64 {
    let mut h = FxHasher::default();
    for (path, lineno) in keys {
        h.write(path.as_bytes());
        h.write_u8(0xff);
        h.write_u64(lineno);
    }
    h.finish()
}

pub fn encode(c: &Cursor) -> Vec<u8> {
    let mode = match c.mode {
        Mode::Matches => 0,
        Mode::Files => 1,
        Mode::Count => 2,
    };
    let mut b = vec![KIND, VERSION, mode, pack_opts(&c.opts)];
    put_varint(&mut b, c.opts.before_context as u64);
    put_varint(&mut b, c.opts.after_context as u64);
    put_varint(&mut b, c.page_size as u64);
    put_varint(&mut b, c.prev_total as u64);
    put_varint(&mut b, c.fingerprint as u64);
    put_varint(&mut b, c.last_lineno);
    put_varint(&mut b, c.last_order as u64);
    b.push(c.sort.encode_key());
    b.push(c.sort.reverse as u8);
    put_opt(&mut b, c.last_path.as_deref());
    put_opt(&mut b, c.root_hint.as_deref());
    put_opt(&mut b, c.weights.as_deref());
    put_bytes(&mut b, c.pattern.as_bytes());
    b
}

pub fn decode(bytes: &[u8]) -> Result<Cursor> {
    let mut cur = bytes;
    if take_u8(&mut cur)? != KIND {
        bail!("not an rgx cursor");
    }
    if take_u8(&mut cur)? != VERSION {
        bail!("unsupported cursor version");
    }
    let mode = match take_u8(&mut cur)? {
        0 => Mode::Matches,
        1 => Mode::Files,
        2 => Mode::Count,
        other => bail!("unknown cursor mode {other}"),
    };
    let packed = take_u8(&mut cur)?;
    let before = take_varint(&mut cur)? as u32;
    let after = take_varint(&mut cur)? as u32;
    let opts = unpack_opts(packed, before, after);
    let page_size = take_varint(&mut cur)? as usize;
    let prev_total = take_varint(&mut cur)? as usize;
    let fingerprint = take_varint(&mut cur)? as u32;
    let last_lineno = take_varint(&mut cur)?;
    let last_order = take_varint(&mut cur)? as i64;
    let sort_key = take_u8(&mut cur)?;
    let sort_reverse = take_u8(&mut cur)? != 0;
    let sort = SortSpec::decode(sort_key, sort_reverse)?;
    let last_path = take_opt(&mut cur)?;
    let root_hint = take_opt(&mut cur)?;
    let weights = take_opt(&mut cur)?;
    let pattern = String::from_utf8(take_bytes(&mut cur)?)?;
    Ok(Cursor {
        mode,
        pattern,
        opts,
        page_size,
        last_path,
        last_lineno,
        last_order,
        sort,
        weights,
        prev_total,
        fingerprint,
        root_hint,
    })
}

/// LEB128 unsigned varint: small values cost 1 byte, keeping a typical cursor short.
fn put_varint(buf: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

fn take_varint(cur: &mut &[u8]) -> Result<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let b = take_u8(cur)?;
        if shift >= 64 {
            bail!("malformed cursor varint");
        }
        result |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

fn put_bytes(buf: &mut Vec<u8>, b: &[u8]) {
    put_varint(buf, b.len() as u64);
    buf.extend_from_slice(b);
}

fn put_opt(buf: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(s) => {
            buf.push(1);
            put_bytes(buf, s.as_bytes());
        }
        None => buf.push(0),
    }
}

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    let (&b, rest) = cur
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("truncated cursor"))?;
    *cur = rest;
    Ok(b)
}

fn take_bytes(cur: &mut &[u8]) -> Result<Vec<u8>> {
    let n = take_varint(cur)? as usize;
    if cur.len() < n {
        bail!("truncated cursor");
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head.to_vec())
}

fn take_opt(cur: &mut &[u8]) -> Result<Option<String>> {
    Ok(match take_u8(cur)? {
        0 => None,
        _ => Some(String::from_utf8(take_bytes(cur)?)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Cursor {
        Cursor {
            mode: Mode::Count,
            pattern: "Foo|Bar".into(),
            opts: SearchOptions {
                case_insensitive: true,
                word: true,
                after_context: 3,
                ..Default::default()
            },
            page_size: 25,
            last_path: Some("src/café.rs".into()),
            last_lineno: 42,
            last_order: -700_000,
            sort: crate::sort::SortSpec {
                key: crate::sort::SortKey::Weight,
                reverse: false,
            },
            weights: Some("w1:0.7,w2:0.3".into()),
            prev_total: 421,
            fingerprint: 0xdead_beef,
            root_hint: Some("src/".into()),
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let c = sample();
        assert_eq!(decode(&encode(&c)).unwrap(), c);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(b"not a cursor").is_err());
        assert!(decode(b"").is_err());
    }

    #[test]
    fn decode_rejects_wrong_kind() {
        assert!(decode(&[0x02, VERSION, 0]).is_err());
    }

    #[test]
    fn fingerprint_changes_when_keys_change() {
        let a = fingerprint([("a.rs", 1), ("b.rs", 2)].into_iter());
        let same = fingerprint([("a.rs", 1), ("b.rs", 2)].into_iter());
        let diff = fingerprint([("a.rs", 1), ("b.rs", 3)].into_iter());
        assert_eq!(a, same);
        assert_ne!(a, diff);
    }
}
