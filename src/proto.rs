//! The daemon wire protocol: length-prefixed frames over a Unix socket.
//!
//! Deliberately tiny and hand-encoded (no serde): a request is one tag byte plus fields; the
//! response is a stream of non-empty data frames terminated by a zero-length frame, so results
//! (rendered `path:line:text`, a file list, or status text) flow without buffering huge sets.

use std::io::{ErrorKind, Read, Write};

use anyhow::{Result, bail};

use crate::confirm::SearchOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Content search: render `path:line:text` for `pattern`.
    Search {
        opts: SearchOptions,
        pattern: String,
    },
    /// File/dir name lookup (fd/find-style). `after` resumes a keyset page: only paths strictly
    /// greater than it are returned (empty/None = from the start).
    Find {
        needle: String,
        after: Option<String>,
        limit: u32,
    },
    /// Index health summary.
    Status,
    /// Subscribe to live status: the daemon streams a fresh status frame on each change (and a
    /// periodic heartbeat) until the client disconnects.
    Watch,
    /// Ask the daemon to exit.
    Shutdown,
    /// Park a pagination cursor blob; the daemon replies with a short opaque token.
    CursorStore { blob: Vec<u8> },
    /// Redeem a pagination token; the daemon replies with the blob, or an empty frame if it has
    /// expired or was already used.
    CursorTake { token: String },
}

pub(crate) fn pack_opts(o: &SearchOptions) -> u8 {
    (o.case_insensitive as u8)
        | ((o.multi_line as u8) << 1)
        | ((o.dot_matches_new_line as u8) << 2)
        | ((o.word as u8) << 3)
        | ((o.fixed_strings as u8) << 4)
        | ((o.invert as u8) << 5)
        | ((o.hidden as u8) << 6)
        | ((o.no_ignore as u8) << 7)
}

pub(crate) fn unpack_opts(b: u8, before: u32, after: u32) -> SearchOptions {
    SearchOptions {
        case_insensitive: b & 1 != 0,
        multi_line: b & 2 != 0,
        dot_matches_new_line: b & 4 != 0,
        word: b & 8 != 0,
        fixed_strings: b & 16 != 0,
        invert: b & 32 != 0,
        hidden: b & 64 != 0,
        no_ignore: b & 128 != 0,
        before_context: before as usize,
        after_context: after as usize,
    }
}

pub fn write_request(w: &mut impl Write, req: &Request) -> Result<()> {
    let mut body = Vec::new();
    match req {
        Request::Search { opts, pattern } => {
            body.push(b'S');
            body.push(pack_opts(opts));
            body.extend_from_slice(&(opts.before_context as u32).to_le_bytes());
            body.extend_from_slice(&(opts.after_context as u32).to_le_bytes());
            put_bytes(&mut body, pattern.as_bytes());
        }
        Request::Find {
            needle,
            after,
            limit,
        } => {
            body.push(b'F');
            body.extend_from_slice(&limit.to_le_bytes());
            put_bytes(&mut body, needle.as_bytes());
            put_bytes(&mut body, after.as_deref().unwrap_or("").as_bytes());
        }
        Request::Status => body.push(b'T'),
        Request::Watch => body.push(b'W'),
        Request::Shutdown => body.push(b'Q'),
        Request::CursorStore { blob } => {
            body.push(b'P');
            put_bytes(&mut body, blob);
        }
        Request::CursorTake { token } => {
            body.push(b'G');
            put_bytes(&mut body, token.as_bytes());
        }
    }
    write_frame(w, &body)
}

