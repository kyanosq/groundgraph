//! JSON-RPC dispatch loop.
//!
//! Owned by [`Server`], which is created once per process and never
//! mutated after construction. Each line of stdin produces at most one
//! line of stdout (notifications produce nothing).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::protocol::{
    Request, Response, ToolCallResult, INVALID_PARAMS, INVALID_REQUEST, MCP_PROTOCOL_VERSION,
    METHOD_NOT_FOUND, PARSE_ERROR, SERVER_NAME, SERVER_VERSION,
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
        if request.is_notification() {
            // We accept `notifications/initialized` and other client-
            // notifications silently. Spec forbids any reply.
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
        // The fallback envelope still has to be valid JSON or we
        // poison the transport.
        format!(
            r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"failed to serialise response: {}"}}}}"#,
            e.to_string().replace('"', "\\\"")
        )
    })
}
