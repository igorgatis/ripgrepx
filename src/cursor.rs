//! Opaque pagination cursor for the compact view. A cursor is a base64url blob the caller echoes
//! back; it carries the entire query (pattern + options) plus a keyset resume position, so page N is
//! provably the same search as page 1 (no flag can be dropped between pages) and a changed result set
//! is detectable via the stored fingerprint. Encoding mirrors the hand-rolled, length-prefixed style
//! in `proto` and reuses its exact options bit-layout.

use std::hash::Hasher;

use anyhow::{Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rustc_hash::FxHasher;

use crate::confirm::SearchOptions;
use crate::proto::{pack_opts, unpack_opts};

const KIND: u8 = 0x01;
const VERSION: u8 = 0x01;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub mode: Mode,
    pub pattern: String,
    pub opts: SearchOptions,
    pub page_size: usize,
    /// Keyset position: resume after this `(path, lineno)` (lineno is 0 in files/count modes).
    pub last_path: Option<String>,
    pub last_lineno: u64,
    /// `total_matches` when the cursor was minted, so a resume can report "N -> M matches".
    pub prev_total: usize,
    /// Fingerprint of the full result set when minted, for staleness detection.
    pub fingerprint: u64,
    /// The positional path the query was scoped to (so `--cursor` reproduces the root); None = cwd.
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

pub fn encode(c: &Cursor) -> String {
    let mode = match c.mode {
        Mode::Matches => 0,
        Mode::Files => 1,
        Mode::Count => 2,
    };
    let mut b = vec![KIND, VERSION, mode, pack_opts(&c.opts)];
    put_u32(&mut b, c.opts.before_context as u32);
    put_u32(&mut b, c.opts.after_context as u32);
    put_u32(&mut b, c.page_size as u32);
    put_u64(&mut b, c.prev_total as u64);
    put_u64(&mut b, c.fingerprint);
    put_u64(&mut b, c.last_lineno);
    put_opt(&mut b, c.last_path.as_deref());
    put_opt(&mut b, c.root_hint.as_deref());
    put_bytes(&mut b, c.pattern.as_bytes());
    URL_SAFE_NO_PAD.encode(&b)
}

pub fn decode(s: &str) -> Result<Cursor> {
    let bytes = URL_SAFE_NO_PAD
        .decode(s.trim())
        .map_err(|_| anyhow::anyhow!("not a valid cursor"))?;
    let mut cur = &bytes[..];
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
    let before = take_u32(&mut cur)?;
    let after = take_u32(&mut cur)?;
    let opts = unpack_opts(packed, before, after);
    let page_size = take_u32(&mut cur)? as usize;
    let prev_total = take_u64(&mut cur)? as usize;
    let fingerprint = take_u64(&mut cur)?;
    let last_lineno = take_u64(&mut cur)?;
    let last_path = take_opt(&mut cur)?;
    let root_hint = take_opt(&mut cur)?;
    let pattern = String::from_utf8(take_bytes(&mut cur)?)?;
    Ok(Cursor {
        mode,
        pattern,
        opts,
        page_size,
        last_path,
        last_lineno,
        prev_total,
        fingerprint,
        root_hint,
    })
}

fn put_u32(buf: &mut Vec<u8>, n: u32) {
    buf.extend_from_slice(&n.to_le_bytes());
}

fn put_u64(buf: &mut Vec<u8>, n: u64) {
    buf.extend_from_slice(&n.to_le_bytes());
}

fn put_bytes(buf: &mut Vec<u8>, b: &[u8]) {
    put_u32(buf, b.len() as u32);
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

fn take_u32(cur: &mut &[u8]) -> Result<u32> {
    if cur.len() < 4 {
        bail!("truncated cursor");
    }
    let (head, rest) = cur.split_at(4);
    *cur = rest;
    Ok(u32::from_le_bytes(head.try_into().unwrap()))
}

fn take_u64(cur: &mut &[u8]) -> Result<u64> {
    if cur.len() < 8 {
        bail!("truncated cursor");
    }
    let (head, rest) = cur.split_at(8);
    *cur = rest;
    Ok(u64::from_le_bytes(head.try_into().unwrap()))
}

fn take_bytes(cur: &mut &[u8]) -> Result<Vec<u8>> {
    let n = take_u32(cur)? as usize;
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
            prev_total: 421,
            fingerprint: 0xdead_beef_1234,
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
        assert!(decode("not a cursor!!!").is_err());
        assert!(decode("").is_err());
    }

    #[test]
    fn decode_rejects_wrong_kind() {
        let bad = URL_SAFE_NO_PAD.encode([0x02, VERSION, 0]);
        assert!(decode(&bad).is_err());
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
