//! Minimal synchronous LSP client used by the Swift / Go indexers (P11).
//!
//! The Dart adapter speaks a custom newline-delimited JSON protocol to a
//! Dart sidecar we wrote ourselves. For the new languages (Swift, Go,
//! and eventually Python / TypeScript / Java) we instead drive the
//! upstream language servers — `sourcekit-lsp` and `gopls` today — over
//! the standard Language Server Protocol. This module is deliberately a
//! small, blocking client: SpecSlice indexes once per `specslice index`
//! invocation and we never need concurrent requests, which dramatically
//! simplifies framing and lifecycle management.
//!
//! What the client supports today:
//! - Spawning a server process with `Stdio::piped()` on stdin / stdout
//!   and `Stdio::inherit()` for stderr (so users see warnings).
//! - LSP-conforming `Content-Length:` message framing.
//! - Synchronous `request` (correlated by integer id) and `notify`
//!   (fire-and-forget) primitives. Server-initiated requests and other
//!   notifications are drained silently while we wait for our response —
//!   `sourcekit-lsp` and `gopls` both push `window/logMessage`,
//!   `$/progress`, `workspace/configuration`, etc.
//! - The `initialize` / `initialized` / `shutdown` / `exit` handshake.
//!
//! What it intentionally does *not* support:
//! - Async / multiplexed requests. The indexer is fundamentally
//!   sequential — open file, ask for symbols, close file, repeat.
//! - Streaming JSON-RPC batches.
//! - Subscribing to diagnostics. We do not consume them.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// Hard cap on how long we wait for a single response message. Picked to
/// be larger than even a cold-start `sourcekit-lsp initialize` on a
/// laptop (~6s observed) without hanging CI indefinitely if the server
/// becomes silent.
const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// LSP `SymbolKind` enum values per the spec (1..=26). We only
/// enumerate the kinds we actually map to a SpecSlice [`crate::NodeKind`]
/// in any language profile — everything else gets discarded by the
/// indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub enum LspSymbolKind {
    File,
    Module,
    Namespace,
    Package,
    Class,
    Method,
    Property,
    Field,
    Constructor,
    Enum,
    Interface,
    Function,
    Variable,
    Constant,
    EnumMember,
    Struct,
    Other(u32),
}

impl LspSymbolKind {
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            1 => LspSymbolKind::File,
            2 => LspSymbolKind::Module,
            3 => LspSymbolKind::Namespace,
            4 => LspSymbolKind::Package,
            5 => LspSymbolKind::Class,
            6 => LspSymbolKind::Method,
            7 => LspSymbolKind::Property,
            8 => LspSymbolKind::Field,
            9 => LspSymbolKind::Constructor,
            10 => LspSymbolKind::Enum,
            11 => LspSymbolKind::Interface,
            12 => LspSymbolKind::Function,
            13 => LspSymbolKind::Variable,
            14 => LspSymbolKind::Constant,
            22 => LspSymbolKind::EnumMember,
            23 => LspSymbolKind::Struct,
            other => LspSymbolKind::Other(other),
        }
    }
}

/// One node in a [`textDocument/documentSymbol`] hierarchy after we have
/// normalised the protocol's old `SymbolInformation[]` shape and the
/// new `DocumentSymbol[]` shape into the same nested form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDocumentSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: LspSymbolKind,
    /// 0-based inclusive start line (LSP positions are 0-indexed; the
    /// engine converts to 1-based when it writes nodes).
    pub start_line: u32,
    /// 0-based inclusive end line.
    pub end_line: u32,
    pub children: Vec<LspDocumentSymbol>,
}

/// Result delivered by the background reader thread for one stdout
/// frame. We send a single terminal `Err` and then close the channel
/// so blocking receivers wake up on EOF / I/O failure.
type ReaderMessage = Result<Value>;

/// Spawned LSP server we can talk to synchronously. Stdout is drained
/// by a background thread so that [`Self::set_response_timeout`] can
/// fire even when the server holds stdin open without writing — the
/// blocking [`BufRead::read_line`] used by [`read_message`] would
/// otherwise sit on a futex forever and trump the deadline check.
pub struct LspClient {
    child: Option<Child>,
    stdin: ChildStdin,
    /// `None` once we drop the receiver during shutdown so the reader
    /// thread observes the disconnect and exits cleanly.
    rx: Option<Receiver<ReaderMessage>>,
    next_id: AtomicI64,
    server_name: String,
    response_timeout: Duration,
}

