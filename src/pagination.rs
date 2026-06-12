//! Server-side pagination store: the daemon keeps the small cursor blob (query + keyset position) for
//! a couple of minutes and hands the client a short opaque token in its place, so the printed
//! `--cursor` is tiny instead of a base64 blob. The blob is exactly what the self-contained cursor
//! used to carry — paging still re-runs the search, so memory is a few dozen bytes per live page, not
//! the result set. Tokens are stamped with a per-process session so a restarted daemon's old tokens
//! miss cleanly (the client just re-runs the search). `take` is single-use: following a page deletes
//! the token it came from.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a minted token stays resolvable. Long enough for an agent to read a page and ask for the
/// next, short enough that abandoned paginations cost nothing.
pub const DEFAULT_TTL: Duration = Duration::from_secs(120);

pub struct PaginationStore {
    /// Per-process stamp (hex) prefixed onto every token; a token from another process can't resolve.
    session: String,
    counter: u64,
    ttl: Duration,
    entries: HashMap<u64, (Vec<u8>, Instant)>,
}

impl PaginationStore {
    pub fn new(session: u64, ttl: Duration) -> Self {
        Self {
            // 32 bits of the seed is plenty to make a previous daemon's tokens miss; keeps the prefix
            // (and thus the printed token) short.
            session: format!("{:08x}", session as u32),
            counter: 0,
            ttl,
            entries: HashMap::new(),
        }
    }

    /// Store `blob` and return its token. `now` is injected so the store stays testable.
    pub fn store(&mut self, blob: Vec<u8>, now: Instant) -> String {
        self.evict(now);
        self.counter += 1;
        let id = self.counter;
        self.entries.insert(id, (blob, now));
        format!("{}{id:x}", self.session)
    }

    /// Resolve and consume `token`, returning its blob if present and unexpired. A token minted by a
    /// different session (e.g. a previous daemon) or already taken returns `None`.
    pub fn take(&mut self, token: &str, now: Instant) -> Option<Vec<u8>> {
        self.evict(now);
        let rest = token.strip_prefix(&self.session)?;
        let id: u64 = u64::from_str_radix(rest, 16).ok()?;
        let (blob, minted) = self.entries.remove(&id)?;
        (now.duration_since(minted) < self.ttl).then_some(blob)
    }

    fn evict(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.entries
            .retain(|_, (_, minted)| now.duration_since(*minted) < ttl);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_then_take_roundtrips_and_is_single_use() {
        let mut s = PaginationStore::new(0xABCD, DEFAULT_TTL);
        let t0 = Instant::now();
        let tok = s.store(b"hello".to_vec(), t0);
        assert!(tok.starts_with("0000abcd"));
        assert_eq!(s.take(&tok, t0).as_deref(), Some(&b"hello"[..]));
        // single use: the token is gone after the first take.
        assert_eq!(s.take(&tok, t0), None);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn expired_token_and_foreign_session_miss() {
        let mut s = PaginationStore::new(1, Duration::from_secs(120));
        let t0 = Instant::now();
        let tok = s.store(b"x".to_vec(), t0);
        assert_eq!(s.take(&tok, t0 + Duration::from_secs(121)), None);

        // A token shaped for a different session never resolves.
        let other = PaginationStore::new(2, DEFAULT_TTL).store(b"y".to_vec(), t0);
        let mut s = PaginationStore::new(1, DEFAULT_TTL);
        assert_eq!(s.take(&other, t0), None);
        assert_eq!(s.take("not-a-token", t0), None);
    }
}
