//! JSON-RPC dispatch loop.
//!
//! Owned by [`Server`], which is created once per process and never
//! mutated after construction. Each line of stdin produces at most one
//! line of stdout (notifications produce nothing).

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::protocol::{
    Request, Response, ToolCallResult, INVALID_PARAMS, INVALID_REQUEST, JSON_RPC_VERSION,
    MCP_PROTOCOL_VERSION, METHOD_NOT_FOUND, PARSE_ERROR, SERVER_NAME, SERVER_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::tools;

/// Upper bound on a single JSON-RPC line read from the stdio transport. MCP
/// messages are small (a tool name plus a JSON argument object); a multi-MB
/// line is already pathological. A malicious or broken client streaming an
/// unbounded line with no `\n` would otherwise make `read_line` grow its
/// buffer until the process is OOM-killed (#107). 16 MiB is generous for any
/// legitimate request and still fatal to a runaway stream.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Page size for `tools/list` (#86). MCP defines optional `cursor` /
/// `nextCursor` pagination so a large catalogue never has to ship in one
/// JSON-RPC line (some clients cap `tools/list` at ~256 KB). Our seven tools
/// fit in a single page, so today's clients see no `nextCursor` and behaviour
/// is unchanged; the machinery is here so the documented future `candidate_*`
/// tools cannot silently blow a client's response-size limit.
const TOOLS_PAGE_SIZE: usize = 100;

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
        let mut buf: Vec<u8> = Vec::new();
        loop {
            match read_line_capped(reader, MAX_LINE_BYTES, &mut buf) {
                Ok(None) => return Ok(()), // EOF
                Ok(Some(_)) => {}
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    // A client is streaming a line past the size budget (#107).
                    // Refuse with a protocol error and stop the loop rather than
                    // let the read buffer grow until the process is OOM-killed.
                    let resp = Response::error(Value::Null, INVALID_REQUEST, e.to_string());
                    writer.write_all(serialize(&resp).as_bytes())?;
                    writer.write_all(b"\n")?;
                    writer.flush()?;
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
            let line = String::from_utf8_lossy(&buf);
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
            "initialize" => Response::success(id, self.initialize_result(&req.params)),
            "tools/list" => self.handle_tools_list(id, &req.params),
            "tools/call" => self.handle_tools_call(id, &req.params),
            "ping" => Response::success(id, json!({})),
            other => Response::error(
                id,
                METHOD_NOT_FOUND,
                format!("unsupported method `{other}`"),
            ),
        }
    }

    fn initialize_result(&self, params: &Value) -> Value {
        // Negotiate the protocol revision against what the client asked for
        // instead of always answering the baseline (#104).
        let requested = params.get("protocolVersion").and_then(|v| v.as_str());
        let negotiated = negotiate_protocol_version(requested);
        // Record who connected, on which revision, to stderr (the log channel
        // for a stdio server — stdout carries the protocol). Only when the
        // client actually sent identity/version so a bare `initialize` from a
        // test harness stays quiet.
        let client_info = params.get("clientInfo");
        if requested.is_some() || client_info.is_some() {
            let name = client_info
                .and_then(|c| c.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            let version = client_info
                .and_then(|c| c.get("version"))
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            eprintln!(
                "specslice-mcp: client {name} v{version} connected (requested protocol {}, negotiated {negotiated})",
                requested.unwrap_or("<none>")
            );
        }
        json!({
            "protocolVersion": negotiated,
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

    /// Serialise the full tool catalogue as MCP `tools/list` entries.
    fn tool_entries(&self) -> Vec<Value> {
        tools::descriptors()
            .iter()
            .map(|d| {
                json!({
                    "name": d.name,
                    "description": d.description,
                    "inputSchema": d.input_schema.clone(),
                })
            })
            .collect()
    }

    fn handle_tools_list(&self, id: Value, params: &Value) -> Response {
        // `cursor` is an opaque string per the MCP spec; reject a non-string,
        // non-null cursor as a parameter error rather than ignoring it (#86).
        let cursor = match params.get("cursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.as_str()),
            Some(_) => {
                return Response::error(id, INVALID_PARAMS, "`cursor` must be a string");
            }
        };
        let all = self.tool_entries();
        match paginate_tools(&all, cursor, TOOLS_PAGE_SIZE) {
            Ok((page, next_cursor)) => {
                let mut result = json!({ "tools": page });
                // `nextCursor` is omitted (not null) on the last page so a
                // client knows the listing is complete.
                if let Some(c) = next_cursor {
                    result["nextCursor"] = Value::String(c);
                }
                Response::success(id, result)
            }
            Err(msg) => Response::error(id, INVALID_PARAMS, msg),
        }
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
        // Enforce the tool's advertised `inputSchema` (#89). Previously the
        // dispatcher trusted the client: a wrong-typed / undeclared field was
        // silently dropped, so `additionalProperties:false` and `required`
        // were unenforced. A contract violation is a parameter error, not a
        // tool runtime error, so surface it as -32602 rather than `isError`.
        if let Err(msg) = tools::validate_call_arguments(&name, &arguments) {
            return Response::error(
                id,
                INVALID_PARAMS,
                format!("invalid arguments for tool `{name}`: {msg}"),
            );
        }
        let result = match tools::call(self, &name, &arguments) {
            Ok(value) => ToolCallResult::ok_json(&value),
            // Error chains from the store / IO layers embed absolute host
            // paths; strip them before they cross the MCP boundary (#210).
            Err(err) => {
                ToolCallResult::err(redact_paths(&self.default_repo_root, &format!("{err:#}")))
            }
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

/// Read one `\n`-terminated line into `buf`, consuming at most `max` bytes.
/// Returns `Ok(None)` at EOF, `Ok(Some(n))` for a line (the trailing newline is
/// kept when present), or an `InvalidData` error when the line reaches `max`
/// bytes without a newline — the caller turns that into a protocol error
/// instead of buffering unbounded input (#107). Unlike `BufRead::read_line`,
/// the buffer can never grow past `max`.
fn read_line_capped<R: BufRead>(
    reader: &mut R,
    max: usize,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<usize>> {
    buf.clear();
    let n = reader.by_ref().take(max as u64).read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() >= max && !buf.ends_with(b"\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "request line exceeds the {max}-byte limit; refusing to buffer unbounded input"
            ),
        ));
    }
    Ok(Some(n))
}

/// Pick the protocol revision to advertise in `initialize` (#104). When the
/// client requests a revision we support, echo it; otherwise (missing or
/// unknown) answer the most-compatible baseline. Returning a supported version
/// for an unknown request follows the MCP handshake rule "respond with another
/// protocol version it supports".
fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(req) => SUPPORTED_PROTOCOL_VERSIONS
            .into_iter()
            .find(|v| *v == req)
            .unwrap_or(MCP_PROTOCOL_VERSION),
        None => MCP_PROTOCOL_VERSION,
    }
}

/// Slice `all` into a page starting at the opaque `cursor` (the next start
/// index encoded as a decimal string). Returns the page plus the cursor for
/// the following page (`None` on the last page). A cursor that does not parse
/// or points past the end is a client error (#86). The cursor format is an
/// implementation detail — clients must treat it as opaque.
fn paginate_tools(
    all: &[Value],
    cursor: Option<&str>,
    page_size: usize,
) -> Result<(Vec<Value>, Option<String>), String> {
    let start = match cursor {
        None => 0,
        Some(s) => {
            let n: usize = s
                .parse()
                .map_err(|_| format!("invalid `cursor` `{s}`: not a valid pagination token"))?;
            if n > all.len() {
                return Err(format!(
                    "invalid `cursor` `{s}`: past the end of the listing"
                ));
            }
            n
        }
    };
    let end = start.saturating_add(page_size).min(all.len());
    let page = all[start..end].to_vec();
    let next_cursor = if end < all.len() {
        Some(end.to_string())
    } else {
        None
    };
    Ok((page, next_cursor))
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

/// Strip host filesystem paths out of a tool error before it crosses the MCP
/// boundary (#210). Store / IO error chains embed absolute paths such as
/// `/Users/alice/Code/proj/.specslice/graph.db` or CI's
/// `/home/runner/work/...`; a remote client has no business learning the
/// server's directory layout or the operator's username. Two passes:
///   1. the configured repo root → `<repo-root>` (mirrors the dashboard, #40),
///   2. any residual `/Users/<name>` or `/home/<name>` home prefix → `<home>`,
///      catching cargo/rustup/temp paths and overridden repo roots.
fn redact_paths(repo_root: &Path, msg: &str) -> String {
    let mut out = msg.to_string();
    let root = repo_root.to_string_lossy();
    if !root.is_empty() {
        out = out.replace(root.as_ref(), "<repo-root>");
    }
    mask_home_prefixes(&out)
}

/// Length of a leading Unix home prefix (`/Users/<name>` or `/home/<name>`) at
/// the start of `s`, or `None`. Only the `<name>` component is consumed (it
/// ends at the next `/`, whitespace, or end of string), so the rest of the
/// path is preserved for context.
fn home_prefix_len(s: &str) -> Option<usize> {
    for base in ["/Users/", "/home/"] {
        if let Some(after) = s.strip_prefix(base) {
            let seg = after
                .find(|c: char| c == '/' || c.is_whitespace())
                .unwrap_or(after.len());
            if seg > 0 {
                return Some(base.len() + seg);
            }
        }
    }
    None
}

/// Replace every `/Users/<name>` / `/home/<name>` prefix with `<home>`,
/// scanning UTF-8-safely so a multibyte username cannot split a char.
fn mask_home_prefixes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(n) = home_prefix_len(rest) {
            out.push_str("<home>");
            rest = &rest[n..];
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
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

    /// #107: a runaway line with no newline must be refused at the cap, never
    /// buffered unbounded. Normal lines and EOF behave as before.
    #[test]
    fn read_line_capped_refuses_overlong_lines() {
        use std::io::Cursor;
        let mut buf = Vec::new();

        // Normal line: returned with its trailing newline.
        let mut r = Cursor::new(b"hello\nworld\n".to_vec());
        assert_eq!(read_line_capped(&mut r, 64, &mut buf).unwrap(), Some(6));
        assert_eq!(&buf, b"hello\n");

        // Over-long line (no newline within the budget) → InvalidData, and the
        // buffer never exceeds the cap.
        let mut r = Cursor::new(vec![b'a'; 4096]);
        let err = read_line_capped(&mut r, 64, &mut buf).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(buf.len() <= 64, "buffer must stay capped: {}", buf.len());

        // EOF → None.
        let mut r = Cursor::new(Vec::<u8>::new());
        assert_eq!(read_line_capped(&mut r, 64, &mut buf).unwrap(), None);
    }

    /// #104: the handshake echoes a supported client protocol revision and
    /// falls back to the baseline for a missing or unknown one.
    #[test]
    fn initialize_negotiates_protocol_version() {
        assert_eq!(negotiate_protocol_version(None), MCP_PROTOCOL_VERSION);
        assert_eq!(negotiate_protocol_version(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate_protocol_version(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate_protocol_version(Some("2024-11-05")), "2024-11-05");
        // An unknown / future revision falls back to a version we support.
        assert_eq!(
            negotiate_protocol_version(Some("2099-01-01")),
            MCP_PROTOCOL_VERSION
        );

        // End-to-end through the dispatcher: a newer client gets its revision
        // echoed, a bare initialize keeps the baseline.
        let (server, _dir) = test_server();
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"cursor","version":"1.0"}}}"#;
        let v: serde_json::Value = serde_json::from_str(&server.dispatch(raw).unwrap()).unwrap();
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18", "got: {v}");

        let raw = r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}"#;
        let v: serde_json::Value = serde_json::from_str(&server.dispatch(raw).unwrap()).unwrap();
        assert_eq!(v["result"]["protocolVersion"], "2024-11-05", "got: {v}");
    }

    /// #86: `tools/list` walks the catalogue in `cursor`-delimited pages and
    /// stops emitting `nextCursor` once the listing is exhausted. Driven over
    /// the real descriptors with a tiny page size so the boundaries are
    /// exercised even though production uses one big page.
    #[test]
    fn tools_list_pagination_walks_every_tool_in_order() {
        let (server, _dir) = test_server();
        let all = server.tool_entries();
        let total = all.len();
        assert!(total >= 3, "need several tools to exercise paging: {total}");

        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let (page, next) = paginate_tools(&all, cursor.as_deref(), 2).unwrap();
            assert!(page.len() <= 2, "page must respect the size budget");
            for entry in &page {
                seen.push(entry["name"].as_str().unwrap().to_string());
            }
            pages += 1;
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
            assert!(pages <= total + 1, "pagination must terminate");
        }

        let expected: Vec<String> = all
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            seen, expected,
            "paging must cover every tool exactly once, in order"
        );
        assert!(
            pages > 1,
            "a 2-per-page walk over {total} tools needs >1 page"
        );
    }

    /// #86: a malformed cursor is a parameter error, and the default
    /// (cursor-less) listing returns every tool with no `nextCursor`.
    #[test]
    fn tools_list_rejects_bad_cursor_and_single_page_has_no_next() {
        let (server, _dir) = test_server();

        let bad =
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"cursor":"not-a-number"}}"#;
        let v: serde_json::Value = serde_json::from_str(&server.dispatch(bad).unwrap()).unwrap();
        assert_eq!(v["error"]["code"], INVALID_PARAMS, "got: {v}");

        let ok = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let v: serde_json::Value = serde_json::from_str(&server.dispatch(ok).unwrap()).unwrap();
        assert!(
            v["result"]["nextCursor"].is_null(),
            "the whole catalogue fits one page, so nextCursor must be absent: {v}"
        );
        assert!(v["result"]["tools"].as_array().unwrap().len() >= 3);
    }

    /// #210: the configured repo root is replaced with a placeholder so a
    /// client never learns the server's absolute layout.
    #[test]
    fn redact_paths_replaces_repo_root_with_placeholder() {
        let root = Path::new("/Users/qjs/Code/Projects/specslice");
        let msg = "opening SQLite database at /Users/qjs/Code/Projects/specslice/.specslice/graph.db: unable to open";
        let out = redact_paths(root, msg);
        assert_eq!(
            out,
            "opening SQLite database at <repo-root>/.specslice/graph.db: unable to open"
        );
        assert!(!out.contains("/Users/qjs"), "username must not leak: {out}");
    }

    /// #210: absolute paths outside the repo root (cargo, rustup, temp) still
    /// have their `/Users/<name>` or `/home/<name>` prefix masked.
    #[test]
    fn redact_paths_masks_home_prefixes_outside_the_repo_root() {
        let root = Path::new("/Users/qjs/Code/Projects/specslice");

        let mac = redact_paths(root, "failed reading /Users/qjs/.cargo/registry/foo.rs");
        assert_eq!(mac, "failed reading <home>/.cargo/registry/foo.rs");

        // CI: repo root differs from the leaked path entirely.
        let ci = redact_paths(
            Path::new("/home/runner/work/specslice/specslice"),
            "spawn gopls failed: /home/runner/go/bin/gopls: no such file",
        );
        assert_eq!(ci, "spawn gopls failed: <home>/go/bin/gopls: no such file");
        assert!(
            !ci.contains("/home/runner"),
            "host layout must not leak: {ci}"
        );
    }

    /// #210: a bare home path at end-of-string is still masked, and a
    /// path-free message is returned untouched.
    #[test]
    fn redact_paths_handles_trailing_home_path_and_leaves_plain_text_alone() {
        let root = Path::new("/tmp/does-not-appear");
        assert_eq!(
            redact_paths(root, "permission denied at /Users/alice"),
            "permission denied at <home>"
        );
        assert_eq!(
            redact_paths(root, "node not found in graph"),
            "node not found in graph",
            "a message with no path must be returned verbatim"
        );
    }

    /// #107: the pump surfaces the refusal as an INVALID_REQUEST line and then
    /// stops, instead of OOM-ing on the unbounded stream.
    #[test]
    fn pump_rejects_overlong_line_with_protocol_error() {
        let (server, _dir) = test_server();
        // No newline anywhere: a single unbounded "line". 32 MiB > 16 MiB cap.
        let hostile = vec![b'a'; (MAX_LINE_BYTES) + 16];
        let mut reader = std::io::Cursor::new(hostile);
        let mut out: Vec<u8> = Vec::new();
        server
            .pump(&mut reader, &mut out)
            .expect("pump must not error");
        let text = String::from_utf8_lossy(&out);
        let v: serde_json::Value =
            serde_json::from_str(text.trim()).expect("one error line emitted");
        assert_eq!(v["error"]["code"], INVALID_REQUEST, "got: {text}");
    }
}
