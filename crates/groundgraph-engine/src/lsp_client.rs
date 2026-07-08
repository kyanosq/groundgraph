//! Minimal synchronous LSP client used by the Swift / Go indexers (P11).
//!
//! The Dart adapter speaks a custom newline-delimited JSON protocol to a
//! Dart sidecar we wrote ourselves. For the new languages (Swift, Go,
//! and eventually Python / TypeScript / Java) we instead drive the
//! upstream language servers — `sourcekit-lsp` and `gopls` today — over
//! the standard Language Server Protocol. This module is deliberately a
//! small, blocking client: GroundGraph indexes once per `groundgraph index`
//! invocation and we never need concurrent requests, which dramatically
//! simplifies framing and lifecycle management.
//!
//! What the client supports today:
//! - Spawning a server process with `Stdio::piped()` on stdin / stdout
//!   and a capped, drained `Stdio::piped()` for stderr (so the server's
//!   log noise is captured for diagnostics instead of leaking onto
//!   GroundGraph's own stderr — see `captured_stderr`).
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
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
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
/// enumerate the kinds we actually map to a GroundGraph [`crate::NodeKind`]
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
    /// 0-based line where the identifier sits — `selectionRange.start.line`
    /// per the LSP spec, used as the cursor position for
    /// `prepareCallHierarchy` / `references` requests.
    pub selection_line: u32,
    /// 0-based character offset inside [`Self::selection_line`].
    pub selection_character: u32,
    pub children: Vec<LspDocumentSymbol>,
}

/// Minimum-viable `CallHierarchyItem`: the fields we actually
/// dereference when resolving outgoing calls back to known GroundGraph
/// symbols. Both `range` and `selectionRange` are 0-based per the LSP
/// spec; we only keep the identifier line for the resolver.
///
/// The `raw` field holds the unmodified JSON the server sent us. The
/// LSP spec requires this to be passed back verbatim on
/// `callHierarchy/outgoingCalls` (and `incomingCalls`); some servers
/// — sourcekit-lsp in particular — attach a `data: { usr }` field
/// that they use as the lookup key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCallHierarchyItem {
    pub name: String,
    pub kind: LspSymbolKind,
    pub uri: String,
    pub selection_line: u32,
    pub selection_character: u32,
    pub raw: Value,
}

/// `Location` as returned by `textDocument/references`. We collapse
/// the range to its 0-based start line + character — every consumer
/// inside GroundGraph only needs the position, not the span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspLocation {
    pub uri: String,
    pub line: u32,
    pub character: u32,
}

/// One entry from `callHierarchy/outgoingCalls`. Bundles the callee
/// (`to`) with the *caller-side* call sites the server reported in
/// `fromRanges` so GroundGraph can record edge evidence at the actual
/// call location instead of the callee's declaration line.
///
/// `from_ranges` carries `(line, character)` 0-based pairs in the
/// caller's file (the URI we issued `prepareCallHierarchy` against).
/// An empty vector means the server elided the field — common for
/// older sourcekit-lsp builds; the indexer falls back to the
/// caller's identifier line in that case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspOutgoingCall {
    pub to: LspCallHierarchyItem,
    pub from_ranges: Vec<(u32, u32)>,
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
    /// Tail of the server's stderr, captured by a drainer thread (#214).
    /// We pipe + drain instead of inheriting so sourcekit-lsp / gopls log
    /// noise never leaks into GroundGraph's own stderr (which an MCP client
    /// captures as diagnostics). Capped at [`MAX_STDERR_TAIL`] bytes.
    stderr_tail: Arc<Mutex<Vec<u8>>>,
}

/// Keep at most the last 64 KiB of server stderr — enough to surface a
/// crash backtrace in `skip_reason`, bounded so a chatty server cannot
/// grow the buffer without limit.
const MAX_STDERR_TAIL: usize = 64 * 1024;

