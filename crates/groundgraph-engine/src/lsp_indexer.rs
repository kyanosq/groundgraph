//! Generic LSP-driven language indexer used by the Swift and Go
//! adapters (P11). The indexer is intentionally language-agnostic: a
//! [`LspProfile`] describes which server to spawn, how to discover
//! source files, and how to map [`LspSymbolKind`] values onto GroundGraph
//! [`NodeKind`] variants. The indexer walks the configured roots,
//! drives the LSP server through the standard `initialize → didOpen
//! → documentSymbol → didClose → shutdown` dance, and accumulates a
//! [`LanguageIndexBatch`] for the engine to merge.
//!
//! Today this is a "structural index" only: we emit files, symbols,
//! and `contains` edges. Call / reference edges will land in a
//! follow-up that uses `textDocument/callHierarchy` (sourcekit-lsp 5.10+
//! / gopls 0.16+ both expose it). Storing the work in two phases keeps
//! the first iteration small enough to be auditable and avoids deeply
//! coupling structural indexing to call-graph latency.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use groundgraph_core::artifact_id::file_id;
use groundgraph_core::edge::EdgeKind;
use groundgraph_core::language_batch::{
    FileArtifact, LanguageIndexBatch, ReferenceEdge, SymbolArtifact, SymbolRange,
};
use groundgraph_core::ArtifactId;
use groundgraph_core::NodeKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lsp_client::{
    file_uri_to_path, path_to_file_uri, LspClient, LspDocumentSymbol, LspOutgoingCall,
    LspSymbolKind,
};

/// Description of a language we drive over LSP. Each profile lives in
/// its own module (`swift_indexer`, `go_indexer`) but they all flow
/// through [`run_profile`].
pub struct LspProfile {
    /// Human-readable label, copied into [`LanguageIndexBatch::language`].
    pub language: &'static str,
    /// LSP `languageId` passed in `didOpen` (`"swift"`, `"go"`, ...).
    pub language_id: &'static str,
    /// File extensions (without the dot) considered source for this
    /// language. Files outside the list are ignored even if the user's
    /// `code_roots` contain them.
    pub file_extensions: &'static [&'static str],
    /// Directory names that should not be descended into. Mostly build
    /// output (`.build/`, `Pods/`, `vendor/`) — matched on each
    /// individual path segment, not as a glob.
    pub skip_dirs: &'static [&'static str],
    /// Suffixes that, when present anywhere in the relative path,
    /// cause the file to be ignored. Useful for `_test.go` /
    /// `_generated.go` heuristics if the operator wants stricter
    /// filtering. The profile leaves this empty by default; the
    /// engine config carries the operator-level `exclude_globs`.
    pub skip_suffixes: &'static [&'static str],
    /// Command to spawn the LSP server. May be overridden via
    /// environment variable (see [`override_command`]).
    pub default_command: &'static str,
    /// Args passed to the LSP server on spawn.
    pub default_args: &'static [&'static str],
    /// Environment variable that operators can set to override
    /// [`default_command`]. We document it next to the profile.
    pub command_env_var: &'static str,
    /// Map an LSP [`LspSymbolKind`] to a GroundGraph [`NodeKind`].
    /// Return `None` to silently drop the symbol — e.g. Go's
    /// `Variable` kind is too noisy to surface as a graph node.
    pub map_kind: fn(LspSymbolKind, parent_kind: Option<NodeKind>) -> Option<NodeKind>,
    /// Build a stable, language-specific qualified name for a symbol.
    /// Receives the file path, the parent's qualified name (if any),
    /// and the symbol's local name. Profile picks the right separator
    /// (Swift uses `.`, Go uses `.` or `/` depending on context).
    pub qualify: fn(file_rel: &str, parent: Option<&str>, name: &str) -> String,
}

#[derive(Debug, Clone)]
pub struct LspIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    /// Operator override for the LSP binary. Falls back to
    /// `profile.default_command` (looked up on `PATH`).
    pub lsp_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LspIndexStats {
    pub files: usize,
    pub symbols: usize,
    pub language: String,
    /// Reason we skipped the language adapter entirely. Empty when the
    /// adapter ran. Mirrors the Dart sidecar's `sidecar_skip_reason`
    /// so the CLI can surface the same UX.
    #[serde(default)]
    pub skip_reason: String,
}

/// Outcome of attempting to run an LSP-based profile. Either we ran
/// successfully and produced a batch, or the indexer skipped (binary
/// missing, no source files) and the engine should treat it as a no-op.
/// The `Indexed` payload is boxed so the enum size stays roughly the
/// same as `Skipped` (avoids `clippy::large_enum_variant`).
pub enum LspIndexOutcome {
    Indexed(Box<LspIndexedBatch>),
    Skipped {
        reason: String,
        language: &'static str,
    },
}

/// Successful indexer output. Owned by [`LspIndexOutcome::Indexed`] via
/// `Box` so the enum's variants stay roughly the same size.
pub struct LspIndexedBatch {
    pub batch: LanguageIndexBatch,
    pub stats: LspIndexStats,
}

