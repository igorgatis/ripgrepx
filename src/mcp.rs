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
            tool_result(
                id,
                &run_request(
                    root,
                    &Request::Search {
                        opts,
                        pattern: pattern.to_string(),
                    },
                ),
            )
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
            "description": "Search file contents with a regex (ripgrep semantics, accelerated by an index). Returns path:line:text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "case_insensitive": {"type": "boolean"},
                    "word": {"type": "boolean", "description": "match only whole words (-w)"},
                    "fixed_strings": {"type": "boolean", "description": "treat pattern as a literal (-F)"},
                    "multi_line": {"type": "boolean"}
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