impl LspClient {
    /// Spawn an LSP server. `command` is the executable, `args` are the
    /// CLI arguments, and `cwd` becomes the server's working directory
    /// (which both `sourcekit-lsp` and `gopls` use as the implicit root
    /// when no explicit `workspaceFolders` are supplied).
    pub fn spawn(command: &str, args: &[&str], cwd: &Path) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning LSP server `{command}`"))?;
        let stdin = child
            .stdin
            .take()
            .context("LSP server did not expose stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("LSP server did not expose stdout")?;

        let (tx, rx) = mpsc::channel::<ReaderMessage>();
        let server_name = command.to_string();
        // Reader thread: drain stdout into the channel one frame at a
        // time. Returns silently when the receiver is dropped (we lose
        // interest) or when stdout EOFs / errors (server died).
        let thread_name = format!("lsp-reader[{server_name}]");
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match read_message(&mut reader) {
                        Ok(value) => {
                            if tx.send(Ok(value)).is_err() {
                                break; // receiver dropped
                            }
                        }
                        Err(err) => {
                            // Best-effort delivery of the terminal error;
                            // ignore send failure since the receiver may
                            // already be gone.
                            let _ = tx.send(Err(err));
                            break;
                        }
                    }
                }
            })
            .context("spawning LSP stdout reader thread")?;

        Ok(Self {
            child: Some(child),
            stdin,
            rx: Some(rx),
            next_id: AtomicI64::new(1),
            server_name,
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
        })
    }

    /// Override the per-request timeout (mainly so tests can shrink it).
    pub fn set_response_timeout(&mut self, timeout: Duration) {
        self.response_timeout = timeout;
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Issue an LSP `initialize` request with sensible defaults for an
    /// indexing client and follow it with the `initialized` notification
    /// the spec mandates. `root_uri` should be a `file://...` URI.
    pub fn initialize(&mut self, root_uri: &str) -> Result<Value> {
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "clientInfo": { "name": "specslice", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": {
                "textDocument": {
                    "synchronization": { "didOpen": true, "didClose": true, "didSave": false },
                    "documentSymbol": {
                        "hierarchicalDocumentSymbolSupport": true,
                        "symbolKind": {
                            "valueSet": (1..=26).collect::<Vec<i64>>(),
                        },
                    },
                },
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                },
            },
            "workspaceFolders": [
                { "uri": root_uri, "name": "specslice-root" }
            ],
            "trace": "off",
        });
        let result = self.request("initialize", params)?;
        self.notify("initialized", json!({}))?;
        Ok(result)
    }

    /// Send a `textDocument/didOpen` notification with the file's text
    /// so the server can answer subsequent requests without re-reading
    /// disk. `language_id` is the LSP language identifier (e.g.
    /// `"swift"`, `"go"`).
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) -> Result<()> {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    pub fn did_close(&mut self, uri: &str) -> Result<()> {
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    /// Run `textDocument/documentSymbol` and normalise the response.
    /// Returns an empty list when the server has nothing to say.
    pub fn document_symbol(&mut self, uri: &str) -> Result<Vec<LspDocumentSymbol>> {
        let raw = self.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )?;
        let items = match raw {
            Value::Null => return Ok(Vec::new()),
            Value::Array(items) => items,
            other => bail!("documentSymbol returned non-array payload: {other}"),
        };
        if items.is_empty() {
            return Ok(Vec::new());
        }
        // The protocol allows two shapes: the older flat
        // `SymbolInformation[]` (every entry has `location`) and the
        // newer hierarchical `DocumentSymbol[]` (entries have `range`
        // and `children`). Decide once based on the first item.
        if items
            .first()
            .and_then(|v| v.get("location"))
            .and_then(Value::as_object)
            .is_some()
        {
            Ok(normalise_symbol_information(&items))
        } else {
            Ok(normalise_document_symbols(&items))
        }
    }

    /// Politely terminate the server using the LSP `shutdown` + `exit`
    /// dance. Failures are surfaced via the returned `Result` but the
    /// child is always waited on via [`Drop`] to avoid zombies.
    pub fn shutdown(&mut self) -> Result<()> {
        let shutdown_result = self.request("shutdown", json!(null));
        let exit_notify = self.notify("exit", json!(null));
        let wait_result = self
            .child
            .as_mut()
            .map(|c| c.wait())
            .transpose()
            .context("waiting on LSP server process");
        shutdown_result.context("LSP shutdown request failed")?;
        exit_notify.context("LSP exit notification failed")?;
        wait_result?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        write_message(&mut self.stdin, &payload)?;
        self.read_response_for(id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        write_message(&mut self.stdin, &payload)
    }

    fn read_response_for(&mut self, expected_id: i64) -> Result<Value> {
        let deadline = Instant::now() + self.response_timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                // Tear the child down before bubbling the error so a
                // hung server cannot live longer than its first stuck
                // request. The reader thread exits on its own once
                // stdout closes.
                self.force_kill();
                bail!(
                    "timed out waiting {:?} for LSP response id={} from `{}`",
                    self.response_timeout,
                    expected_id,
                    self.server_name
                );
            }
            let remaining = deadline - now;
            let rx = self
                .rx
                .as_ref()
                .ok_or_else(|| anyhow!("LSP stdout receiver already closed"))?;
            let message = match rx.recv_timeout(remaining) {
                Ok(Ok(value)) => value,
                Ok(Err(err)) => {
                    // Terminal error from reader thread — bubble up with
                    // context so callers see which server died.
                    return Err(err).with_context(|| {
                        format!("reading message from LSP server `{}`", self.server_name)
                    });
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.force_kill();
                    bail!(
                        "timed out waiting {:?} for LSP response id={} from `{}`",
                        self.response_timeout,
                        expected_id,
                        self.server_name
                    );
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!(
                        "LSP server `{}` closed stdout before sending a complete response",
                        self.server_name
                    );
                }
            };
            // A response always has both `id` and either `result` or
            // `error`. Anything else (notifications, server-initiated
            // requests) we silently ignore — the next iteration of the
            // loop pulls the next frame.
            if let Some(id_value) = message.get("id") {
                if message.get("method").is_some() {
                    // Server-initiated request — answer with an empty
                    // `result` so well-behaved servers (gopls) do not
                    // deadlock waiting on us.
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id_value,
                        "result": null,
                    });
                    write_message(&mut self.stdin, &response)?;
                    continue;
                }
                if let Some(actual_id) = id_value.as_i64() {
                    if actual_id == expected_id {
                        if let Some(error) = message.get("error") {
                            return Err(anyhow!(
                                "LSP error response for id={}: {}",
                                expected_id,
                                error
                            ));
                        }
                        return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                    }
                }
                // A response for some other id — almost certainly a stale
                // one we no longer care about. Drop it.
                continue;
            }
            // Notification — nothing to do.
        }
    }

    /// Hard-terminate the child process and drop the stdout receiver.
    /// Used when a response times out or when the user explicitly
    /// asks for a forceful shutdown. Idempotent.
    fn force_kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Drop the receiver so the reader thread observes a disconnect
        // and exits. The thread itself is detached.
        self.rx.take();
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.rx.take();
    }
}