/// Drive an [`LspProfile`] against an operator config. Returns
/// [`LspIndexOutcome::Skipped`] (never an `Err`) when the LSP binary
/// is missing, when there are no source files, or when the workspace
/// disables the language. Returns `Err` only for actual I/O or
/// protocol failures the operator should see.
pub fn run_profile(profile: &LspProfile, options: &LspIndexOptions) -> Result<LspIndexOutcome> {
    let command = options
        .lsp_command
        .clone()
        .unwrap_or_else(|| profile.default_command.to_string());
    if !binary_on_path(&command) {
        return Ok(LspIndexOutcome::Skipped {
            reason: format!(
                "未在 PATH 中找到 `{command}`（可设置 `{env}` 或 `.groundgraph.yaml` 的 `{lang}.lsp_command`）",
                env = profile.command_env_var,
                lang = profile.language
            ),
            language: profile.language,
        });
    }

    let files = discover_files(
        profile,
        &options.repo_root,
        &options.code_roots,
        &options.exclude_globs,
    )?;
    if files.is_empty() {
        return Ok(LspIndexOutcome::Skipped {
            reason: format!("未发现任何 `{}` 源文件，跳过 LSP 索引", profile.language),
            language: profile.language,
        });
    }

    let arg_strs: Vec<&str> = profile.default_args.to_vec();
    // P12 复核 [P1]: any runtime LSP failure must surface as a graceful
    // `Skipped` (or partial `Indexed`) so `groundgraph index` keeps
    // working even when sourcekit-lsp / gopls can't reach their cache,
    // crash mid-handshake, etc. The only thing that still propagates is
    // logic-bug-style errors from us (e.g. file extension on PATH check
    // changed mid-call) — none of those are reachable today.
    let mut client = match LspClient::spawn(&command, &arg_strs, &options.repo_root) {
        Ok(c) => c,
        Err(err) => {
            return Ok(LspIndexOutcome::Skipped {
                reason: format!(
                    "无法启动 {} LSP `{}`：{}",
                    profile.language,
                    command,
                    flatten_error_message(&err)
                ),
                language: profile.language,
            });
        }
    };
    let root_uri = path_to_file_uri(&options.repo_root);
    if let Err(err) = client.initialize(&root_uri) {
        return Ok(LspIndexOutcome::Skipped {
            reason: with_server_stderr(
                format!(
                    "{} LSP `{}` initialize 失败：{}",
                    profile.language,
                    command,
                    flatten_error_message(&err)
                ),
                &client.captured_stderr(),
            ),
            language: profile.language,
        });
    }

    let mut batch = LanguageIndexBatch {
        language: profile.language.into(),
        ..Default::default()
    };

    let mut stats = LspIndexStats {
        files: 0,
        symbols: 0,
        language: profile.language.into(),
        skip_reason: String::new(),
    };

    // P13 — track every ingested symbol so the second pass (callHierarchy
    // + references) can find a position to drive the server with, and a
    // way to resolve LSP locations back to a known GroundGraph symbol id.
    let mut ingested: Vec<IngestedSymbol> = Vec::new();
    // Map opened LSP URIs so the probing pass can skip files we never
    // ingested (the resolver only counts something as an edge target
    // when both sides live in indexed files).
    let mut opened_uris: Vec<String> = Vec::new();

    for file in &files {
        let file_artifact = file.artifact.clone();
        batch.files.push(file_artifact.clone());
        stats.files += 1;

        // Cap single-file reads: a giant vendored/generated file would be
        // loaded whole into a String (and shipped to the LSP) and can OOM
        // `groundgraph index` (#186). Skip past the index budget, like the
        // tree-sitter/docs paths already do.
        if crate::source_text::is_oversized_source(&file.absolute) {
            push_partial_warning(
                &mut stats,
                &format!("跳过超大文件 {}（超过索引大小上限）", file.relative),
            );
            continue;
        }
        let text = match std::fs::read_to_string(&file.absolute) {
            Ok(t) => t,
            Err(err) => {
                push_partial_warning(&mut stats, &format!("读取 {} 失败：{err}", file.relative));
                continue;
            }
        };
        let uri = path_to_file_uri(&file.absolute);
        if let Err(err) = client.did_open(&uri, profile.language_id, &text) {
            push_partial_warning(
                &mut stats,
                &format!(
                    "{} LSP didOpen({}) 失败：{}",
                    profile.language,
                    file.relative,
                    flatten_error_message(&err)
                ),
            );
            // If notify fails, the LSP transport is wedged — abort
            // the rest of the run, but keep what we have already.
            break;
        }
        opened_uris.push(uri.clone());
        let symbols = match client.document_symbol(&uri) {
            Ok(s) => s,
            Err(err) => {
                push_partial_warning(
                    &mut stats,
                    &format!(
                        "{} LSP documentSymbol({}) 失败：{}",
                        profile.language,
                        file.relative,
                        flatten_error_message(&err)
                    ),
                );
                break;
            }
        };
        let symbol_count = ingest_symbols(
            profile,
            &file.relative,
            &uri,
            &symbols,
            &mut batch,
            &mut ingested,
        );
        stats.symbols += symbol_count;
        // NOTE: do NOT didClose here — we want the document to stay
        // open so the second pass can run callHierarchy / references
        // against the in-memory text. Files are released when the
        // server exits or when we explicitly close them below.
    }

    // P13 — second pass: drive `prepareCallHierarchy → outgoingCalls`
    // and `textDocument/references` for every callable symbol the
    // server already saw. Resolution is best-effort: edges whose
    // target / caller lives outside the indexed roots are silently
    // dropped (we don't synthesise nodes for stdlib / 3rd party).
    if !ingested.is_empty() {
        // sourcekit-lsp's IndexStoreDB picks up a freshly-built
        // `.build/.../index/store` asynchronously, so the very first
        // `prepareCallHierarchy` after `initialize` can return `[]`
        // even when the build is on disk. Poll the first callable
        // symbol with a short budget so we don't drop every edge.
        warmup_call_hierarchy(&mut client, &ingested);
        let resolver = SymbolResolver::build(&options.repo_root, &ingested);
        probe_call_hierarchy_and_references(
            &mut client,
            profile,
            &ingested,
            &resolver,
            &mut batch,
            &mut stats,
        );
    }

    // Best-effort close every file we opened so the server can release
    // its in-memory caches before shutdown.
    for uri in &opened_uris {
        let _ = client.did_close(uri);
    }

    // Best-effort shutdown — failures are surfaced as `Err` only when
    // the user has set RUST_BACKTRACE=1 etc., otherwise we swallow
    // them so a flaky server does not invalidate a successful index.
    if let Err(err) = client.shutdown() {
        tracing_skip_reason_into(&mut stats, &err);
    }

    // If we got nothing useful out, surface a `Skipped` so the CLI
    // does not present an empty Swift / Go section as success.
    if batch.symbols.is_empty() && !stats.skip_reason.is_empty() {
        return Ok(LspIndexOutcome::Skipped {
            reason: stats.skip_reason,
            language: profile.language,
        });
    }

    Ok(LspIndexOutcome::Indexed(Box::new(LspIndexedBatch {
        batch,
        stats,
    })))
}

/// Format an anyhow error chain into a single sentence suitable for
/// the operator-facing `skip_reason` field. Mirrors the helper the
/// Dart sidecar uses.
fn flatten_error_message(err: &anyhow::Error) -> String {
    let mut out = err.to_string();
    for cause in err.chain().skip(1) {
        out.push('：');
        out.push_str(&cause.to_string());
    }
    out
}

/// Fold a server's captured stderr tail (#214) into a skip reason so the
/// operator sees the server's *own* error (e.g. "sourcekitd crashed") rather
/// than just our generic "initialize 失败". Blank tails are omitted; long
/// tails are truncated on a UTF-8 boundary, keeping the most recent bytes.
fn with_server_stderr(reason: String, server_stderr: &str) -> String {
    let tail = server_stderr.trim();
    if tail.is_empty() {
        return reason;
    }
    const MAX: usize = 600;
    let shown = if tail.len() > MAX {
        let mut start = tail.len() - MAX;
        while start < tail.len() && !tail.is_char_boundary(start) {
            start += 1;
        }
        format!("…{}", &tail[start..])
    } else {
        tail.to_string()
    };
    format!("{reason}（server stderr: {shown}）")
}