impl LspClient {
    /// Spawn an LSP server. `command` is the executable, `args` are the
    /// CLI arguments, and `cwd` becomes the server's working directory
    /// (which both `sourcekit-lsp` and `gopls` use as the implicit root
    /// when no explicit `workspaceFolders` are supplied).
    pub fn spawn(command: &str, args: &[&str], cwd: &Path) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // #214: capture stderr rather than inheriting it. An inherited
            // stream lets the server scribble onto GroundGraph's stderr, which
            // an MCP client reads as our diagnostics.
            .stderr(Stdio::piped());
        // #68: own process group so a teardown also reaps the analysis-server /
        // build-tool grandchildren that sourcekit-lsp / gopls fork.
        crate::proc::detach_process_group(&mut cmd);
        let mut child = cmd
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
        let stderr = child
            .stderr
            .take()
            .context("LSP server did not expose stderr")?;

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

        // Drainer thread: continuously read the server's stderr into a capped
        // buffer so the pipe never fills (which would block the server) and so
        // the tail is available for diagnostics on failure. Detached: it exits
        // on stderr EOF when the child dies.
        let stderr_tail = Arc::new(Mutex::new(Vec::<u8>::new()));
        {
            let sink = Arc::clone(&stderr_tail);
            std::thread::Builder::new()
                .name(format!("lsp-stderr[{server_name}]"))
                .spawn(move || {
                    let mut reader = BufReader::new(stderr);
                    let mut chunk = [0u8; 4096];
                    loop {
                        match reader.read(&mut chunk) {
                            Ok(0) => break, // EOF: server closed stderr / exited
                            Ok(n) => {
                                if let Ok(mut buf) = sink.lock() {
                                    buf.extend_from_slice(&chunk[..n]);
                                    // Retain only the last MAX_STDERR_TAIL bytes.
                                    let len = buf.len();
                                    if len > MAX_STDERR_TAIL {
                                        buf.drain(0..len - MAX_STDERR_TAIL);
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                })
                .context("spawning LSP stderr drainer thread")?;
        }

        Ok(Self {
            child: Some(child),
            stdin,
            rx: Some(rx),
            next_id: AtomicI64::new(1),
            server_name,
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            stderr_tail,
        })
    }

    /// The captured tail (≤ 64 KiB) of the server's stderr. Used by indexers
    /// to fold a failing server's diagnostics into their `skip_reason` instead
    /// of letting the noise escape onto GroundGraph's own stderr (#214).
    pub fn captured_stderr(&self) -> String {
        match self.stderr_tail.lock() {
            Ok(buf) => String::from_utf8_lossy(&buf).into_owned(),
            Err(_) => String::new(),
        }
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
            "clientInfo": { "name": "groundgraph", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": {
                "textDocument": {
                    "synchronization": { "didOpen": true, "didClose": true, "didSave": false },
                    "documentSymbol": {
                        "hierarchicalDocumentSymbolSupport": true,
                        "symbolKind": {
                            "valueSet": (1..=26).collect::<Vec<i64>>(),
                        },
                    },
                    // P13 — opt-in to call hierarchy + references so
                    // sourcekit-lsp / gopls advertise the corresponding
                    // providers in their reply and accept our follow-up
                    // requests.
                    "callHierarchy": { "dynamicRegistration": false },
                    "references": { "dynamicRegistration": false },
                },
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                },
            },
            "workspaceFolders": [
                { "uri": root_uri, "name": "groundgraph-root" }
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

    /// P13 — `textDocument/prepareCallHierarchy`. Returns the set of
    /// call-hierarchy items the server identified at `(line, character)`
    /// (0-based per the LSP spec). An empty vector means the server
    /// could not anchor the position (typical for whitespace / unknown
    /// identifiers).
    pub fn prepare_call_hierarchy(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspCallHierarchyItem>> {
        let raw = self.request(
            "textDocument/prepareCallHierarchy",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )?;
        Ok(parse_call_hierarchy_items(&raw))
    }

    /// P13 / P15 — `callHierarchy/outgoingCalls`. Given a previously
    /// prepared call-hierarchy item, return the callees the server
    /// reports together with the caller-side `fromRanges` it attached
    /// to each. The indexer uses `fromRanges[0]` as the audit-trail
    /// evidence location for the `Calls` edge so the row in
    /// `references` points at where the call actually occurs, not at
    /// the callee's declaration.
    ///
    /// We echo the server's original `CallHierarchyItem` JSON
    /// verbatim — the LSP spec requires the opaque `data` field to be
    /// round-tripped unchanged, and sourcekit-lsp in particular puts a
    /// `{ usr }` in there which is the only thing the indexer keys on.
    pub fn outgoing_calls(&mut self, item: &LspCallHierarchyItem) -> Result<Vec<LspOutgoingCall>> {
        let raw = self.request("callHierarchy/outgoingCalls", json!({ "item": item.raw }))?;
        Ok(parse_outgoing_calls(&raw))
    }

    /// P13 — `textDocument/references`. Returns each `Location` where
    /// the symbol at `(line, character)` is referenced. We deliberately
    /// pass `includeDeclaration: false` because the declaration is
    /// already a GroundGraph symbol; what we need are the *call sites*.
    pub fn references(&mut self, uri: &str, line: u32, character: u32) -> Result<Vec<LspLocation>> {
        let raw = self.request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": { "includeDeclaration": false },
            }),
        )?;
        Ok(parse_locations(&raw))
    }

    /// Politely terminate the server using the LSP `shutdown` + `exit`
    /// dance. Failures are surfaced via the returned `Result` but the
    /// child is always waited on via [`Drop`] to avoid zombies.
    pub fn shutdown(&mut self) -> Result<()> {
        if self.child.is_none() {
            // Already force-killed (e.g. after a response timeout) —
            // writing shutdown/exit into a dead pipe is pointless.
            return Ok(());
        }
        let shutdown_result = self.request("shutdown", json!(null));
        let exit_notify = self.notify("exit", json!(null));
        // #77: bound the wait. A server that ignores `exit` (or whose `kill`
        // we couldn't deliver) must not hang the indexer — reap within a
        // budget, then take the whole process group down as a fallback.
        if let Some(mut child) = self.child.take() {
            if !crate::proc::reap_within(&mut child, Duration::from_secs(2)) {
                crate::proc::kill_and_reap(&mut child, Duration::from_secs(2));
            }
        }
        self.rx.take();
        shutdown_result.context("LSP shutdown request failed")?;
        exit_notify.context("LSP exit notification failed")?;
        Ok(())
    }

    /// Fire-and-forget LSP `shutdown` + `exit` so a dropped client gives the
    /// server a chance to release its lock files before we SIGKILL it (#69).
    /// Never waits for the response — [`Drop`] must not block on I/O.
    fn try_graceful_exit(&mut self) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let shutdown = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "shutdown",
            "params": null,
        });
        let _ = write_message(&mut self.stdin, &shutdown);
        let _ = self.notify("exit", json!(null));
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
            // #68/#77: kill the whole process group and bound the reap so a
            // failed signal cannot wedge us.
            crate::proc::kill_and_reap(&mut child, Duration::from_secs(2));
        }
        // Drop the receiver so the reader thread observes a disconnect
        // and exits. The thread itself is detached.
        self.rx.take();
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // #69: best-effort graceful shutdown first so sourcekit-lsp / gopls
            // release their sourcekitd + index-store locks (a hard SIGKILL
            // leaves them stale and stalls the next index by 8-12s). Tightly
            // bounded so Drop can never hang (#77).
            self.try_graceful_exit();
            if !crate::proc::reap_within(&mut child, Duration::from_millis(300)) {
                crate::proc::kill_and_reap(&mut child, Duration::from_secs(2));
            }
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
    // Prefer the identifier-only `selectionRange` for call-hierarchy /
    // references positions. Fall back to the wider `range` when the
    // server only emitted one (older SymbolInformation responses go
    // through a different normaliser).
    let selection_range = obj.get("selectionRange").or(Some(range))?;
    let (selection_line, selection_character) =
        extract_position(selection_range).unwrap_or((start_line, 0));
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
        selection_line,
        selection_character,
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
                selection_line: flat.start_line,
                selection_character: 0,
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
            selection_line: flat.start_line,
            selection_character: 0,
            start_line: flat.start_line,
            end_line: flat.end_line,
            children,
        });
    }
    tree
}

