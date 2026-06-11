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
            let Some(pattern) = args.and_then(|a| a.get("pattern")).and_then(Value::as_str) else {
                return error(id, -32602, "missing required argument 'pattern'");
            };
            let opts = SearchOptions {
                case_insensitive: arg_bool(args, "case_insensitive"),
                word: arg_bool(args, "word"),
                fixed_strings: arg_bool(args, "fixed_strings"),
                multi_line: arg_bool(args, "multi_line"),
                ..Default::default()
            };
            let page = arg_usize(args, "page").unwrap_or(1);
            tool_result(id, &compact_search(root, pattern, opts, page))
        }
        Some("file_search") => {
            let Some(query) = args.and_then(|a| a.get("query")).and_then(Value::as_str) else {
                return error(id, -32602, "missing required argument 'query'");
            };
            tool_result(
                id,
                &run_request(
                    root,
                    &Request::Find {
                        needle: query.to_string(),
                        limit: 200,
                    },
                ),
            )
        }
        Some("status") => tool_result(id, &run_request(root, &Request::Status)),
        Some(other) => error(id, -32602, &format!("unknown tool {other:?}")),
        None => error(id, -32602, "missing tool name"),
    }
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

/// Run a content search and return the token-savings view: results grouped by file and paged, with a
/// hint to fetch the next page. Matching is identical to `rg`; only presentation differs (see
/// `compact`). Paging is cheap (warm index), so an agent pulls more on demand rather than dumping all.
fn compact_search(root: &Path, pattern: &str, opts: SearchOptions, page: usize) -> String {
    let raw = match crate::collect_search(root, pattern, opts) {
        Ok(b) => b,
        Err(e) => return format!("error: {e}"),
    };
    let p = compact::format(
        &raw,
        pattern,
        opts,
        CompactOpts {
            page,
            ..Default::default()
        },
    );
    let mut text = format!("{}\n{}", p.header, p.body);
    if p.has_more() {
        text.push_str(&format!(
            "\n(more: call content_search with page: {})",
            p.page + 1
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
                "is dropped. Paging is cheap (the index is warm), so narrow the pattern when you can, ",
                "but prefer fetching the next page over a broad dump. Long lines are trimmed around the ",
                "match (read the file for the full line)."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "case_insensitive": {"type": "boolean"},
                    "word": {"type": "boolean", "description": "match only whole words (-w)"},
                    "fixed_strings": {"type": "boolean", "description": "treat pattern as a literal (-F)"},
                    "multi_line": {"type": "boolean"},
                    "page": {"type": "integer", "description": "1-based page number; the response tells you when more pages exist"}
                },
                "required": ["pattern"]
            }
        },
        {
            "name": "file_search",
            "description": "Find files/directories by name or path substring (fd/find-style). Returns one path per line.",
            "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}
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
    fn content_search_advertises_paging() {
        let listed = tools().to_string();
        assert!(listed.contains("content_search"));
        assert!(listed.contains("\"page\""));
        assert!(listed.contains("page number"));
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