/// Maximum number of detailed partial-warning segments kept in
/// `skip_reason`. A flapping LSP (e.g. sourcekit-lsp timing out on
/// every symbol of a large Swift repo) used to push one per-symbol
/// failure each into `skip_reason`, joined by `；`, with no cap —
/// ballooning the field to hundreds of KB and flooding stdout / the
/// MCP payload. We keep the first few details and fold the rest into a
/// running count.
const MAX_PARTIAL_WARNING_DETAILS: usize = 8;
const PARTIAL_WARNING_FOLD_PREFIX: &str = "…（另有 ";
const PARTIAL_WARNING_FOLD_SUFFIX: &str = " 条 LSP 警告已折叠）";

/// Append a partial-run warning into `stats.skip_reason`, joining
/// existing entries with `；`. Detail is capped at
/// [`MAX_PARTIAL_WARNING_DETAILS`]; further warnings only bump a
/// trailing "(folded N more)" counter so a flapping LSP cannot balloon
/// the field into a multi-hundred-KB single line.
fn push_partial_warning(stats: &mut LspIndexStats, msg: &str) {
    // Already folding: just bump the trailing counter and return. This
    // keeps the field at a fixed size no matter how many failures flap.
    if let Some((head, count)) = take_fold_count(&stats.skip_reason) {
        stats.skip_reason = format!(
            "{head}；{PARTIAL_WARNING_FOLD_PREFIX}{}{PARTIAL_WARNING_FOLD_SUFFIX}",
            count + 1
        );
        return;
    }
    if stats.skip_reason.is_empty() {
        stats.skip_reason = msg.to_string();
        return;
    }
    if stats.skip_reason.split('；').count() < MAX_PARTIAL_WARNING_DETAILS {
        stats.skip_reason.push('；');
        stats.skip_reason.push_str(msg);
    } else {
        // Cap reached: start folding with this overflow message as #1.
        stats.skip_reason = format!(
            "{}；{PARTIAL_WARNING_FOLD_PREFIX}1{PARTIAL_WARNING_FOLD_SUFFIX}",
            stats.skip_reason
        );
    }
}

/// If `s` ends with a fold marker (`…（另有 N 条 LSP 警告已折叠）`),
/// return the head (without the trailing `；` + marker) and the folded
/// count `N`. Returns `None` when no marker is present.
fn take_fold_count(s: &str) -> Option<(&str, usize)> {
    let marker_start = s.rfind(PARTIAL_WARNING_FOLD_PREFIX)?;
    let after_prefix = &s[marker_start + PARTIAL_WARNING_FOLD_PREFIX.len()..];
    let count: usize = after_prefix
        .strip_suffix(PARTIAL_WARNING_FOLD_SUFFIX)?
        .parse()
        .ok()?;
    let head = s[..marker_start]
        .strip_suffix('；')
        .unwrap_or(&s[..marker_start]);
    Some((head, count))
}

/// Index built once per `run_profile` invocation that turns an LSP
/// `Location.uri + position.line` into a known GroundGraph
/// [`ArtifactId`]. Falls back to `None` for cross-file calls into
/// stdlib / 3rd-party code that we never ingested.
struct SymbolResolver {
    repo_root: PathBuf,
    /// Per relative-path-with-`/` separators: sorted-by-start-line
    /// table of `(start_line_1based, end_line_1based, symbol_id, kind)`.
    by_path: HashMap<String, Vec<(u32, u32, ArtifactId, NodeKind)>>,
}

impl SymbolResolver {
    fn build(repo_root: &Path, ingested: &[IngestedSymbol]) -> Self {
        let mut by_path: HashMap<String, Vec<(u32, u32, ArtifactId, NodeKind)>> = HashMap::new();
        for sym in ingested {
            by_path.entry(sym.file_rel.clone()).or_default().push((
                sym.start_line,
                sym.end_line,
                sym.symbol_id.clone(),
                sym.kind,
            ));
        }
        // Sort each bucket by start_line ascending so the innermost
        // enclosing symbol wins ties when we walk in reverse.
        for ranges in by_path.values_mut() {
            ranges.sort_by_key(|(start, _, _, _)| *start);
        }
        // macOS tempfiles live under `/var/folders/...` which is a
        // symlink to `/private/var/folders/...`. sourcekit-lsp / gopls
        // emit the symlink-resolved form, so we canonicalize the root
        // here to make `strip_prefix` work without surprises.
        let repo_root =
            std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
        SymbolResolver { repo_root, by_path }
    }

    /// Resolve a 0-based LSP `(uri, line)` to the innermost GroundGraph
    /// symbol enclosing that line. Returns `None` when the URI sits
    /// outside the repo root or the path was never ingested.
    fn resolve(&self, uri: &str, line_zero_based: u32) -> Option<(ArtifactId, NodeKind, String)> {
        let abs = file_uri_to_path(uri)?;
        // Canonicalize the URI-derived path too so both sides agree on
        // whether the symlink prefix is present. Fall back to the
        // raw path when canonicalize fails (file disappeared, etc.).
        let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
        let rel = abs.strip_prefix(&self.repo_root).ok()?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let ranges = self.by_path.get(&rel_str)?;
        let line_1 = line_zero_based.saturating_add(1);
        // Innermost: the symbol with the *largest* start_line still
        // <= line_1 and end_line >= line_1.
        let mut best: Option<&(u32, u32, ArtifactId, NodeKind)> = None;
        for entry in ranges {
            if entry.0 <= line_1 && entry.1 >= line_1 {
                if let Some(prev) = best {
                    if entry.0 > prev.0 {
                        best = Some(entry);
                    }
                } else {
                    best = Some(entry);
                }
            }
        }
        let chosen = best?;
        Some((chosen.2.clone(), chosen.3, rel_str))
    }
}

/// True when this kind has a meaningful body the server can mine for
/// call-hierarchy / references. Types / enums / protocols do not.
fn is_probable_callsite_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::DartMethod
            | NodeKind::DartFunction
            | NodeKind::DartConstructor
            | NodeKind::SwiftMethod
            | NodeKind::SwiftFunction
            | NodeKind::SwiftInitializer
            | NodeKind::GoMethod
            | NodeKind::GoFunction
            | NodeKind::PythonMethod
            | NodeKind::PythonFunction
            | NodeKind::TestCase
    )
}