/// Parse a `CallHierarchyItem[]` JSON payload into our normalised
/// shape. The protocol allows `null` as an empty result; we treat
/// anything that is not an array as "no items".
fn parse_call_hierarchy_items(raw: &Value) -> Vec<LspCallHierarchyItem> {
    let Some(items) = raw.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if let Some(parsed) = parse_call_hierarchy_item(item) {
            out.push(parsed);
        }
    }
    out
}

/// Parse a `CallHierarchyOutgoingCall[]` JSON payload — one entry
/// per callee, each carrying `to` (the callee item) and `fromRanges`
/// (call sites inside the caller). Defensive: entries missing `to`
/// or whose `to` cannot be parsed are dropped silently so a single
/// malformed item does not nuke the rest of the response.
fn parse_outgoing_calls(raw: &Value) -> Vec<LspOutgoingCall> {
    let Some(items) = raw.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for entry in items {
        let Some(to_value) = entry.get("to") else {
            continue;
        };
        let Some(to) = parse_call_hierarchy_item(to_value) else {
            continue;
        };
        let from_ranges = entry
            .get("fromRanges")
            .and_then(Value::as_array)
            .map(|ranges| {
                ranges
                    .iter()
                    .filter_map(extract_position)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.push(LspOutgoingCall { to, from_ranges });
    }
    out
}

fn parse_call_hierarchy_item(item: &Value) -> Option<LspCallHierarchyItem> {
    let obj = item.as_object()?;
    let name = obj.get("name")?.as_str()?.to_string();
    let uri = obj.get("uri")?.as_str()?.to_string();
    let kind_raw = u32::try_from(obj.get("kind")?.as_u64()?).ok()?;
    let position_source = obj.get("selectionRange").or_else(|| obj.get("range"))?;
    let (line, character) = extract_position(position_source)?;
    Some(LspCallHierarchyItem {
        name,
        uri,
        kind: LspSymbolKind::from_raw(kind_raw),
        selection_line: line,
        selection_character: character,
        raw: item.clone(),
    })
}

/// Parse a `Location[]` JSON payload into our normalised shape.
/// `Location | LocationLink[]` would be a more complete typing but
/// `textDocument/references` only ever returns the simple `Location`
/// variant.
fn parse_locations(raw: &Value) -> Vec<LspLocation> {
    let Some(items) = raw.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(uri) = obj.get("uri").and_then(Value::as_str) else {
            continue;
        };
        let Some(range) = obj.get("range") else {
            continue;
        };
        let Some((line, character)) = extract_position(range) else {
            continue;
        };
        out.push(LspLocation {
            uri: uri.to_string(),
            line,
            character,
        });
    }
    out
}

fn extract_range_lines(range: &Value) -> Option<(u32, u32)> {
    let start = u32::try_from(range.get("start")?.get("line")?.as_u64()?).ok()?;
    let end = u32::try_from(range.get("end")?.get("line")?.as_u64()?).ok()?;
    let end = end.max(start);
    Some((start, end))
}

/// Read `{ "start": { "line": L, "character": C } }` from an LSP range.
/// Returns 0-based `(line, character)` so callers can hand the position
/// straight to `prepareCallHierarchy` / `textDocument/references`.
fn extract_position(range: &Value) -> Option<(u32, u32)> {
    let start = range.get("start")?;
    let line = u32::try_from(start.get("line")?.as_u64()?).ok()?;
    let character = u32::try_from(start.get("character")?.as_u64()?).ok()?;
    Some((line, character))
}

fn write_message<W: Write>(writer: &mut W, body: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(body)?;
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

fn read_message<R: BufRead>(reader: &mut R) -> Result<Value> {
    // A single header line is tiny in practice (`Content-Length: <digits>`,
    // `Content-Type: …`). Cap each line so a hostile server streaming an
    // endless, newline-less header cannot grow `header` to OOM — the body
    // already has its own cap (#32), this closes the header side (#249).
    const MAX_HEADER_LINE_BYTES: u64 = 8 * 1024;
    let mut content_length: Option<usize> = None;
    let mut header = String::new();
    loop {
        header.clear();
        let read = reader
            .by_ref()
            .take(MAX_HEADER_LINE_BYTES)
            .read_line(&mut header)
            .context("reading LSP header line")?;
        if read == 0 {
            bail!("LSP server closed stdout before sending a complete message");
        }
        // Hit the per-line cap without a terminator → unbounded header.
        if read as u64 == MAX_HEADER_LINE_BYTES && !header.ends_with('\n') {
            bail!("LSP header line exceeds the {MAX_HEADER_LINE_BYTES}-byte cap");
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
    // A hostile or broken server must not drive a multi-GB allocation from
    // one header line (issues2.md #32). Real responses (large
    // documentSymbol trees) stay well under this.
    const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
    if length > MAX_FRAME_BYTES {
        bail!("LSP frame of {length} bytes exceeds the {MAX_FRAME_BYTES}-byte cap");
    }
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

/// Reverse of [`path_to_file_uri`]. Accepts both the canonical
/// `file:///abs/path` form and lenient forms occasionally produced by
/// language servers (`file:/abs/path` or even bare paths). Returns
/// `None` when the URI scheme is not `file` so the caller can ignore
/// remote / virtual documents.
pub fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("file:"))?;
    // Some servers emit `file:///abs/path`, others `file://localhost/abs/path`;
    // strip the optional authority component if present.
    let after_authority = if let Some(idx) = rest.find('/') {
        if rest[..idx].is_empty() {
            // `file:///abs/...` (no authority) → leave the leading slash.
            rest
        } else {
            // `file://host/abs/...` (we ignore the host).
            &rest[idx..]
        }
    } else {
        rest
    };
    let decoded = percent_decode(after_authority);
    Some(std::path::PathBuf::from(decoded))
}

/// Minimal percent-decoder shared by [`file_uri_to_path`]. Only handles
/// `%XX` triplets; anything malformed is passed through verbatim so we
/// never silently drop characters the operator can see.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                // `h` and `l` are 4-bit hex digits (max 0x0F), so the
                // composite fits in a single byte. `try_from` keeps
                // clippy's strict cast lints happy without runtime cost.
                if let Ok(byte) = u8::try_from((h << 4) | l) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|err| {
        // Fallback: lossy decode rather than losing the whole URI.
        let bytes = err.into_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    })
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

    /// issues.md #214: an LSP server's stderr (sourcekit-lsp / gopls push
    /// `window/logMessage`, crash backtraces, JVM warnings) must be *captured*
    /// — piped + drained into a capped buffer — never `Stdio::inherit()`ed into
    /// GroundGraph's own stderr, or an MCP client treats GroundGraph as "randomly
    /// vomiting log noise". We spawn a stand-in server that writes a marker to
    /// stderr, then confirm the client drained it into its own buffer.
    #[cfg(unix)]
    #[test]
    fn spawn_captures_server_stderr_instead_of_inheriting_it() {
        let dir = std::env::temp_dir();
        // The stand-in writes one stderr line, then blocks on stdin (`cat`) so
        // the child stays alive long enough for the drainer to read it.
        let client = LspClient::spawn(
            "sh",
            &["-c", "echo groundgraph-stderr-probe 1>&2; cat"],
            &dir,
        )
        .expect("spawn stand-in server");

        // Poll the captured tail (drainer runs on its own thread).
        let mut captured = String::new();
        for _ in 0..40 {
            captured = client.captured_stderr();
            if captured.contains("groundgraph-stderr-probe") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            captured.contains("groundgraph-stderr-probe"),
            "child stderr must be captured, not inherited; got: {captured:?}"
        );
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

    /// issues2.md #32: a hostile/broken server header like
    /// `Content-Length: 99999999999` must be rejected up front, not turned
    /// into a giant buffer allocation.
    #[test]
    fn read_message_rejects_absurd_content_length_without_allocating() {
        let data = b"Content-Length: 99999999999\r\n\r\n";
        let mut reader = std::io::BufReader::new(&data[..]);
        let err = read_message(&mut reader).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("exceeds") || msg.contains("too large"),
            "expected a size-cap error, got: {msg}"
        );
    }

    /// #249: the body cap (#32) protected `Content-Length`, but a single
    /// header *line* with no terminator was still read unbounded. A hostile
    /// server streaming an endless header must be refused, not buffered to OOM.
    #[test]
    fn read_message_rejects_unbounded_header_line() {
        let mut data = Vec::new();
        data.extend_from_slice(b"Content-Length: ");
        data.extend(std::iter::repeat_n(b'0', 64 * 1024)); // 64 KiB, no '\n'
        let mut reader = std::io::BufReader::new(data.as_slice());
        let err = read_message(&mut reader).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("header line exceeds"),
            "expected a header-line cap error, got: {msg}"
        );
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
        let p = Path::new("/tmp/groundgraph/中文 路径/file.swift");
        let uri = path_to_file_uri(p);
        assert!(uri.starts_with("file:///tmp/groundgraph/"), "uri: {uri}");
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

    /// After a timeout the child has already been force-killed and
    /// reaped — `shutdown()` must be a no-op success, not a doomed
    /// shutdown/exit write into a closed pipe (issues.md #25).
    #[test]
    #[cfg(unix)]
    fn shutdown_after_forced_kill_is_a_clean_noop() {
        let mut client = LspClient::spawn("sleep", &["30"], Path::new("/"))
            .expect("sleep should be spawnable on Unix");
        client.set_response_timeout(Duration::from_millis(100));
        let _ = client.initialize("file:///tmp").unwrap_err();
        client
            .shutdown()
            .expect("shutdown after force_kill must succeed as a no-op");
    }

    /// P15 — `callHierarchy/outgoingCalls` returns
    /// `{ to: CallHierarchyItem, fromRanges: Range[] }`. GroundGraph
    /// needs both: `to` identifies the callee while `fromRanges`
    /// pinpoints the call sites inside the *caller* file — that is
    /// the audit-trail location we want to record as edge evidence.
    /// Earlier revisions only kept `to`, which made `Calls` edges
    /// look like they originated from the callee's declaration.
    #[test]
    fn parse_outgoing_calls_returns_from_ranges_alongside_callee() {
        let payload = json!([
            {
                "to": {
                    "name": "greet",
                    "kind": 6,
                    "uri": "file:///tmp/Greeter.swift",
                    "range": { "start": { "line": 20, "character": 0 }, "end": { "line": 23, "character": 1 } },
                    "selectionRange": { "start": { "line": 21, "character": 16 }, "end": { "line": 21, "character": 21 } }
                },
                "fromRanges": [
                    { "start": { "line": 7, "character": 4 }, "end": { "line": 7, "character": 9 } },
                    { "start": { "line": 11, "character": 12 }, "end": { "line": 11, "character": 17 } }
                ]
            },
            {
                "to": {
                    "name": "goodbye",
                    "kind": 6,
                    "uri": "file:///tmp/Greeter.swift",
                    "range": { "start": { "line": 30, "character": 0 }, "end": { "line": 34, "character": 1 } },
                    "selectionRange": { "start": { "line": 31, "character": 16 }, "end": { "line": 31, "character": 23 } }
                }
                // no fromRanges — accept and surface empty list
            },
            { "garbage": true } // missing `to` — must be skipped
        ]);
        let parsed = parse_outgoing_calls(&payload);
        assert_eq!(parsed.len(), 2, "expected two valid outgoing calls");
        assert_eq!(parsed[0].to.name, "greet");
        assert_eq!(parsed[0].from_ranges, vec![(7, 4), (11, 12)]);
        assert_eq!(parsed[1].to.name, "goodbye");
        assert!(
            parsed[1].from_ranges.is_empty(),
            "expected empty fromRanges when server omitted them"
        );
    }

    #[test]
    fn parse_call_hierarchy_items_normalises_kind_selection_and_preserves_data() {
        // The `data` field is opaque (sourcekit-lsp puts a USR there).
        // Our parser must keep it intact under `raw` so the indexer
        // can echo the whole `CallHierarchyItem` back on
        // `callHierarchy/outgoingCalls`.
        let payload = json!([
            {
                "name": "greet",
                "kind": 6,
                "uri": "file:///tmp/Greeter.swift",
                "range": { "start": { "line": 20, "character": 0 }, "end": { "line": 23, "character": 1 } },
                "selectionRange": { "start": { "line": 21, "character": 16 }, "end": { "line": 21, "character": 21 } },
                "data": { "usr": "s:7GreeterAAC5greetSSyF" }
            },
            { "broken": true }
        ]);
        let items = parse_call_hierarchy_items(&payload);
        assert_eq!(items.len(), 1);
        let it = &items[0];
        assert_eq!(it.name, "greet");
        assert_eq!(it.kind, LspSymbolKind::Method);
        assert_eq!(it.uri, "file:///tmp/Greeter.swift");
        assert_eq!(it.selection_line, 21);
        assert_eq!(it.selection_character, 16);
        assert_eq!(
            it.raw
                .get("data")
                .and_then(|d| d.get("usr"))
                .and_then(|u| u.as_str()),
            Some("s:7GreeterAAC5greetSSyF")
        );
    }

    #[test]
    fn parse_locations_collects_line_character_for_references() {
        let payload = json!([
            {
                "uri": "file:///tmp/Caller.swift",
                "range": { "start": { "line": 7, "character": 4 }, "end": { "line": 7, "character": 9 } },
            },
            { "uri": "file:///tmp/B.swift" /* missing range */ }
        ]);
        let locs = parse_locations(&payload);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].line, 7);
        assert_eq!(locs[0].character, 4);
        assert_eq!(locs[0].uri, "file:///tmp/Caller.swift");
    }

    #[test]
    fn file_uri_to_path_round_trips_through_path_to_file_uri() {
        let original = Path::new("/tmp/groundgraph/中文 路径/file.swift");
        let uri = path_to_file_uri(original);
        let recovered = file_uri_to_path(&uri).expect("recoverable");
        assert_eq!(recovered, original);
    }

    #[test]
    fn file_uri_to_path_handles_lenient_forms() {
        assert_eq!(
            file_uri_to_path("file:/tmp/x.swift"),
            Some(std::path::PathBuf::from("/tmp/x.swift"))
        );
        assert_eq!(
            file_uri_to_path("file://localhost/tmp/x.swift"),
            Some(std::path::PathBuf::from("/tmp/x.swift"))
        );
        assert_eq!(file_uri_to_path("https://example.com"), None);
    }

    #[test]
    fn flatten_pairs_walks_pre_order() {
        let tree = vec![LspDocumentSymbol {
            name: "A".into(),
            detail: None,
            kind: LspSymbolKind::Class,
            start_line: 0,
            end_line: 100,
            selection_line: 0,
            selection_character: 6,
            children: vec![LspDocumentSymbol {
                name: "A.m".into(),
                detail: None,
                kind: LspSymbolKind::Method,
                start_line: 1,
                end_line: 5,
                selection_line: 1,
                selection_character: 2,
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
