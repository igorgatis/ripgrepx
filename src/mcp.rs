//! Minimal MCP stdio server exposing rgx's search to AI agents.
//!
//! Speaks JSON-RPC 2.0 over stdio (newline-delimited), implementing the handshake plus three tools:
//! `content_search`, `file_search`, and `status`. Results come back as ripgrep-style text (the shape
//! models already know), per `docs/mcp.md`. Hand-rolled to avoid a heavy MCP dependency for now.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;

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
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(resp) = handle_message(trimmed, &root) {
            writeln!(stdout, "{resp}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Handle one JSON-RPC message; returns a response line, or `None` for notifications.
fn handle_message(msg: &str, root: &std::path::Path) -> Option<String> {
    let id = extract_raw(msg, "\"id\"");
    let method = extract_string(msg, "\"method\"")?;
    match method.as_str() {
        "initialize" => Some(result(
            &id?,
            &format!(
                r#"{{"protocolVersion":"{PROTOCOL_VERSION}","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"rgx","version":"{}"}}}}"#,
                env!("CARGO_PKG_VERSION")
            ),
        )),
        "tools/list" => Some(result(&id?, TOOLS)),
        "tools/call" => Some(handle_tool_call(&id?, msg, root)),
        "notifications/initialized" | "notifications/cancelled" => None,
        _ => id.map(|id| error(&id, -32601, "method not found")),
    }
}

fn handle_tool_call(id: &str, msg: &str, root: &std::path::Path) -> String {
    let name = match extract_string(msg, "\"name\"") {
        Some(n) => n,
        None => return error(id, -32602, "missing tool name"),
    };
    let text = match name.as_str() {
        "content_search" => {
            let Some(pattern) = extract_string(msg, "\"pattern\"") else {
                return error(id, -32602, "missing pattern");
            };
            let opts = SearchOptions {
                case_insensitive: extract_bool(msg, "\"case_insensitive\""),
                ..Default::default()
            };
            run_request(root, &Request::Search { opts, pattern })
        }
        "file_search" => {
            let Some(needle) =
                extract_string(msg, "\"name\"").filter(|_| msg.contains("\"arguments\""))
            else {
                return error(id, -32602, "missing name");
            };
            // `name` here is the tool name; the query arg is `query`.
            let query = extract_string(msg, "\"query\"").unwrap_or(needle);
            run_request(
                root,
                &Request::Find {
                    needle: query,
                    limit: 200,
                },
            )
        }
        "status" => run_request(root, &Request::Status),
        other => return error(id, -32602, &format!("unknown tool {other}")),
    };
    tool_result(id, &text)
}

fn run_request(root: &std::path::Path, req: &Request) -> String {
    match client::request(root, req) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => format!("error: {e}"),
    }
}

const TOOLS: &str = r#"{"tools":[
{"name":"content_search","description":"Search file contents with a regex (ripgrep semantics, accelerated by an index). Returns path:line:text.","inputSchema":{"type":"object","properties":{"pattern":{"type":"string"},"case_insensitive":{"type":"boolean"}},"required":["pattern"]}},
{"name":"file_search","description":"Find files/directories by name or path substring (fd/find-style). Returns one path per line.","inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}},
{"name":"status","description":"Report index health: whether it is ready, file and trigram counts.","inputSchema":{"type":"object","properties":{}}}
]}"#;

// --- tiny JSON helpers (sufficient for the well-formed messages MCP clients send) ---

fn result(id: &str, result_json: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result_json}}}"#)
}

fn error(id: &str, code: i32, message: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":{}}}}}"#,
        json_string(message)
    )
}

fn tool_result(id: &str, text: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"content":[{{"type":"text","text":{}}}]}}}}"#,
        json_string(text)
    )
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Extract a string value for `key` (e.g. `"method"`) from a flat JSON object. Minimal and
/// tolerant: finds the key, the following `:`, and the next quoted string, unescaping basic escapes.
fn extract_string(msg: &str, key: &str) -> Option<String> {
    let start = msg.find(key)? + key.len();
    let rest = &msg[start..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let q = after.find('"')?;
    let bytes = &after.as_bytes()[q + 1..];
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some(out),
            b'\\' if i + 1 < bytes.len() => {
                i += 1;
                out.push(match bytes[i] {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    other => other as char,
                });
            }
            b => out.push(b as char),
        }
        i += 1;
    }
    None
}

/// Extract the raw token after `key` (used for the JSON-RPC id, which may be a number or string).
fn extract_raw(msg: &str, key: &str) -> Option<String> {
    let start = msg.find(key)? + key.len();
    let rest = &msg[start..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    if let Some(stripped) = after.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(format!("\"{}\"", &stripped[..end]))
    } else {
        let end = after.find([',', '}']).unwrap_or(after.len());
        Some(after[..end].trim().to_string())
    }
}

fn extract_bool(msg: &str, key: &str) -> bool {
    msg.find(key)
        .and_then(|p| {
            msg[p + key.len()..]
                .find(':')
                .map(|c| p + key.len() + c + 1)
        })
        .map(|i| msg[i..].trim_start().starts_with("true"))
        .unwrap_or(false)
}
