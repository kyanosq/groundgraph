//! JSON-RPC dispatch loop.
//!
//! Owned by [`Server`], which is created once per process and never
//! mutated after construction. Each line of stdin produces at most one
//! line of stdout (notifications produce nothing).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::protocol::{
    Request, Response, ToolCallResult, INVALID_PARAMS, INVALID_REQUEST, JSON_RPC_VERSION,
    MCP_PROTOCOL_VERSION, METHOD_NOT_FOUND, PARSE_ERROR, SERVER_NAME, SERVER_VERSION,
};
use crate::tools;

/// MCP server state.
#[derive(Debug, Clone)]
pub struct Server {
    /// Default repository root for tool calls that do not pass an
    /// explicit `repo_root` argument. Resolved at startup so each tool
    /// handler does not have to know about CLI flags / env vars.
    pub default_repo_root: PathBuf,
}

impl Server {
    pub fn new(default_repo_root: PathBuf) -> Self {
        Self { default_repo_root }
    }

    /// Pump messages between the given reader and writer. Returns when
    /// the reader hits EOF. Designed so tests can pipe in-memory
    /// readers / writers without touching real stdio.
    pub fn pump<R: BufRead, W: Write>(
        &self,
        reader: &mut R,
        writer: &mut W,
    ) -> std::io::Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                return Ok(());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(response_line) = self.dispatch(trimmed) {
                writer.write_all(response_line.as_bytes())?;
                writer.write_all(b"\n")?;
                writer.flush()?;
            }
        }
    }

    /// Parse one JSON-RPC envelope and return the serialized response
    /// line (or `None` for notifications).
    pub fn dispatch(&self, raw: &str) -> Option<String> {
        // JSON-RPC batch arrays are not part of the MCP stdio transport
        // (batching was removed from the MCP spec); name the problem
        // instead of emitting a confusing parse error (issues2.md #57).
        if raw.trim_start().starts_with('[') {
            let resp = Response::error(
                Value::Null,
                INVALID_REQUEST,
                "batch requests are not supported by the MCP stdio transport; \
                 send one JSON-RPC message per line",
            );
            return Some(serialize(&resp));
        }
        let request: Request = match serde_json::from_str(raw) {
            Ok(r) => r,
            Err(err) => {
                let resp = Response::error(
                    Value::Null,
                    PARSE_ERROR,
                    format!("invalid JSON-RPC envelope: {err}"),
                );
                return Some(serialize(&resp));
            }
        };
        // §4: a Request must declare `"jsonrpc": "2.0"` (issues2.md #38).
        if request.jsonrpc != JSON_RPC_VERSION {
            let id = request.id.clone().unwrap_or(Value::Null);
            let resp = Response::error(
                id,
                INVALID_REQUEST,
                format!(
                    "unsupported jsonrpc version `{}` (expected `{JSON_RPC_VERSION}`)",
                    request.jsonrpc
                ),
            );
            return Some(serialize(&resp));
        }
        if request.is_notification() {
            // We accept `notifications/initialized` and other client-
            // notifications silently. Spec forbids any reply. Note: serde
            // cannot distinguish a missing `id` from `"id": null`, so the
            // discouraged `id: null` request form is treated as a
            // notification too — no known MCP client emits it.
            return None;
        }
        let response = self.handle(request);
        Some(serialize(&response))
    }

    fn handle(&self, req: Request) -> Response {
        let id = req.id.clone().unwrap_or(Value::Null);
        match req.method.as_str() {
            "initialize" => Response::success(id, self.initialize_result()),
            "tools/list" => Response::success(id, self.tools_list_result()),
            "tools/call" => self.handle_tools_call(id, &req.params),
            "ping" => Response::success(id, json!({})),
            other => Response::error(
                id,
                METHOD_NOT_FOUND,
                format!("unsupported method `{other}`"),
            ),
        }
    }

    fn initialize_result(&self) -> Value {
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION,
            },
            "capabilities": {
                "tools": {
                    "listChanged": false,
                }
            },
            "instructions": "SpecSlice MCP — agent tools for the SpecSlice code graph. \
                Default repo root is the working directory; every tool accepts a \
                `repo_root` field to override. Tools return structured JSON payloads \
                in a single text content block.",
        })
    }

    fn tools_list_result(&self) -> Value {
        let descriptors = tools::descriptors();
        let tools_arr: Vec<Value> = descriptors
            .iter()
            .map(|d| {
                json!({
                    "name": d.name,
                    "description": d.description,
                    "inputSchema": d.input_schema.clone(),
                })
            })
            .collect();
        json!({ "tools": tools_arr })
    }

    fn handle_tools_call(&self, id: Value, params: &Value) -> Response {
        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return Response::error(id, INVALID_REQUEST, "missing `name` in tools/call params");
            }
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !tools::is_known(&name) {
            return Response::error(
                id,
                INVALID_PARAMS,
                format!("unknown tool `{name}`. call tools/list for the catalogue."),
            );
        }
        let result = match tools::call(self, &name, &arguments) {
            Ok(value) => ToolCallResult::ok_json(&value),
            Err(err) => ToolCallResult::err(format!("{err:#}")),
        };
        let value = serde_json::to_value(&result).unwrap_or_else(|e| {
            json!({
                "content": [{ "type": "text", "text": format!("serialising tool result failed: {e}") }],
                "isError": true,
            })
        });
        Response::success(id, value)
    }
}