/// Resolve callHierarchy outgoing calls and textDocument/references
/// for every callable symbol the structural pass surfaced. Best-effort:
/// per-symbol failures are accumulated into `stats.skip_reason` and
/// the loop continues so a single buggy server response does not lose
/// the rest of the graph.
fn probe_call_hierarchy_and_references(
    client: &mut LspClient,
    profile: &LspProfile,
    ingested: &[IngestedSymbol],
    resolver: &SymbolResolver,
    batch: &mut LanguageIndexBatch,
    stats: &mut LspIndexStats,
) {
    use std::collections::HashSet;

    let resolver_label = format!("{}_lsp", profile.language);
    // Dedupe `(from, to, kind)` so two LSP-reported hits on the same
    // callee only emit one edge.
    let mut seen: HashSet<(String, String, EdgeKind)> = HashSet::new();

    let mut push_edge = |batch: &mut LanguageIndexBatch,
                         from: &ArtifactId,
                         to: &ArtifactId,
                         kind: EdgeKind,
                         file_rel: &str,
                         line: u32| {
        if from.as_str() == to.as_str() {
            return; // skip self-edges; recursion is not interesting here
        }
        let key = (from.as_str().to_string(), to.as_str().to_string(), kind);
        if !seen.insert(key) {
            return;
        }
        batch.references.push(ReferenceEdge {
            from_symbol_id: from.clone(),
            to_symbol_id: to.clone(),
            kind,
            source_file: file_rel.to_string(),
            line,
            snippet: String::new(),
            resolver: resolver_label.clone(),
        });
    };

    for sym in ingested {
        if !is_probable_callsite_kind(sym.kind) {
            continue;
        }
        // --- 1) outgoing calls via prepareCallHierarchy → outgoingCalls ---
        //
        // P15 evidence accuracy: LSP `outgoingCalls[].fromRanges`
        // are *caller-side* ranges (in `sym.file_uri`) that name the
        // actual call sites. The earlier implementation used the
        // callee's `selection_line`, which records the *declaration*
        // of the called function — useless for "go to the call site
        // that produced this edge". We now record one edge per
        // call site (deduped by `(from, to, kind)` in `push_edge`)
        // using `(sym.file_rel, fromRange.line + 1)` as evidence.
        // When the server emits no `fromRanges` (older servers),
        // fall back to the caller's identifier line — strictly
        // worse than the call site but still inside the caller's
        // file, never the callee declaration.
        match probe_outgoing_calls(client, sym) {
            Ok(items) => {
                for outgoing in items {
                    let Some((target_id, _kind, _target_rel)) =
                        resolver.resolve(&outgoing.to.uri, outgoing.to.selection_line)
                    else {
                        continue;
                    };
                    // First `fromRange` wins for evidence; the
                    // `(from, to, kind)` dedup below would collapse
                    // additional rows anyway. When the server emits
                    // no `fromRanges`, anchor on the caller's
                    // identifier line — still inside the caller
                    // file, which is what auditors need.
                    let (evidence_line_zero_based,) = outgoing
                        .from_ranges
                        .first()
                        .map(|(line, _ch)| (*line,))
                        .unwrap_or((sym.selection_line,));
                    push_edge(
                        batch,
                        &sym.symbol_id,
                        &target_id,
                        EdgeKind::Calls,
                        &sym.file_rel,
                        evidence_line_zero_based.saturating_add(1),
                    );
                }
            }
            Err(err) => {
                push_partial_warning(
                    stats,
                    &format!(
                        "{} LSP callHierarchy({}:{}) 失败：{}",
                        profile.language,
                        sym.file_rel,
                        sym.start_line,
                        flatten_error_message(&err),
                    ),
                );
            }
        }
        // --- 2) inbound references via textDocument/references ---
        // We model each location as a `References` edge from the
        // enclosing caller symbol back to us. The structural pass
        // already produced `Calls` edges where call-hierarchy exists;
        // textDocument/references picks up the rest (string-keyed
        // lookups, plain property reads, etc.).
        match client.references(&sym.file_uri, sym.selection_line, sym.selection_character) {
            Ok(locs) => {
                for loc in locs {
                    let Some((caller_id, _kind, caller_rel)) = resolver.resolve(&loc.uri, loc.line)
                    else {
                        continue;
                    };
                    push_edge(
                        batch,
                        &caller_id,
                        &sym.symbol_id,
                        EdgeKind::References,
                        &caller_rel,
                        loc.line.saturating_add(1),
                    );
                }
            }
            Err(err) => {
                push_partial_warning(
                    stats,
                    &format!(
                        "{} LSP references({}:{}) 失败：{}",
                        profile.language,
                        sym.file_rel,
                        sym.start_line,
                        flatten_error_message(&err),
                    ),
                );
            }
        }
    }
}

/// Best-effort wait for the LSP server's call-hierarchy index to come
/// online. We pick a callable symbol from each opened file (so that
/// per-file readiness is exercised) and retry `prepareCallHierarchy`
/// until it answers, sleeping `WARMUP_SLEEP` between attempts.
/// The whole pass is capped at `WARMUP_TOTAL_BUDGET` (15s, sized for
/// sourcekit-lsp's IndexStoreDB cold start — see the constant below)
/// so a wedged server can never stall `groundgraph index` beyond that.
///
/// We never propagate errors here: warmup is a best-effort signal,
/// and the subsequent probing pass will still respect graceful
/// degradation if the server is permanently empty.
fn warmup_call_hierarchy(client: &mut LspClient, ingested: &[IngestedSymbol]) {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};
    const WARMUP_SLEEP: Duration = Duration::from_millis(250);
    // sourcekit-lsp empirically needs ~10s after `initialize` before
    // its IndexStoreDB watcher picks up a freshly-built index. gopls
    // is faster but does not mind the extra polling — empty results
    // return instantly, so the worst case here is one extra request.
    const WARMUP_TOTAL_BUDGET: Duration = Duration::from_secs(15);

    // Pick at most one callable per file so we touch every translation
    // unit (sourcekit-lsp / gopls warm up per-file). Stable order so the
    // behaviour is reproducible across runs.
    let mut probes: Vec<&IngestedSymbol> = Vec::new();
    let mut seen_files: HashSet<&str> = HashSet::new();
    for sym in ingested {
        if !is_probable_callsite_kind(sym.kind) {
            continue;
        }
        if seen_files.insert(sym.file_rel.as_str()) {
            probes.push(sym);
        }
    }

    let start = Instant::now();
    'outer: for sym in probes {
        while start.elapsed() < WARMUP_TOTAL_BUDGET {
            match client.prepare_call_hierarchy(
                &sym.file_uri,
                sym.selection_line,
                sym.selection_character,
            ) {
                Ok(items) if !items.is_empty() => continue 'outer,
                Ok(_) => {
                    std::thread::sleep(WARMUP_SLEEP);
                }
                Err(_) => break, // hard failure — bail on this probe, try the next file
            }
        }
    }
}

