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
use crate::filter::FilterSpec;
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
                        start_after: c
                            .last_path
                            .clone()
                            .map(|p| (c.last_order, p, c.last_lineno, c.last_ordinal)),
                        prev: Some((c.prev_total, c.fingerprint)),
                        pattern: c.pattern,
                        opts: c.opts,
                        mode: c.mode,
                        page_size: c.page_size,
                        sort: c.sort,
                        weights: c.weights,
                        filter: c.filter,
                    },
                    Err(e) => return error(id, -32602, &format!("invalid cursor: {e}")),
                }
            } else {
                let Some(pattern) = arg_str(args, "pattern") else {
                    return error(id, -32602, "missing required argument 'pattern'");
                };
                let sort = match arg_str(args, "sort") {
                    Some(v) => match crate::sort::parse(v, arg_bool(args, "reverse")) {
                        Ok(s) => s,
                        Err(e) => return error(id, -32602, &format!("{e}")),
                    },
                    None => crate::sort::SortSpec::default(),
                };
                let weights = arg_str(args, "weights").map(str::to_string);
                if sort.needs_weights() && weights.is_none() {
                    return error(id, -32602, "sort=weight needs weights (label:weight,...)");
                }
                if !sort.needs_weights() && weights.is_some() {
                    return error(id, -32602, "weights applies only to sort=weight");
                }
                Query {
                    pattern: pattern.to_string(),
                    opts: SearchOptions {
                        case_insensitive: arg_bool(args, "case_insensitive"),
                        word: arg_bool(args, "word"),
                        fixed_strings: arg_bool(args, "fixed_strings"),
                        multi_line: arg_bool(args, "multi_line"),
                        invert: arg_bool(args, "invert_match"),
                        hidden: arg_bool(args, "hidden"),
                        no_ignore: arg_bool(args, "no_ignore"),
                        only_matching: arg_bool(args, "only_matching"),
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
                    sort,
                    weights,
                    filter: FilterSpec {
                        globs: arg_str_list(args, "globs"),
                        types: arg_str_list(args, "types"),
                        type_nots: arg_str_list(args, "type_nots"),
                    },
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
    start_after: Option<(i64, String, u64, u32)>,
    page_size: usize,
    /// `(total, fingerprint)` when resuming a cursor, for the staleness note.
    prev: Option<(usize, u32)>,
    /// How to order results (`sort`/`reverse`).
    sort: crate::sort::SortSpec,
    /// The `weights` map for `sort=weight`, or `None`.
    weights: Option<String>,
    /// `-g`/`-t`/`-T` file filter.
    filter: FilterSpec,
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

/// A JSON array of strings argument (e.g. `globs`), or empty when absent.
fn arg_str_list(args: Option<&Value>, key: &str) -> Vec<String> {
    args.and_then(|a| a.get(key))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Run a content search and return the token-savings view: results grouped by file and paged, with a
/// cursor to fetch the next page. Matching is identical to `rg`; only presentation differs (see
/// `compact`). Paging is cheap (warm index), so an agent pulls more on demand rather than dumping all.
fn compact_search(root: &Path, q: Query) -> String {
    // Validate the filter here — a bad glob / unknown type would otherwise be swallowed on the daemon
    // path (the request handler ignores content_search errors).
    if let Err(e) = q.filter.compile(root) {
        return format!("error: {e}");
    }
    // sort=weight: the plain pattern (annotations stripped) is what gets searched; the ranker only
    // reorders the rendered view. Other keys leave the pattern untouched.
    let ranking = match crate::rank::parse(&q.pattern, q.weights.as_deref(), q.opts) {
        Ok(r) => r,
        Err(e) => return format!("error: {e}"),
    };
    let raw = match crate::collect_search(root, &ranking.plain, q.opts, &q.filter) {
        Ok(b) => b,
        Err(e) => return format!("error: {e}"),
    };
    let p = compact::format(
        &raw,
        &ranking.plain,
        q.opts,
        CompactOpts {
            mode: q.mode,
            start_after: q.start_after,
            page_size: q.page_size,
            max_cols: compact::DEFAULT_MAX_COLS,
            sort: q.sort,
            ranker: ranking.ranker,
            root: Some(root.to_path_buf()),
        },
    );
    let mut text = format!("{}\n{}", p.header, p.body);
    if let Some(note) = p.staleness_note(q.prev) {
        text.push_str(&format!("\nnote: {note}"));
    }
    // root_hint is None: the MCP server root is authoritative, so the cursor never carries a path.
    // The blob is parked in this root's daemon; the agent echoes back the short token. On a store
    // failure, still tell the agent more remains so it can't mistake a partial page for the whole.
    if let Some(next) = p.next_cursor(
        q.mode,
        q.pattern,
        q.opts,
        q.filter,
        q.page_size,
        None,
        q.sort,
        q.weights,
    ) {
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
                    "invert_match": {"type": "boolean", "description": "return non-matching lines (-v)"},
                    "only_matching": {"type": "boolean", "description": "return only the matched part of each line (-o)"},
                    "hidden": {"type": "boolean", "description": "also search hidden files/dirs (--hidden)"},
                    "no_ignore": {"type": "boolean", "description": "ignore .gitignore/.ignore rules (--no-ignore)"},
                    "globs": {"type": "array", "items": {"type": "string"}, "description": "include/exclude files by glob (-g); a leading ! negates, e.g. [\"*.rs\", \"!*_test.rs\"]"},
                    "types": {"type": "array", "items": {"type": "string"}, "description": "restrict to file types (-t), e.g. [\"rust\", \"py\"]"},
                    "type_nots": {"type": "array", "items": {"type": "string"}, "description": "exclude file types (-T), e.g. [\"lock\"]"},
                    "files_only": {"type": "boolean", "description": "list matching file paths only (-l)"},
                    "count": {"type": "boolean", "description": "per-file match counts only (-c)"},
                    "page_size": {"type": "integer", "description": "matches (or files, for -l/-c) per page; default 50"},
                    "sort": {"type": "string", "description": "order results by one of: path, modified, accessed, created (file metadata, like ripgrep --sort), or weight (relevance). Reorders only -- the match set is unchanged."},
                    "reverse": {"type": "boolean", "description": "descending sort (like ripgrep --sortr); for sort=weight this puts least-relevant first"},
                    "weights": {"type": "string", "description": "for sort=weight: declare branch weights as label:weight,... (e.g. \"impl:0.7,call:0.3\") and tag regex alternation branches in `pattern` with <label> (e.g. \"fn (process<impl>|process\\\\(<call>)\"). Files are ordered by the weight of the branch they matched (highest first). The <label> tags are stripped before searching, so the match set stays ripgrep's."},
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
