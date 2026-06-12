//! Minimal MCP stdio server exposing rgx's search to AI agents.
//!
//! Speaks JSON-RPC 2.0 over stdio (newline-delimited), implementing the handshake plus three tools:
//! `content_search`, `file_search`, and `status`. Results come back as ripgrep-style text (the shape
//! models already know), per `docs/mcp.md`. Parsing uses `serde_json` so UTF-8, escapes, and key
//! order are handled correctly.

use std::io::{BufRead, Write};
use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};

use crate::client;
use crate::compact::{self, CompactOpts};
use crate::confirm::SearchOptions;
use crate::cursor::{self, Mode};
use crate::proto::Request;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the MCP server rooted at `root` until stdin closes.
pub fn run(root: PathBuf) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle_message(line.trim(), &root) {
            writeln!(stdout, "{resp}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Handle one JSON-RPC message; returns a response line, or `None` for notifications / unparseable
/// input.
fn handle_message(msg: &str, root: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(msg).ok()?;
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    let method = v.get("method")?.as_str()?;
    match method {
        "initialize" => Some(result(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "rgx", "version": env!("CARGO_PKG_VERSION")},
            }),
        )),
        "tools/list" => Some(result(id, tools())),
        "tools/call" => Some(handle_tool_call(id, &v, root)),
        m if m.starts_with("notifications/") => None,
        _ => {
            if id.is_null() {
                None
            } else {
                Some(error(id, -32601, "method not found"))
            }
        }
    }
}

fn handle_tool_call(id: Value, msg: &Value, root: &Path) -> String {
    let params = msg.get("params");
    let name = params.and_then(|p| p.get("name")).and_then(Value::as_str);
    let args = params.and_then(|p| p.get("arguments"));
    match name {
        Some("content_search") => {
            // A cursor carries the entire query + resume position, so it supersedes the other args.
            let query = if let Some(tok) = arg_str(args, "cursor") {
                let blob = match client::take_cursor(root, tok) {
                    Ok(Some(blob)) => blob,
                    Ok(None) => {
                        return error(id, -32602, "pagination expired — re-run the search");
                    }
                    Err(e) => return error(id, -32603, &format!("{e}")),
                };
                match cursor::decode(&blob) {
                    Ok(c) => Query {
                        start_after: c.last_path.clone().map(|p| (p, c.last_lineno)),
                        prev: Some((c.prev_total, c.fingerprint)),
                        pattern: c.pattern,
                        opts: c.opts,
                        mode: c.mode,
                        page_size: c.page_size,
                    },
                    Err(e) => return error(id, -32602, &format!("invalid cursor: {e}")),
                }
            } else {
                let Some(pattern) = arg_str(args, "pattern") else {
                    return error(id, -32602, "missing required argument 'pattern'");
                };
                Query {
                    pattern: pattern.to_string(),
                    opts: SearchOptions {
                        case_insensitive: arg_bool(args, "case_insensitive"),
                        word: arg_bool(args, "word"),
                        fixed_strings: arg_bool(args, "fixed_strings"),
                        multi_line: arg_bool(args, "multi_line"),
                        ..Default::default()
                    },
                    mode: if arg_bool(args, "count") {
                        Mode::Count
                    } else if arg_bool(args, "files_only") {
                        Mode::Files
                    } else {
                        Mode::Matches
                    },
                    start_after: None,
                    page_size: arg_usize(args, "page_size").unwrap_or(compact::DEFAULT_PAGE_SIZE),
                    prev: None,
                }
            };
            tool_result(id, &compact_search(root, query))
        }
        Some("file_search") => {
            let Some(query) = arg_str(args, "query") else {
                return error(id, -32602, "missing required argument 'query'");
            };
            let limit = arg_usize(args, "limit").unwrap_or(200) as u32;
            let after = arg_str(args, "after").map(str::to_string);
            tool_result(id, &file_search(root, query, after, limit))
        }
        Some("status") => tool_result(id, &run_request(root, &Request::Status)),
        Some(other) => error(id, -32602, &format!("unknown tool {other:?}")),
        None => error(id, -32602, "missing tool name"),
    }
}

/// A resolved content query: from explicit args, or unpacked from a `cursor`.
struct Query {
    pattern: String,
    opts: SearchOptions,
    mode: Mode,
    start_after: Option<(String, u64)>,
    page_size: usize,
    /// `(total, fingerprint)` when resuming a cursor, for the staleness note.
    prev: Option<(usize, u32)>,
}

fn arg_bool(args: Option<&Value>, key: &str) -> bool {
    args.and_then(|a| a.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn arg_usize(args: Option<&Value>, key: &str) -> Option<usize> {
    args.and_then(|a| a.get(key))
        .and_then(Value::as_u64)
        .map(|n| n as usize)
}

fn arg_str<'a>(args: Option<&'a Value>, key: &str) -> Option<&'a str> {
    args.and_then(|a| a.get(key)).and_then(Value::as_str)
}