fn probe_outgoing_calls(
    client: &mut LspClient,
    sym: &IngestedSymbol,
) -> Result<Vec<LspOutgoingCall>> {
    let items = client.prepare_call_hierarchy(
        &sym.file_uri,
        sym.selection_line,
        sym.selection_character,
    )?;
    let mut out = Vec::new();
    for item in items {
        let calls = client.outgoing_calls(&item)?;
        out.extend(calls);
    }
    Ok(out)
}

fn tracing_skip_reason_into(stats: &mut LspIndexStats, err: &anyhow::Error) {
    // Keep the structured stats clean — only carry the message, not the
    // full chain. The CLI prefers a single human-readable sentence.
    let msg = err.to_string();
    if !msg.is_empty() {
        if stats.skip_reason.is_empty() {
            stats.skip_reason = format!("LSP shutdown 警告：{msg}");
        } else {
            stats.skip_reason.push('；');
            stats.skip_reason.push_str(&msg);
        }
    }
}

struct DiscoveredFile {
    relative: String,
    absolute: PathBuf,
    artifact: FileArtifact,
}

fn discover_files(
    profile: &LspProfile,
    repo_root: &Path,
    code_roots: &[PathBuf],
    exclude_globs: &[String],
) -> Result<Vec<DiscoveredFile>> {
    let mut out: Vec<DiscoveredFile> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    // Compile the (constant) exclude globs once instead of re-collecting each
    // pattern's chars for every candidate file (#142).
    let exclude = ExcludeGlobs::compile(exclude_globs);
    for code_root in code_roots {
        let abs_root = repo_root.join(code_root);
        if !abs_root.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&abs_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if profile.skip_dirs.contains(&name) {
                        continue;
                    }
                }
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(extension) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            if !profile.file_extensions.contains(&extension) {
                continue;
            }
            let rel = path
                .strip_prefix(repo_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if profile.skip_suffixes.iter().any(|s| rel.ends_with(s)) {
                continue;
            }
            if exclude.matches(&rel) {
                continue;
            }
            // Also skip when any path segment is in skip_dirs (handles
            // the case where the operator passed a high root like the
            // repo root and we hit `.build/` nested inside Sources).
            if rel.split('/').any(|seg| profile.skip_dirs.contains(&seg)) {
                continue;
            }
            if seen.insert(rel.clone(), ()).is_some() {
                continue;
            }
            // Skip oversized files: read whole into a String + hashed + fed to
            // the LSP; an accidental giant blob can OOM the indexer (#186).
            if crate::source_text::is_oversized_source(path) {
                continue;
            }
            let source = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
            // `walkdir` preserves whatever shape the seed had — e.g.
            // when `code_roots = ["."]` the absolute path keeps a `./`
            // mid-segment (`/repo/./cmd/server/main.go`). gopls treats
            // such URIs as foreign files and refuses callHierarchy
            // with `no package metadata`, so we always rebuild the
            // absolute path from `repo_root + relative` here.
            let canonical_abs = repo_root.join(&rel);
            out.push(DiscoveredFile {
                artifact: FileArtifact {
                    id: file_id(&rel),
                    path: rel.clone(),
                    language: profile.language.into(),
                    content_hash: hash,
                },
                relative: rel,
                absolute: canonical_abs,
            });
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

/// Per-recursion bookkeeping used while walking a `documentSymbol`
/// tree. Bundled into one struct so the recursive helper has a small
/// argument list and clippy stays happy with `too_many_arguments`.
struct SymbolVisit<'a> {
    profile: &'a LspProfile,
    file_rel: &'a str,
    file_uri: &'a str,
    parent_qual: Option<&'a str>,
    parent_id: Option<&'a ArtifactId>,
    parent_kind: Option<NodeKind>,
}

/// Tracking entry recorded as we ingest one symbol. The fields are
/// exactly what the call-hierarchy / references probing pass needs:
/// the GroundGraph symbol id, its identifier-cursor position (0-based,
/// LSP convention) and the file URI we already opened. The 1-based
/// line range is duplicated here so we can quickly attach evidence
/// without crossing back into the SQLite store.
#[derive(Debug, Clone)]
struct IngestedSymbol {
    file_rel: String,
    file_uri: String,
    symbol_id: ArtifactId,
    kind: NodeKind,
    /// 0-based — fed straight to `prepareCallHierarchy` / `references`.
    selection_line: u32,
    selection_character: u32,
    /// 1-based (matches `SymbolRange`) for evidence printing.
    start_line: u32,
    end_line: u32,
}

fn ingest_symbols(
    profile: &LspProfile,
    file_rel: &str,
    file_uri: &str,
    symbols: &[LspDocumentSymbol],
    batch: &mut LanguageIndexBatch,
    ingested: &mut Vec<IngestedSymbol>,
) -> usize {
    let mut count = 0;
    let frame = SymbolVisit {
        profile,
        file_rel,
        file_uri,
        parent_qual: None,
        parent_id: None,
        parent_kind: None,
    };
    visit_symbols(&frame, symbols, batch, &mut count, ingested);
    count
}

fn visit_symbols(
    frame: &SymbolVisit<'_>,
    symbols: &[LspDocumentSymbol],
    batch: &mut LanguageIndexBatch,
    count: &mut usize,
    ingested: &mut Vec<IngestedSymbol>,
) {
    for symbol in symbols {
        let Some(kind) = (frame.profile.map_kind)(symbol.kind, frame.parent_kind) else {
            // Recurse so children of an ignored container are still
            // surfaced (e.g. Swift `extension` blocks).
            visit_symbols(frame, &symbol.children, batch, count, ingested);
            continue;
        };
        let qualified = (frame.profile.qualify)(frame.file_rel, frame.parent_qual, &symbol.name);
        let id = ArtifactId::new(format!("{}::{}", frame.profile.language, qualified));
        let start_line = symbol.start_line.saturating_add(1);
        let end_line = symbol.end_line.saturating_add(1).max(start_line);
        let symbol_artifact = SymbolArtifact {
            id: id.clone(),
            kind,
            path: frame.file_rel.into(),
            name: symbol.name.clone(),
            qualified_name: qualified.clone(),
            start_line,
            end_line,
            parent_symbol_id: frame.parent_id.cloned(),
            metadata_json: None,
        };
        batch.symbol_ranges.push(SymbolRange {
            file_path: frame.file_rel.into(),
            symbol_id: id.clone(),
            start_line,
            end_line,
            symbol_kind: kind,
            qualified_name: qualified.clone(),
            parent_symbol_id: frame.parent_id.cloned(),
        });
        batch.symbols.push(symbol_artifact);
        ingested.push(IngestedSymbol {
            file_rel: frame.file_rel.to_string(),
            file_uri: frame.file_uri.to_string(),
            symbol_id: id.clone(),
            kind,
            selection_line: symbol.selection_line,
            selection_character: symbol.selection_character,
            start_line,
            end_line,
        });
        *count += 1;

        let child_frame = SymbolVisit {
            profile: frame.profile,
            file_rel: frame.file_rel,
            file_uri: frame.file_uri,
            parent_qual: Some(&qualified),
            parent_id: Some(&id),
            parent_kind: Some(kind),
        };
        visit_symbols(&child_frame, &symbol.children, batch, count, ingested);
    }
}

/// Lightweight glob used for `exclude_globs`. Matches the same subset
/// the Dart indexer accepts:
/// - `**` — any number of characters including `/` (cross-directory).
/// - `*`  — any number of characters *within a single segment* (does
///   not cross `/`).
/// - `?`  — a single character (any except `/`).
/// - Literal characters compare 1-for-1.
///
/// Implemented as a small recursive descent matcher so the
/// `*` vs `**` distinction is explicit. Backtracking is fine for the
/// patterns operators put in `.groundgraph.yaml` (a handful of segments
/// at most).
/// Single-pattern convenience over [`glob_match_chars`]. Production discovery
/// uses the compiled [`ExcludeGlobs`] (#142); this remains as the readable
/// reference the glob tests assert the compiled matcher against.
#[cfg(test)]
pub(crate) fn simple_glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = path.chars().collect();
    glob_match_chars(&pat, &txt)
}

