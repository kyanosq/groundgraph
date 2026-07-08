//! Minimal JSON-RPC 2.0 + MCP wire envelopes.
//!
//! Spec references:
//! - JSON-RPC 2.0: <https://www.jsonrpc.org/specification>
//! - MCP transports (stdio newline-delimited JSON): see the protocol
//!   spec page on modelcontextprotocol.io.
//!
//! Only the subset we actually emit / accept is modelled. Anything
//! exotic is left as a raw [`serde_json::Value`] so the dispatcher can
//! forward it to handlers without coupling to a frozen schema.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC protocol version we speak.
pub const JSON_RPC_VERSION: &str = "2.0";

/// MCP protocol revision we advertise in `initialize` when the client does
/// not request one. The 2024-11-05 revision is the baseline every current MCP
/// client (Cursor, Claude Desktop, Continue) understands; newer clients
/// request a later revision and we echo it back (see
/// [`SUPPORTED_PROTOCOL_VERSIONS`]).
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Protocol revisions this stdio server speaks (#104). Our wire usage —
/// `tools/list` returning name/description/inputSchema and `tools/call`
/// returning text content blocks with `isError` — is a strict subset valid in
/// all of these, and we already reject the JSON-RPC batching removed in the
/// later revisions. When a client requests one of these in `initialize` we
/// echo it instead of silently downgrading the handshake to the baseline.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// Server identity returned by `initialize.result.serverInfo`.
pub const SERVER_NAME: &str = "groundgraph-mcp";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Standard JSON-RPC error codes (see §5.1).
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// Inbound JSON-RPC message. The `id` field is `Option` because
/// notifications (no response expected) carry no id.
#[derive(Debug, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    /// `true` when the message is a notification — i.e. carries no
    /// id, so the server MUST NOT emit a response.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION,
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// MCP `tools/list` entry. We hand-roll JSON Schema for each tool's
/// `inputSchema` so consumers (Cursor's tool picker, the model's tool
/// call planner) can validate args before invoking.
#[derive(Debug, Serialize)]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// MCP `tools/call` result envelope. We always return one `text` block
/// whose body is a pretty-printed JSON document — the only reliable
/// shape every MCP client renders consistently.
#[derive(Debug, Serialize)]
pub struct ToolCallResult {
    pub content: Vec<ToolContentBlock>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContentBlock {
    Text { text: String },
}

/// Upper bound on a successful tool result's serialised size. A `get_subgraph
/// --depth 10` on a large graph can return hundreds of thousands of edges;
/// serialising that allocates a multi-MiB string the MCP client would truncate
/// anyway, so we refuse it instead of paying the cost (#209). The error carries
/// only sizes — never a path — so it is safe to return verbatim.
pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 1024 * 1024;

impl ToolCallResult {
    pub fn ok_json(value: &Value) -> Self {
        // Pretty-printing keeps the response readable when an agent
        // surfaces it in chat. `serde_json::to_string_pretty` cannot
        // fail on a `Value`.
        let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        if text.len() > MAX_TOOL_RESULT_BYTES {
            return Self::err(format!(
                "tool result is {} bytes, over the {} byte cap (#209); \
                 narrow the query (smaller depth or limit) and retry",
                text.len(),
                MAX_TOOL_RESULT_BYTES
            ));
        }
        Self {
            content: vec![ToolContentBlock::Text { text }],
            is_error: false,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContentBlock::Text {
                text: message.into(),
            }],
            is_error: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text_of(r: &ToolCallResult) -> &str {
        match &r.content[0] {
            ToolContentBlock::Text { text } => text,
        }
    }

    #[test]
    fn ok_json_returns_small_payloads_verbatim() {
        let r = ToolCallResult::ok_json(&json!({ "hello": "world" }));
        assert!(!r.is_error);
        assert!(text_of(&r).contains("\"hello\""));
    }

    /// #209: a result whose serialised form exceeds the cap is refused as an
    /// error rather than shipped, and the refusal leaks no filesystem path.
    #[test]
    fn ok_json_refuses_payloads_over_the_size_cap() {
        let big: Vec<String> = (0..40_000)
            .map(|i| format!("edge-{i:08}-padding-so-each-row-is-reasonably-wide"))
            .collect();
        let value = serde_json::to_value(big).unwrap();
        assert!(
            serde_json::to_string_pretty(&value).unwrap().len() > MAX_TOOL_RESULT_BYTES,
            "fixture must exceed the cap for the test to be meaningful",
        );
        let r = ToolCallResult::ok_json(&value);
        assert!(r.is_error, "an over-cap result must be flagged as an error");
        let t = text_of(&r);
        assert!(
            t.contains("over the"),
            "message should explain the cap: {t}"
        );
        assert!(!t.contains('/'), "cap message must not leak any path: {t}");
    }
}