/// Run a content search and return the token-savings view: results grouped by file and paged, with a
/// cursor to fetch the next page. Matching is identical to `rg`; only presentation differs (see
/// `compact`). Paging is cheap (warm index), so an agent pulls more on demand rather than dumping all.
fn compact_search(root: &Path, q: Query) -> String {
    let raw = match crate::collect_search(root, &q.pattern, q.opts) {
        Ok(b) => b,
        Err(e) => return format!("error: {e}"),
    };
    let p = compact::format(
        &raw,
        &q.pattern,
        q.opts,
        CompactOpts {
            mode: q.mode,
            start_after: q.start_after,
            page_size: q.page_size,
            max_cols: compact::DEFAULT_MAX_COLS,
        },
    );
    let mut text = format!("{}\n{}", p.header, p.body);
    if let Some(note) = p.staleness_note(q.prev) {
        text.push_str(&format!("\nnote: {note}"));
    }
    // root_hint is None: the MCP server root is authoritative, so the cursor never carries a path.
    // The blob is parked in this root's daemon; the agent echoes back the short token. On a store
    // failure, still tell the agent more remains so it can't mistake a partial page for the whole.
    if let Some(next) = p.next_cursor(q.mode, q.pattern, q.opts, q.page_size, None) {
        match client::store_cursor(root, cursor::encode(&next)) {
            Ok(token) => text.push_str(&format!(
                "\n(more: call content_search with cursor: \"{token}\")"
            )),
            Err(e) => text.push_str(&format!(
                "\nnote: more results exist but the pagination cursor could not be stored ({e}); \
                 re-run the search"
            )),
        }
    }
    text
}

/// File-name search returning the token-savings shape: a `[files X-Y of N]` header, one path per
/// line, and a hint to fetch more (the daemon reports the true total and the keyset resume key).
fn file_search(root: &Path, query: &str, after: Option<String>, limit: u32) -> String {
    let bytes = match client::request(
        root,
        &Request::Find {
            needle: query.to_string(),
            after,
            limit,
        },
    ) {
        Ok(b) => b,
        Err(e) => return format!("error: {e}"),
    };
    let (header, body) = crate::proto::parse_find_header(&bytes);
    let body = String::from_utf8_lossy(body);
    let Some(h) = header else {
        return body.into_owned();
    };
    let first = if h.returned == 0 { 0 } else { h.start + 1 };
    let mut text = format!(
        "[files {first}-{} of {}]\n{body}",
        h.start + h.returned,
        h.total
    );
    if let Some(next) = h.next_after {
        text.push_str(&format!(
            "\n(more: call file_search with after: \"{next}\")"
        ));
    }
    text
}

fn run_request(root: &Path, req: &Request) -> String {
    match client::request(root, req) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => format!("error: {e}"),
    }
}

fn tools() -> Value {
    json!({"tools": [
        {
            "name": "content_search",
            "description": concat!(
                "Search file contents with a regex (ripgrep semantics, accelerated by an index). ",
                "Results are grouped by file and paged: the match set is identical to ripgrep, nothing ",
                "is dropped. The header reports the total match/file count, so you know how much you ",
                "have NOT seen; when more remains, fetch it by passing the opaque `cursor` from the ",
                "response (it carries the exact same query, so the next page can't drift). Paging is ",
                "cheap (the index is warm). For a quick sense of scope, use `files_only` (paths only) ",
                "or `count` (per-file counts) instead of a page-walk. Long lines are trimmed around ",
                "the match (read the file for the full line)."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "required for a new search; omit when paging via cursor"},
                    "case_insensitive": {"type": "boolean"},
                    "word": {"type": "boolean", "description": "match only whole words (-w)"},
                    "fixed_strings": {"type": "boolean", "description": "treat pattern as a literal (-F)"},
                    "multi_line": {"type": "boolean"},
                    "files_only": {"type": "boolean", "description": "list matching file paths only (-l)"},
                    "count": {"type": "boolean", "description": "per-file match counts only (-c)"},
                    "page_size": {"type": "integer", "description": "matches (or files, for -l/-c) per page; default 50"},
                    "cursor": {"type": "string", "description": "opaque token from a previous response; fetches the next page and supersedes all other args"}
                }
            }
        },
        {
            "name": "file_search",
            "description": concat!(
                "Find files/directories by name or path substring (fd/find-style). Returns a header ",
                "with the true total, then one path per line; when more remain than the page holds, ",
                "the response gives an `after` key to fetch the next page."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "description": "max paths per page; default 200"},
                    "after": {"type": "string", "description": "resume key from a previous response (keyset paging)"}
                },
                "required": ["query"]
            }
        },
        {
            "name": "status",
            "description": "Report index health: whether it is ready, file and trigram counts.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ]})
}

fn result(id: Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn error(id: Value, code: i32, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn tool_result(id: Value, text: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}]}})
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_unicode_pattern_parse() {
        let resp = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            Path::new("."),
        )
        .unwrap();
        assert!(resp.contains("\"protocolVersion\""));
        // A non-ASCII pattern must round-trip through the parser unmolested.
        let v: Value = serde_json::from_str(
            r#"{"params":{"name":"content_search","arguments":{"pattern":"café"}}}"#,
        )
        .unwrap();
        let pat = v["params"]["arguments"]["pattern"].as_str().unwrap();
        assert_eq!(pat, "café");
    }

    #[test]
    fn content_search_advertises_cursor_paging() {
        let listed = tools().to_string();
        assert!(listed.contains("content_search"));
        assert!(listed.contains("\"cursor\""));
        assert!(listed.contains("\"page_size\""));
        assert!(!listed.contains("\"page\""));
    }

    #[test]
    fn file_search_advertises_keyset_paging() {
        let listed = tools().to_string();
        assert!(listed.contains("file_search"));
        assert!(listed.contains("\"after\""));
        assert!(listed.contains("\"limit\""));
    }

    #[test]
    fn notifications_get_no_response() {
        assert!(
            handle_message(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                Path::new(".")
            )
            .is_none()
        );
    }
}