/// Core matcher over pre-collected char slices. `simple_glob_match` and the
/// compiled [`ExcludeGlobs`] both funnel here so they share identical semantics.
fn glob_match_chars(pat: &[char], txt: &[char]) -> bool {
    // Failure memo: patterns with several `*`/`**` segments otherwise
    // backtrack exponentially (`**/**/**/x` vs a long path froze whole
    // index runs — issues2.md #50). State space is (pi, ti), so caching
    // failed states bounds the walk at O(P×T) cached entries.
    let mut failed: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    glob_match_rec(pat, 0, txt, 0, &mut failed)
}

/// Pre-compiled `exclude_globs`. `discover_files` checks every candidate file
/// against the operator's globs; collecting each (constant) pattern into a
/// `Vec<char>` on *every* call meant a 100k-file × 5-glob walk did millions of
/// throwaway allocations (#142). Compiling the patterns once, and collecting
/// each path's chars once per call (not once per glob), removes that churn while
/// reusing the exact `simple_glob_match` semantics.
pub(crate) struct ExcludeGlobs {
    pats: Vec<Vec<char>>,
}

impl ExcludeGlobs {
    pub(crate) fn compile(globs: &[String]) -> Self {
        Self {
            pats: globs.iter().map(|g| g.chars().collect()).collect(),
        }
    }

    /// `true` if any glob matches `path`. An empty set matches nothing.
    pub(crate) fn matches(&self, path: &str) -> bool {
        if self.pats.is_empty() {
            return false;
        }
        let txt: Vec<char> = path.chars().collect();
        self.pats.iter().any(|pat| glob_match_chars(pat, &txt))
    }
}

fn glob_match_rec(
    pat: &[char],
    start_pi: usize,
    txt: &[char],
    start_ti: usize,
    failed: &mut std::collections::HashSet<(usize, usize)>,
) -> bool {
    if failed.contains(&(start_pi, start_ti)) {
        return false;
    }
    let mut pi = start_pi;
    let mut ti = start_ti;
    let matched = loop {
        if pi >= pat.len() {
            break ti == txt.len();
        }
        if pat[pi] == '*' {
            let double = pi + 1 < pat.len() && pat[pi + 1] == '*';
            if double {
                // `**` — consume optional trailing `/` so `**/foo`
                // also matches a path that starts at `foo`.
                let mut next = pi + 2;
                if next < pat.len() && pat[next] == '/' {
                    next += 1;
                }
                // Try matching the remainder against every suffix of
                // `txt[ti..]` (including the empty suffix).
                break (ti..=txt.len()).any(|j| glob_match_rec(pat, next, txt, j, failed));
            }
            // `*` — match within a single segment.
            let next = pi + 1;
            let mut hit = false;
            for j in ti..=txt.len() {
                // Stop expanding once we hit a `/`.
                if j > ti && txt[j - 1] == '/' {
                    break;
                }
                if glob_match_rec(pat, next, txt, j, failed) {
                    hit = true;
                    break;
                }
            }
            break hit;
        }
        if ti >= txt.len() {
            break false;
        }
        match pat[pi] {
            '?' => {
                if txt[ti] == '/' {
                    break false;
                }
            }
            c if c == txt[ti] => {}
            _ => break false,
        }
        pi += 1;
        ti += 1;
    };
    if !matched {
        failed.insert((start_pi, start_ti));
    }
    matched
}