pub fn read_request(r: &mut impl Read) -> Result<Request> {
    let body = read_frame(r)?;
    let mut cur = &body[..];
    let tag = take_u8(&mut cur)?;
    Ok(match tag {
        b'S' => {
            let flags = take_u8(&mut cur)?;
            let before = take_u32(&mut cur)?;
            let after = take_u32(&mut cur)?;
            let opts = unpack_opts(flags, before, after);
            let pattern = String::from_utf8(take_bytes(&mut cur)?)?;
            Request::Search { opts, pattern }
        }
        b'F' => {
            let limit = take_u32(&mut cur)?;
            let needle = String::from_utf8(take_bytes(&mut cur)?)?;
            let after = String::from_utf8(take_bytes(&mut cur)?)?;
            let after = (!after.is_empty()).then_some(after);
            Request::Find {
                needle,
                after,
                limit,
            }
        }
        b'T' => Request::Status,
        b'W' => Request::Watch,
        b'Q' => Request::Shutdown,
        b'P' => Request::CursorStore {
            blob: take_bytes(&mut cur)?,
        },
        b'G' => Request::CursorTake {
            token: String::from_utf8(take_bytes(&mut cur)?)?,
        },
        other => bail!("unknown request tag {other}"),
    })
}

/// `Find` responses optionally lead with a one-line header
/// `\x01<total>\t<start>\t<returned>\t<next_after>\n` so the client can report the true total (not
/// just the truncated page), the keyset offset (`start` = items skipped before this page, for an
/// honest "X-Y of N" range), and resume via `next_after`. The `0x01` sentinel can't begin a real path
/// line, and older/headerless blobs parse as all-paths.
pub const FIND_HEADER_SENTINEL: u8 = 0x01;

pub struct FindHeader {
    pub total: usize,
    pub start: usize,
    pub returned: usize,
    pub next_after: Option<String>,
}

pub fn format_find_header(
    total: usize,
    start: usize,
    returned: usize,
    next_after: Option<&str>,
) -> String {
    format!(
        "{}{total}\t{start}\t{returned}\t{}\n",
        FIND_HEADER_SENTINEL as char,
        next_after.unwrap_or("")
    )
}

/// Split a `Find` response blob into its optional header and the remaining path lines.
pub fn parse_find_header(blob: &[u8]) -> (Option<FindHeader>, &[u8]) {
    if blob.first() != Some(&FIND_HEADER_SENTINEL) {
        return (None, blob);
    }
    let Some(nl) = blob.iter().position(|&b| b == b'\n') else {
        return (None, blob);
    };
    let line = String::from_utf8_lossy(&blob[1..nl]);
    // `splitn(4)` keeps next_after (a file path) intact even if it contains a tab — the three numeric
    // fields are tab-delimited, and everything after the third tab is the path verbatim.
    let mut parts = line.splitn(4, '\t');
    let total = parts.next().and_then(|s| s.parse().ok());
    let start = parts.next().and_then(|s| s.parse().ok());
    let returned = parts.next().and_then(|s| s.parse().ok());
    match (total, start, returned) {
        (Some(total), Some(start), Some(returned)) => {
            let next_after = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
            (
                Some(FindHeader {
                    total,
                    start,
                    returned,
                    next_after,
                }),
                &blob[nl + 1..],
            )
        }
        _ => (None, blob),
    }
}

/// Responses are a stream of non-empty data frames terminated by a zero-length frame, so the daemon
/// can emit results as it finds them and the client writes them straight to stdout (no buffering of
/// huge result sets on either side).
pub fn write_data(w: &mut impl Write, data: &[u8]) -> Result<()> {
    if !data.is_empty() {
        write_frame(w, data)?;
    }
    Ok(())
}

pub fn end_stream(w: &mut impl Write) -> Result<()> {
    w.write_all(&0u32.to_le_bytes())?;
    w.flush()?;
    Ok(())
}

/// Read a response stream, writing each chunk to `sink`; returns total bytes written.
pub fn read_stream(r: &mut impl Read, sink: &mut impl Write) -> Result<usize> {
    let mut total = 0;
    loop {
        let n = read_len(r)?;
        if n == 0 {
            return Ok(total);
        }
        let mut body = vec![0u8; n];
        r.read_exact(&mut body)?;
        sink.write_all(&body)?;
        total += n;
    }
}

/// Convenience: collect a whole response stream into a `Vec` (for small responses like status/find).
pub fn read_stream_to_vec(r: &mut impl Read) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    read_stream(r, &mut v)?;
    Ok(v)
}