fn normalise_document_symbols(items: &[Value]) -> Vec<LspDocumentSymbol> {
    let mut out = Vec::new();
    for item in items {
        let Some(symbol) = parse_document_symbol(item) else {
            continue;
        };
        out.push(symbol);
    }
    out
}

fn parse_document_symbol(item: &Value) -> Option<LspDocumentSymbol> {
    let obj = item.as_object()?;
    let name = obj.get("name")?.as_str()?.to_string();
    let detail = obj
        .get("detail")
        .and_then(Value::as_str)
        .map(str::to_string);
    let kind_raw = u32::try_from(obj.get("kind")?.as_u64()?).ok()?;
    let range = obj.get("range").or_else(|| obj.get("selectionRange"))?;
    let (start_line, end_line) = extract_range_lines(range)?;
    let children = obj
        .get("children")
        .and_then(Value::as_array)
        .map(|c| normalise_document_symbols(c))
        .unwrap_or_default();
    Some(LspDocumentSymbol {
        name,
        detail,
        kind: LspSymbolKind::from_raw(kind_raw),
        start_line,
        end_line,
        children,
    })
}

fn normalise_symbol_information(items: &[Value]) -> Vec<LspDocumentSymbol> {
    // Reconstruct the parent/child hierarchy implied by the
    // `containerName` field. The original `SymbolInformation` shape did
    // not expose explicit nesting, so we group children under the first
    // symbol whose `name` equals their `containerName` *and* whose
    // range encloses theirs.
    #[derive(Clone)]
    struct Flat {
        name: String,
        detail: Option<String>,
        kind: LspSymbolKind,
        start_line: u32,
        end_line: u32,
        container: Option<String>,
    }
    let mut flats: Vec<Flat> = Vec::new();
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(kind_raw) = obj.get("kind").and_then(Value::as_u64) else {
            continue;
        };
        let Some(range) = obj
            .get("location")
            .and_then(|l| l.get("range"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let Some((start_line, end_line)) = extract_range_lines(&Value::Object(range.clone()))
        else {
            continue;
        };
        let Ok(kind_u32) = u32::try_from(kind_raw) else {
            continue;
        };
        flats.push(Flat {
            name: name.to_string(),
            detail: None,
            kind: LspSymbolKind::from_raw(kind_u32),
            start_line,
            end_line,
            container: obj
                .get("containerName")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    // Build a top-down tree by matching each child to the smallest
    // enclosing parent with a matching name.
    fn build(
        flats: &[Flat],
        parent: Option<&Flat>,
        used: &mut HashSet<usize>,
    ) -> Vec<LspDocumentSymbol> {
        let mut out: Vec<LspDocumentSymbol> = Vec::new();
        for (idx, flat) in flats.iter().enumerate() {
            if used.contains(&idx) {
                continue;
            }
            let belongs = match parent {
                Some(p) => flat
                    .container
                    .as_deref()
                    .map(|c| {
                        c == p.name
                            && flat.start_line >= p.start_line
                            && flat.end_line <= p.end_line
                    })
                    .unwrap_or(false),
                None => flat
                    .container
                    .as_deref()
                    .map(|c| c.is_empty())
                    .unwrap_or(true),
            };
            if !belongs {
                continue;
            }
            used.insert(idx);
            let children = build(flats, Some(flat), used);
            out.push(LspDocumentSymbol {
                name: flat.name.clone(),
                detail: flat.detail.clone(),
                kind: flat.kind,
                start_line: flat.start_line,
                end_line: flat.end_line,
                children,
            });
        }
        out
    }
    let mut used: HashSet<usize> = HashSet::new();
    let mut tree = build(&flats, None, &mut used);
    // Anything left over (containerName pointed at something we never
    // saw) becomes a top-level symbol so it still shows up in the graph.
    for (idx, flat) in flats.iter().enumerate() {
        if used.contains(&idx) {
            continue;
        }
        let children = build(&flats, Some(flat), &mut used);
        used.insert(idx);
        tree.push(LspDocumentSymbol {
            name: flat.name.clone(),
            detail: flat.detail.clone(),
            kind: flat.kind,
            start_line: flat.start_line,
            end_line: flat.end_line,
            children,
        });
    }
    tree
}

fn extract_range_lines(range: &Value) -> Option<(u32, u32)> {
    let start = u32::try_from(range.get("start")?.get("line")?.as_u64()?).ok()?;
    let end = u32::try_from(range.get("end")?.get("line")?.as_u64()?).ok()?;
    let end = end.max(start);
    Some((start, end))
}

fn write_message<W: Write>(writer: &mut W, body: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(body)?;
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

fn read_message<R: BufRead>(reader: &mut R) -> Result<Value> {
    let mut content_length: Option<usize> = None;
    let mut header = String::new();
    loop {
        header.clear();
        let read = reader
            .read_line(&mut header)
            .context("reading LSP header line")?;
        if read == 0 {
            bail!("LSP server closed stdout before sending a complete message");
        }
        let trimmed = header.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .with_context(|| format!("parsing Content-Length `{value}`"))?,
            );
        }
        // We ignore Content-Type and any other vendor headers.
    }
    let length =
        content_length.ok_or_else(|| anyhow!("LSP frame missing Content-Length header"))?;
    let mut buf = vec![0u8; length];
    reader
        .read_exact(&mut buf)
        .with_context(|| format!("reading LSP frame body of {length} bytes"))?;
    let body: Value = serde_json::from_slice(&buf)
        .with_context(|| format!("parsing LSP frame body: {}", String::from_utf8_lossy(&buf)))?;
    Ok(body)
}

// ---------------------------------------------------------------------------
// Helpers exposed to other engine modules.
// ---------------------------------------------------------------------------

/// Convert a filesystem path to a `file://` URI accepted by LSP servers.
/// The implementation is deliberately small — both sourcekit-lsp and
/// gopls accept percent-encoded ASCII paths and treat literal spaces
/// fine, so we only escape the characters that would otherwise break the
/// URI grammar.
pub fn path_to_file_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for ch in s.chars() {
        match ch {
            '/' | '-' | '_' | '.' | '~' | ':' => out.push(ch),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                for byte in encoded.bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    out
}

/// Flatten a hierarchical [`LspDocumentSymbol`] tree into parent-child
/// pairs in pre-order. Helper for tests and indexer plumbing.
#[allow(dead_code)]
pub fn flatten_pairs(
    symbols: &[LspDocumentSymbol],
) -> Vec<(Option<&LspDocumentSymbol>, &LspDocumentSymbol)> {
    fn visit<'a>(
        out: &mut Vec<(Option<&'a LspDocumentSymbol>, &'a LspDocumentSymbol)>,
        parent: Option<&'a LspDocumentSymbol>,
        items: &'a [LspDocumentSymbol],
    ) {
        for item in items {
            out.push((parent, item));
            visit(out, Some(item), &item.children);
        }
    }
    let mut out = Vec::new();
    visit(&mut out, None, symbols);
    out
}

/// Thin convenience wrapper so callers can opt in to a strict
/// `Deserialize`-driven model when we extend the client beyond
/// `documentSymbol`.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct InitializeResult {
    #[serde(rename = "serverInfo")]
    pub server_info: Option<ServerInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ServerInfo {
    pub name: String,
    pub version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_messages_with_content_length_framing() {
        let mut buf: Vec<u8> = Vec::new();
        let body = json!({ "jsonrpc": "2.0", "method": "ping" });
        write_message(&mut buf, &body).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let payload = serde_json::to_string(&body).unwrap();
        let expected_header = format!("Content-Length: {}\r\n\r\n", payload.len());
        assert!(
            text.starts_with(&expected_header),
            "expected header `{expected_header:?}`, got: {text:?}"
        );
        assert!(text.ends_with(&payload));
    }

    #[test]
    fn reads_a_message_with_extra_header() {
        let mut data: Vec<u8> = Vec::new();
        let body = r#"{"jsonrpc":"2.0","id":1,"result":42}"#;
        data.extend_from_slice(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n");
        data.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        data.extend_from_slice(body.as_bytes());
        let mut reader = std::io::BufReader::new(data.as_slice());
        let value = read_message(&mut reader).unwrap();
        assert_eq!(value["id"], 1);
        assert_eq!(value["result"], 42);
    }

    #[test]
    fn read_message_fails_when_header_truncated() {
        let data = b"Content-Length: 4\r\n\r\nab";
        let mut reader = std::io::BufReader::new(&data[..]);
        let err = read_message(&mut reader).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("4 bytes"), "expected length error, got: {msg}");
    }

    #[test]
    fn from_raw_covers_known_lsp_symbol_kinds() {
        assert_eq!(LspSymbolKind::from_raw(5), LspSymbolKind::Class);
        assert_eq!(LspSymbolKind::from_raw(6), LspSymbolKind::Method);
        assert_eq!(LspSymbolKind::from_raw(11), LspSymbolKind::Interface);
        assert_eq!(LspSymbolKind::from_raw(12), LspSymbolKind::Function);
        assert_eq!(LspSymbolKind::from_raw(23), LspSymbolKind::Struct);
        assert_eq!(LspSymbolKind::from_raw(99), LspSymbolKind::Other(99));
    }

    #[test]
    fn document_symbols_normalise_hierarchical_shape() {
        let payload = json!([{
            "name": "Greeter",
            "kind": 5,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 10, "character": 1 } },
            "selectionRange": { "start": { "line": 0, "character": 6 }, "end": { "line": 0, "character": 13 } },
            "children": [{
                "name": "greet",
                "kind": 6,
                "range": { "start": { "line": 2, "character": 2 }, "end": { "line": 5, "character": 3 } },
                "selectionRange": { "start": { "line": 2, "character": 6 }, "end": { "line": 2, "character": 11 } },
            }]
        }]);
        let arr = payload.as_array().unwrap();
        let tree = normalise_document_symbols(arr);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].name, "Greeter");
        assert_eq!(tree[0].kind, LspSymbolKind::Class);
        assert_eq!(tree[0].start_line, 0);
        assert_eq!(tree[0].end_line, 10);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].name, "greet");
        assert_eq!(tree[0].children[0].kind, LspSymbolKind::Method);
        assert_eq!(tree[0].children[0].start_line, 2);
        assert_eq!(tree[0].children[0].end_line, 5);
    }

    #[test]
    fn symbol_information_legacy_shape_rebuilds_hierarchy_via_container_name() {
        let payload = json!([
            {
                "name": "Greeter",
                "kind": 5,
                "location": {
                    "uri": "file:///tmp/x.swift",
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 10, "character": 1 } }
                }
            },
            {
                "name": "greet",
                "kind": 6,
                "containerName": "Greeter",
                "location": {
                    "uri": "file:///tmp/x.swift",
                    "range": { "start": { "line": 2, "character": 2 }, "end": { "line": 5, "character": 3 } }
                }
            }
        ]);
        let arr = payload.as_array().unwrap();
        let tree = normalise_symbol_information(arr);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].name, "Greeter");
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].name, "greet");
    }

    #[test]
    fn path_to_file_uri_encodes_non_ascii_and_keeps_slashes() {
        let p = Path::new("/tmp/specslice/中文 路径/file.swift");
        let uri = path_to_file_uri(p);
        assert!(uri.starts_with("file:///tmp/specslice/"), "uri: {uri}");
        assert!(uri.contains("file.swift"));
        assert!(!uri.contains(' '));
        assert!(uri.contains("%E4%B8%AD")); // 中 in UTF-8 → percent encoded
    }

    /// Regression for P12 复核 [P1]: when an LSP server reads our stdin
    /// but never writes a reply, `read_response_for` must honour
    /// `set_response_timeout` and bail (eventually killing the child),
    /// not block forever on the blocking stdout read. The previous
    /// implementation called `BufRead::read_line` inside `read_message`
    /// which would never return.
    #[test]
    #[cfg(unix)]
    fn request_times_out_when_lsp_server_never_writes() {
        // `sleep 30` is a process that holds stdin open without ever
        // writing to stdout — the worst case for our reader loop.
        let mut client = LspClient::spawn("sleep", &["30"], Path::new("/"))
            .expect("sleep should be spawnable on Unix");
        client.set_response_timeout(Duration::from_millis(150));
        let started = Instant::now();
        let err = client.initialize("file:///tmp").unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "LSP request did not honour the 150ms timeout (took {elapsed:?})"
        );
        let msg = format!("{err:?}");
        assert!(
            msg.contains("timed out") || msg.contains("超时"),
            "expected a timeout error, got: {msg}"
        );
    }

    #[test]
    fn flatten_pairs_walks_pre_order() {
        let tree = vec![LspDocumentSymbol {
            name: "A".into(),
            detail: None,
            kind: LspSymbolKind::Class,
            start_line: 0,
            end_line: 100,
            children: vec![LspDocumentSymbol {
                name: "A.m".into(),
                detail: None,
                kind: LspSymbolKind::Method,
                start_line: 1,
                end_line: 5,
                children: Vec::new(),
            }],
        }];
        let pairs = flatten_pairs(&tree);
        let names: Vec<String> = pairs
            .iter()
            .map(|(parent, child)| {
                format!(
                    "{}:{}",
                    parent.map(|p| p.name.as_str()).unwrap_or(""),
                    child.name.as_str()
                )
            })
            .collect();
        assert_eq!(names, vec![":A".to_string(), "A:A.m".to_string()]);
    }
}