/// Return `true` when `command` resolves to a binary on the system
/// `PATH` (or is itself an absolute path that exists). Mirrors the
/// behaviour of the Dart sidecar's binary check.
pub fn binary_on_path(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_absolute() {
        return path.is_file();
    }
    if let Ok(env_path) = std::env::var("PATH") {
        for dir in env_path.split(if cfg!(windows) { ';' } else { ':' }) {
            let candidate = Path::new(dir).join(command);
            if candidate.is_file() {
                return true;
            }
            // On Windows fall back to the common `.exe` suffix.
            if cfg!(windows) {
                let with_exe = candidate.with_extension("exe");
                if with_exe.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #214: when an LSP server fails to come up, its own stderr (captured by
    /// the drainer) is the real cause — fold it into the skip reason instead of
    /// just reporting "initialize failed". Empty tails are omitted.
    #[test]
    fn with_server_stderr_appends_captured_tail_when_present() {
        let reason = "swift LSP `sourcekit-lsp` initialize 失败：timeout".to_string();
        assert_eq!(
            with_server_stderr(reason.clone(), "   \n  "),
            reason,
            "blank stderr must not append a noisy empty segment"
        );

        let enriched = with_server_stderr(reason.clone(), "fatal error: sourcekitd crashed\n");
        assert!(enriched.starts_with(&reason));
        assert!(
            enriched.contains("server stderr:") && enriched.contains("sourcekitd crashed"),
            "captured stderr must be surfaced, got: {enriched}"
        );
    }

    /// A pathologically chatty server must not blow up the skip reason: the
    /// tail is truncated, and truncation never splits a UTF-8 char.
    #[test]
    fn with_server_stderr_truncates_long_tail_on_char_boundary() {
        let reason = "go LSP `gopls` 失败".to_string();
        let tail = "日志".repeat(1000); // 6 KiB of multi-byte chars
        let enriched = with_server_stderr(reason, &tail);
        assert!(
            enriched.len() < 1200,
            "tail must be truncated, got {} bytes",
            enriched.len()
        );
        assert!(enriched.contains('…'), "truncation marker expected");
        // The fact that this owns a valid String already proves boundary-safety,
        // but assert the tail content is intact Unicode.
        assert!(enriched.contains('日'));
    }

    fn dummy_qualify(file_rel: &str, parent: Option<&str>, name: &str) -> String {
        match parent {
            Some(p) => format!("{p}.{name}"),
            None => format!("{file_rel}#{name}"),
        }
    }

    fn dummy_map(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
        match kind {
            LspSymbolKind::Class => Some(NodeKind::SwiftClass),
            LspSymbolKind::Method => Some(NodeKind::SwiftMethod),
            _ => None,
        }
    }

    fn profile() -> LspProfile {
        LspProfile {
            language: "swift",
            language_id: "swift",
            file_extensions: &["swift"],
            skip_dirs: &[".build", "Pods"],
            skip_suffixes: &[],
            default_command: "sourcekit-lsp",
            default_args: &[],
            command_env_var: "GROUNDGRAPH_SWIFT_LSP_BIN",
            map_kind: dummy_map,
            qualify: dummy_qualify,
        }
    }

    #[test]
    fn ingest_symbols_emits_parent_child_pairs_and_skips_unmapped_kinds() {
        let profile = profile();
        let tree = vec![LspDocumentSymbol {
            name: "Greeter".into(),
            detail: None,
            kind: LspSymbolKind::Class,
            start_line: 0,
            end_line: 9,
            selection_line: 0,
            selection_character: 6,
            children: vec![
                LspDocumentSymbol {
                    name: "greet".into(),
                    detail: None,
                    kind: LspSymbolKind::Method,
                    start_line: 1,
                    end_line: 4,
                    selection_line: 1,
                    selection_character: 5,
                    children: Vec::new(),
                },
                LspDocumentSymbol {
                    name: "noise".into(),
                    detail: None,
                    kind: LspSymbolKind::Variable,
                    start_line: 5,
                    end_line: 5,
                    selection_line: 5,
                    selection_character: 0,
                    children: Vec::new(),
                },
            ],
        }];
        let mut batch = LanguageIndexBatch {
            language: profile.language.into(),
            ..Default::default()
        };
        let mut ingested = Vec::new();
        let n = ingest_symbols(
            &profile,
            "Sources/Greeter.swift",
            "file:///tmp/Sources/Greeter.swift",
            &tree,
            &mut batch,
            &mut ingested,
        );
        assert_eq!(n, 2);
        assert_eq!(ingested.len(), 2);
        assert_eq!(ingested[1].selection_line, 1);
        assert_eq!(ingested[1].selection_character, 5);
        assert_eq!(batch.symbols.len(), 2);
        let class = &batch.symbols[0];
        assert_eq!(class.kind, NodeKind::SwiftClass);
        assert_eq!(class.start_line, 1);
        assert_eq!(class.end_line, 10);
        assert!(class.parent_symbol_id.is_none());

        let method = &batch.symbols[1];
        assert_eq!(method.kind, NodeKind::SwiftMethod);
        assert_eq!(
            method.parent_symbol_id.as_ref().unwrap().as_str(),
            class.id.as_str()
        );
        assert_eq!(method.start_line, 2);
        assert_eq!(method.end_line, 5);
        assert_eq!(
            method.qualified_name,
            format!("{}.greet", class.qualified_name)
        );
    }

    #[test]
    fn ingest_symbols_recurses_into_unmapped_containers() {
        // An ignored container (e.g. Swift `extension`) should still let
        // us discover its mapped children.
        fn passthrough_map(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
            match kind {
                LspSymbolKind::Method => Some(NodeKind::SwiftMethod),
                _ => None,
            }
        }
        let mut profile = profile();
        profile.map_kind = passthrough_map;

        let tree = vec![LspDocumentSymbol {
            name: "Greeter+API".into(),
            detail: None,
            kind: LspSymbolKind::Namespace,
            start_line: 0,
            end_line: 20,
            selection_line: 0,
            selection_character: 10,
            children: vec![LspDocumentSymbol {
                name: "greetAsync".into(),
                detail: None,
                kind: LspSymbolKind::Method,
                start_line: 3,
                end_line: 6,
                selection_line: 3,
                selection_character: 8,
                children: Vec::new(),
            }],
        }];
        let mut batch = LanguageIndexBatch::default();
        let mut ingested = Vec::new();
        let n = ingest_symbols(
            &profile,
            "Sources/Greeter+API.swift",
            "file:///tmp/Sources/Greeter+API.swift",
            &tree,
            &mut batch,
            &mut ingested,
        );
        assert_eq!(n, 1);
        assert_eq!(batch.symbols.len(), 1);
        assert_eq!(batch.symbols[0].name, "greetAsync");
        assert!(batch.symbols[0].parent_symbol_id.is_none());
    }

    #[test]
    fn simple_glob_match_handles_dot_star_and_double_star() {
        assert!(simple_glob_match(
            "**/*.gen.go",
            "internal/api/users.gen.go"
        ));
        assert!(simple_glob_match(
            "**/.build/**",
            "Foo/.build/release/lib.swift"
        ));
        assert!(!simple_glob_match("**/.build/**", "Foo/Sources/lib.swift"));
        assert!(simple_glob_match("*.swift", "Hello.swift"));
        assert!(!simple_glob_match("*.swift", "Sources/Hello.swift"));
    }

    /// #142: the compiled `ExcludeGlobs` matcher must be byte-for-byte
    /// equivalent to calling `simple_glob_match` per pattern — it only hoists
    /// the (constant) pattern `Vec<char>` collection out of the per-file loop,
    /// never changes the match semantics. Multi-pattern `matches` is the OR of
    /// the individual patterns (a file is excluded if *any* glob matches).
    #[test]
    fn exclude_globs_compiled_matches_simple_glob_match() {
        let cases = [
            ("**/*.gen.go", "internal/api/users.gen.go"),
            ("**/.build/**", "Foo/.build/release/lib.swift"),
            ("**/.build/**", "Foo/Sources/lib.swift"),
            ("*.swift", "Hello.swift"),
            ("*.swift", "Sources/Hello.swift"),
            ("src/?.rs", "src/a.rs"),
            ("src/?.rs", "src/ab.rs"),
            ("a/**/z.rs", "a/b/c/z.rs"),
            ("literal.txt", "literal.txt"),
            ("literal.txt", "other.txt"),
        ];
        for (pat, path) in cases {
            let compiled = ExcludeGlobs::compile(&[pat.to_string()]);
            assert_eq!(
                compiled.matches(path),
                simple_glob_match(pat, path),
                "compiled vs per-call mismatch for ({pat:?}, {path:?})",
            );
        }

        // Multi-pattern: OR semantics, and an empty set never excludes.
        let globs = vec!["**/*.gen.go".to_string(), "*.swift".to_string()];
        let compiled = ExcludeGlobs::compile(&globs);
        for path in ["x/y.gen.go", "Hello.swift", "src/keep.rs"] {
            let expected = globs.iter().any(|g| simple_glob_match(g, path));
            assert_eq!(compiled.matches(path), expected, "OR mismatch for {path:?}");
        }
        assert!(
            !ExcludeGlobs::compile(&[]).matches("anything/at/all.rs"),
            "an empty exclude set excludes nothing",
        );
    }

    /// issues2.md #50: a user glob with several `**` segments used to
    /// backtrack exponentially (O(L^N)) against long non-matching paths —
    /// one bad exclude line froze `groundgraph index`. Must finish instantly.
    #[test]
    fn simple_glob_match_survives_pathological_double_star_patterns() {
        let pattern = "**/**/**/**/**/**/**/**/needle.swift";
        let path = (0..50)
            .map(|i| format!("d{i}"))
            .collect::<Vec<_>>()
            .join("/")
            + "/leaf.swift";
        let started = std::time::Instant::now();
        assert!(!simple_glob_match(pattern, &path));
        // generous bound: memoized matching is microseconds; the old
        // backtracker would burn effectively forever.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "pathological glob took {:?}",
            started.elapsed()
        );
        // And the same pattern still matches when it should.
        let good = (0..10)
            .map(|i| format!("d{i}"))
            .collect::<Vec<_>>()
            .join("/")
            + "/needle.swift";
        assert!(simple_glob_match(pattern, &good));
    }

    #[test]
    fn discover_files_filters_by_extension_and_exclude_globs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("Sources/App")).unwrap();
        std::fs::create_dir_all(root.join(".build/release")).unwrap();
        std::fs::write(root.join("Sources/App/A.swift"), "// a\n").unwrap();
        std::fs::write(root.join("Sources/App/B.swift"), "// b\n").unwrap();
        std::fs::write(root.join("Sources/App/C.txt"), "ignore me\n").unwrap();
        std::fs::write(root.join(".build/release/D.swift"), "// d\n").unwrap();

        let profile = profile();
        let opts = LspIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from("Sources")],
            exclude_globs: vec!["**/B.swift".into()],
            lsp_command: None,
        };
        let files = discover_files(
            &profile,
            &opts.repo_root,
            &opts.code_roots,
            &opts.exclude_globs,
        )
        .unwrap();
        let names: Vec<String> = files.into_iter().map(|f| f.relative).collect();
        assert_eq!(names, vec!["Sources/App/A.swift"]);
    }

    #[test]
    fn binary_on_path_finds_or_misses_known_tool() {
        // `sh` always lives on PATH on macOS / Linux; missing-tool case
        // is the obviously made-up name.
        if cfg!(unix) {
            assert!(binary_on_path("sh"), "sh should be on PATH");
        }
        assert!(
            !binary_on_path("groundgraph_nonexistent_tool_12345"),
            "made-up binary should not resolve"
        );
    }

    /// Regression: a flapping LSP (e.g. sourcekit-lsp timing out on
    /// every symbol of a large Swift repo) used to push one per-symbol
    /// failure each into `skip_reason`, joined by `；`, with no cap —
    /// ballooning the field to hundreds of KB and flooding stdout / the
    /// MCP payload. `push_partial_warning` must keep the field bounded
    /// and fold the overflow into an accurate count.
    #[test]
    fn push_partial_warning_caps_runaway_failures() {
        let mut stats = LspIndexStats::default();
        for i in 0..200 {
            push_partial_warning(
                &mut stats,
                &format!("swift LSP callHierarchy(f{i}.swift:1) 失败：timed out"),
            );
        }
        assert!(
            stats.skip_reason.len() < 2000,
            "skip_reason ballooned to {} bytes",
            stats.skip_reason.len()
        );
        assert!(
            stats.skip_reason.contains("f0.swift"),
            "first detail dropped: {}",
            stats.skip_reason
        );
        assert!(
            stats.skip_reason.contains("192 条 LSP 警告已折叠"),
            "expected accurate fold count, got: {}",
            stats.skip_reason
        );
    }

    #[test]
    fn run_profile_skips_when_binary_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = profile();
        p.default_command = "groundgraph_nonexistent_tool_12345";
        let opts = LspIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("Sources")],
            exclude_globs: Vec::new(),
            lsp_command: None,
        };
        let outcome = run_profile(&p, &opts).unwrap();
        match outcome {
            LspIndexOutcome::Skipped { reason, language } => {
                assert_eq!(language, "swift");
                assert!(reason.contains("PATH"), "expected PATH hint: {reason}");
            }
            LspIndexOutcome::Indexed(_) => panic!("expected skip"),
        }
    }

    /// Regression for P12 复核 [P1]: a runtime LSP failure (binary
    /// resolves on PATH but exits without speaking LSP) must surface as
    /// [`LspIndexOutcome::Skipped`], never an `Err`. Otherwise
    /// `groundgraph index` of a workspace with `swift.enabled = true` will
    /// abort whenever sourcekit-lsp can't access its cache, etc.
    #[test]
    #[cfg(unix)]
    fn run_profile_downgrades_runtime_lsp_failure_to_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("Sources/App")).unwrap();
        std::fs::write(tmp.path().join("Sources/App/A.swift"), "// a\n").unwrap();

        let p = profile();
        let opts = LspIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("Sources")],
            exclude_globs: Vec::new(),
            // `true(1)` is on PATH everywhere on Unix and exits 0 without
            // writing any LSP frames — perfectly impersonates a misbehaving
            // language server.
            lsp_command: Some("true".into()),
        };

        let outcome = run_profile(&p, &opts).expect("runtime LSP failures must not propagate Err");
        match outcome {
            LspIndexOutcome::Skipped { reason, language } => {
                assert_eq!(language, "swift");
                assert!(
                    reason.contains("LSP")
                        || reason.contains("initialize")
                        || reason.contains("closed"),
                    "expected LSP-related skip reason, got: {reason}"
                );
            }
            LspIndexOutcome::Indexed(_) => panic!("expected runtime failure to be skipped"),
        }
    }
}