fn serialize(response: &Response) -> String {
    serde_json::to_string(response).unwrap_or_else(|e| {
        // The fallback envelope still has to be valid JSON — and stay on
        // ONE line — or we poison the newline-delimited transport.
        format!(
            r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"failed to serialise response: {}"}}}}"#,
            escape_json_string(&e.to_string())
        )
    })
}

/// Escape a string for embedding inside a JSON string literal: quotes,
/// backslashes and every control character (issues.md #24 — the old
/// fallback only handled `"`, so a message containing `\` or a newline
/// produced invalid JSON / broke line framing).
fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape_json_string;
    use super::*;

    #[test]
    fn fallback_escape_keeps_hostile_messages_valid_single_line_json() {
        let hostile = "a \"quote\" and \\backslash\nnewline\ttab\rcr \u{1} ctrl";
        let escaped = escape_json_string(hostile);
        let wrapped = format!(r#"{{"m":"{escaped}"}}"#);
        assert!(
            !wrapped.contains('\n'),
            "a literal newline would break the NDJSON transport: {wrapped:?}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&wrapped).expect("fallback envelope must stay valid JSON");
        assert_eq!(
            parsed["m"].as_str().unwrap(),
            hostile,
            "lossless round-trip"
        );
    }

    fn test_server() -> (Server, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".specslice.yaml"), "{}").unwrap();
        let server = Server::new(dir.path().to_path_buf());
        (server, dir)
    }

    /// issues2.md #38: a wrong `jsonrpc` version must be rejected with
    /// INVALID_REQUEST instead of being processed as if it were 2.0.
    #[test]
    fn dispatch_rejects_wrong_jsonrpc_version() {
        let (server, _dir) = test_server();
        let raw = r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#;
        let line = server.dispatch(raw).expect("a request gets a response");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["code"], INVALID_REQUEST, "got: {line}");
        assert_eq!(v["id"], 1, "error must echo the request id");
    }

    /// issues2.md #57: JSON-RPC batch arrays are not part of the MCP
    /// stdio transport (batching was removed from the MCP spec). The
    /// server must say so clearly, not emit a generic parse error.
    #[test]
    fn dispatch_rejects_batch_arrays_with_a_clear_error() {
        let (server, _dir) = test_server();
        let raw = r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#;
        let line = server.dispatch(raw).expect("batch gets an error response");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["code"], INVALID_REQUEST);
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.to_lowercase().contains("batch"),
            "error must name the problem: {msg}"
        );
    }

    /// Notifications (no `id`) must stay silent — including unknown ones.
    #[test]
    fn dispatch_stays_silent_for_notifications() {
        let (server, _dir) = test_server();
        assert!(server
            .dispatch(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }
}