/// Read one frame from an open-ended stream (e.g. `Watch`), returning `None` when the stream ends
/// (zero-length terminator or the daemon closing the connection).
pub fn read_watch_frame(r: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let n = u32::from_le_bytes(len) as usize;
    if n == 0 {
        return Ok(None);
    }
    if n > MAX_FRAME {
        bail!("frame length {n} exceeds maximum {MAX_FRAME}");
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Upper bound on a single frame, so a bogus/desynced length prefix can't trigger a multi-GB
/// allocation. Generous (search results stream in many small frames; requests are tiny).
const MAX_FRAME: usize = 512 * 1024 * 1024;

fn read_len(r: &mut impl Read) -> Result<usize> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        bail!("frame length {n} exceeds maximum {MAX_FRAME}");
    }
    Ok(n)
}

fn write_frame(w: &mut impl Write, body: &[u8]) -> Result<()> {
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(body)?;
    w.flush()?;
    Ok(())
}

fn read_frame(r: &mut impl Read) -> Result<Vec<u8>> {
    let mut body = vec![0u8; read_len(r)?];
    r.read_exact(&mut body)?;
    Ok(body)
}

fn put_bytes(buf: &mut Vec<u8>, b: &[u8]) {
    buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
    buf.extend_from_slice(b);
}

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    let (&b, rest) = cur
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("short frame"))?;
    *cur = rest;
    Ok(b)
}

fn take_u32(cur: &mut &[u8]) -> Result<u32> {
    if cur.len() < 4 {
        bail!("short frame");
    }
    let (head, rest) = cur.split_at(4);
    *cur = rest;
    Ok(u32::from_le_bytes(head.try_into().unwrap()))
}

fn take_bytes(cur: &mut &[u8]) -> Result<Vec<u8>> {
    let n = take_u32(cur)? as usize;
    if cur.len() < n {
        bail!("short frame");
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(req: Request) {
        let mut buf = Vec::new();
        write_request(&mut buf, &req).unwrap();
        let got = read_request(&mut &buf[..]).unwrap();
        assert_eq!(req, got);
    }

    #[test]
    fn request_roundtrips() {
        roundtrip(Request::Search {
            opts: SearchOptions {
                case_insensitive: true,
                ..Default::default()
            },
            pattern: "Foo|Bar".to_string(),
        });
        roundtrip(Request::Find {
            needle: "config".into(),
            after: None,
            limit: 50,
        });
        roundtrip(Request::Find {
            needle: "config".into(),
            after: Some("src/config.rs".into()),
            limit: 50,
        });
        roundtrip(Request::Status);
        roundtrip(Request::Watch);
        roundtrip(Request::Shutdown);
        roundtrip(Request::CursorStore {
            blob: vec![0, 1, 2, 255],
        });
        roundtrip(Request::CursorTake {
            token: "0000abcd5".to_string(),
        });
    }

    #[test]
    fn find_header_roundtrips_and_tolerates_headerless() {
        let blob = format!(
            "{}src/a.rs\nsrc/b.rs\n",
            format_find_header(1342, 200, 2, Some("src/b.rs"))
        );
        let (header, rest) = parse_find_header(blob.as_bytes());
        let header = header.unwrap();
        assert_eq!(header.total, 1342);
        assert_eq!(header.start, 200);
        assert_eq!(header.returned, 2);
        assert_eq!(header.next_after.as_deref(), Some("src/b.rs"));
        assert_eq!(rest, b"src/a.rs\nsrc/b.rs\n");

        // A headerless blob (no sentinel) parses as all paths.
        let (none, rest) = parse_find_header(b"src/a.rs\n");
        assert!(none.is_none());
        assert_eq!(rest, b"src/a.rs\n");
    }

    #[test]
    fn response_stream_roundtrips() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"path:1:hello\n").unwrap();
        write_data(&mut buf, b"").unwrap(); // empty chunk is a no-op, not a terminator
        write_data(&mut buf, b"path:2:world\n").unwrap();
        end_stream(&mut buf).unwrap();
        assert_eq!(
            read_stream_to_vec(&mut &buf[..]).unwrap(),
            b"path:1:hello\npath:2:world\n"
        );
    }
}
