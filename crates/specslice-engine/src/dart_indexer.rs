//! Dart indexing orchestrator (P23.6 — consolidated on tree-sitter).
//!
//! Two tiers feed one [`LanguageIndexBatch`]:
//! - **Tier-2 (structure, authoritative):** [`build_dart_structure`] runs the
//!   generic tree-sitter driver ([`crate::dart_treesitter`]) over every
//!   `.dart` file and produces the file / class / method / function /
//!   constructor / test nodes, `contains` hierarchy, resolved `imports`
//!   edges and symbol ranges — all under the legacy `dart_*::` id scheme.
//! - **Tier-3 (semantics, overlay):** the Dart analyzer sidecar
//!   ([`crate::dart_sidecar`]) — or, when no Dart SDK is present, the
//!   heuristic [`specslice_lang_dart::index_dart_paths`] fallback — only
//!   contributes `Calls` / `References` / framework edges, the synthetic
//!   `route` / `storage` nodes, and the framework-semantic `DartProvider`
//!   symbols. Its *plain* structural output is discarded in favour of the
//!   tree-sitter tier; [`backfill_referenced_symbols`] re-homes any symbol an
//!   overlay edge references but the structural pass did not emit, so no
//!   semantic edge dangles.
//!
//! Because both tiers share the legacy id scheme, the overlay binds to the
//! tree-sitter structure with zero translation and the pixcraft golden stays
//! byte-stable.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use specslice_core::artifact_id::file_id;
use specslice_core::language_batch::{FileArtifact, ImportEdge, LanguageIndexBatch};
use specslice_core::{EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use specslice_lang_dart::index_dart_paths;
use specslice_store::Store;

use crate::dart_sidecar::{self, SidecarOutcome};
use crate::dart_treesitter::{dart_extract_structure, dart_resolve_import, DART_SPEC};
use crate::treesitter::discover_relative_paths;

pub const DART_INDEXER_NAME: &str = "dart_lightweight";

/// Resolver tag stored on `DartIndexResult.resolver_used`. `dart_analyzer`
/// means the P7 sidecar produced this batch; `dart_lightweight` means we
/// fell back to the heuristic adapter.
pub const RESOLVER_DART_ANALYZER: &str = "dart_analyzer";
pub const RESOLVER_DART_LIGHTWEIGHT: &str = "dart_lightweight";

#[derive(Debug, Clone, Default)]
pub struct DartIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    /// Optional `glob`-style patterns to filter out paths that are technically
    /// inside a `code_roots` entry but should be skipped (generated files,
    /// build output, etc.). Patterns use the simple matcher in
    /// [`crate::dart_indexer::path_matches_glob`] — only `**`, `*`, `?`, `.`
    /// and `/` are honoured.
    pub exclude_globs: Vec<String>,
    /// P23.7 — when `true`, skip the Dart analyzer (Tier-3 semantic) overlay
    /// and index Dart with the tree-sitter structure + the heuristic
    /// lightweight reference scanner only. Maps from `enrichment.analyzer:
    /// false`. Defaults to `false` (analyzer enabled), preserving historical
    /// behaviour; the `SPECSLICE_DART_ANALYZER=0` env override is still
    /// honoured independently.
    pub disable_analyzer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DartIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub declared_implementations: usize,
    pub declared_verifications: usize,
    /// Which resolver produced the batch — `dart_analyzer` when the
    /// P7 sidecar ran successfully, `dart_lightweight` when we fell back
    /// to the heuristic adapter. Empty string is preserved for backward
    /// compatibility with stored results from before P7.
    #[serde(default)]
    pub resolver_used: String,
    /// Optional explanation if the sidecar was attempted but skipped
    /// (env disabled, Dart SDK missing, JSON malformed, etc.). Empty
    /// when the sidecar was used or when it wasn't attempted at all.
    #[serde(default)]
    pub sidecar_skip_reason: String,
}

pub fn index_dart(store: &mut Store, options: &DartIndexOptions) -> Result<DartIndexResult> {
    // P23.6 — tree-sitter is the sole *structural* backend for Dart. The
    // analyzer sidecar (or, without a Dart SDK, the heuristic lightweight
    // reference scanner) is demoted to a Tier-3 *semantic* overlay that only
    // contributes `Calls` / `References` / framework edges and the synthetic
    // `route` / `storage` / provider nodes. Both tiers share the legacy
    // `dart_*::` id scheme, so the overlay binds to the tree-sitter structure
    // with zero translation.

    // ---- Tier-2: structural pass (tree-sitter) -----------------------------
    let mut batch = build_dart_structure(options)?;

    // ---- Tier-3: semantic overlay (analyzer or lightweight) ---------------
    // Opt out of the sidecar with `enrichment.analyzer: false`
    // (`options.disable_analyzer`) or `SPECSLICE_DART_ANALYZER=0`; it also
    // self-skips when no `dart` binary / sidecar source is available.
    let outcome = if options.disable_analyzer {
        SidecarOutcome::Skipped {
            reason: "dart analyzer disabled by config (enrichment.analyzer=false)".to_string(),
        }
    } else {
        dart_sidecar::try_run(
            &options.repo_root,
            &options.code_roots,
            &options.exclude_globs,
        )
    };
    let (mut overlay, resolver_used, skip_reason) = match outcome {
        SidecarOutcome::Used(b) => (b, RESOLVER_DART_ANALYZER.to_string(), String::new()),
        SidecarOutcome::Skipped { reason } => (
            index_dart_paths(&options.repo_root, &options.code_roots)
                .context("scanning Dart sources")?,
            RESOLVER_DART_LIGHTWEIGHT.to_string(),
            reason,
        ),
    };

    // Carry only the *semantic* facts forward — the overlay's own *plain*
    // structural symbols / files / tests / imports / ranges are discarded in
    // favour of the tree-sitter ones.
    batch.synthetic_nodes = std::mem::take(&mut overlay.synthetic_nodes);
    batch.references = std::mem::take(&mut overlay.references);
    batch.diagnostics = std::mem::take(&mut overlay.diagnostics);

    // Riverpod providers are a *framework-semantic* concept the analyzer
    // tier owns (the same way it owns routes / storage); tree-sitter only
    // produces plain structure. Carry `DartProvider` symbols across so
    // `reads_provider` edges and candidate evidence keep an anchor.
    for sym in &overlay.symbols {
        if sym.kind == NodeKind::DartProvider {
            batch.symbol_ranges.push(specslice_core::SymbolRange {
                file_path: sym.path.clone(),
                symbol_id: sym.id.clone(),
                start_line: sym.start_line,
                end_line: sym.end_line,
                symbol_kind: sym.kind,
                qualified_name: sym.qualified_name.clone(),
                parent_symbol_id: sym.parent_symbol_id.clone(),
            });
            batch.symbols.push(sym.clone());
        }
    }

    // Repair tree-sitter structural misparses (function-typed static
    // fields degrade a class's static methods into phantom top-level
    // `dart_fn` nodes) using the overlay's classification as ground truth,
    // *before* backfill so the cleaned id set is consistent.
    reconcile_misparsed_callables(&mut batch, &overlay.symbols);

    // Safety net: if the overlay references a structural symbol the
    // tree-sitter pass did not emit (a construct it parses more precisely),
    // re-home that symbol from the overlay so no semantic edge dangles.
    backfill_referenced_symbols(&mut batch, &overlay.symbols);

    if !options.exclude_globs.is_empty() {
        let exclude = options.exclude_globs.clone();
        let drop_file = |path: &str| exclude.iter().any(|g| path_matches_glob(g, path));
        batch.files.retain(|f| !drop_file(&f.path));
        batch.symbols.retain(|s| !drop_file(&s.path));
        batch.tests.retain(|t| !drop_file(&t.path));
        batch.symbol_ranges.retain(|r| !drop_file(&r.file_path));
    }

    let mut result = ingest(store, &batch, &resolver_used)?;
    result.resolver_used = resolver_used;
    result.sidecar_skip_reason = skip_reason;
    Ok(result)
}

/// Tier-2 structural pass: discover every `.dart` file under the configured
/// roots, run the generic tree-sitter driver over each, and lower the result
/// into a [`LanguageIndexBatch`] addressed with the legacy `dart_*::` id
/// scheme. Imports are resolved to repo-relative file ids against the full
/// discovered file set.
fn build_dart_structure(options: &DartIndexOptions) -> Result<LanguageIndexBatch> {
    let mut batch = LanguageIndexBatch {
        language: "dart".into(),
        ..Default::default()
    };
    let files = discover_relative_paths(
        &options.repo_root,
        &options.code_roots,
        &[],
        DART_SPEC.extensions,
        DART_SPEC.skip_dirs,
    )
    .context("discovering Dart sources")?;
    let all_files = files.clone();

    for rel in &files {
        let abs = options.repo_root.join(rel);
        let Ok(source) = std::fs::read_to_string(&abs) else {
            continue; // unreadable / non-UTF-8: skip, never abort.
        };
        let hash = format!("{:x}", sha2::Sha256::digest(source.as_bytes()));
        batch.files.push(FileArtifact {
            id: file_id(rel),
            path: rel.clone(),
            language: "dart".into(),
            content_hash: hash,
        });

        let structure = dart_extract_structure(rel, &source);
        batch.symbols.extend(structure.symbols);
        batch.tests.extend(structure.tests);
        batch.symbol_ranges.extend(structure.ranges);
        for raw in &structure.raw_imports {
            if let Some(target) = dart_resolve_import(raw, rel, &all_files, &[]) {
                batch.imports.push(ImportEdge {
                    from_file: file_id(rel),
                    to_path: target,
                });
            }
        }
    }
    Ok(batch)
}

/// Re-home structural symbols referenced by overlay edges but missing from
/// the tree-sitter structure, so the Tier-3 semantic edges never dangle.
/// Only `dart_class/method/fn/ctor` endpoints qualify; synthetic
/// (`route`/`storage`/provider) and test endpoints are left alone.
fn backfill_referenced_symbols(
    batch: &mut LanguageIndexBatch,
    overlay_symbols: &[specslice_core::language_batch::SymbolArtifact],
) {
    let mut present: BTreeSet<String> = batch
        .symbols
        .iter()
        .map(|s| s.id.to_string())
        .chain(batch.tests.iter().map(|t| t.id.to_string()))
        .collect();

    for reference in &batch.references {
        for endpoint in [&reference.from_symbol_id, &reference.to_symbol_id] {
            let id = endpoint.to_string();
            if present.contains(&id) || !is_structural_dart_id(&id) {
                continue;
            }
            if let Some(sym) = overlay_symbols.iter().find(|s| s.id == *endpoint) {
                // Attach to the file (not a possibly-missing parent) so the
                // gap-fill never introduces a dangling `contains` edge.
                let mut sym = sym.clone();
                sym.parent_symbol_id = None;
                batch.symbol_ranges.push(specslice_core::SymbolRange {
                    file_path: sym.path.clone(),
                    symbol_id: sym.id.clone(),
                    start_line: sym.start_line,
                    end_line: sym.end_line,
                    symbol_kind: sym.kind,
                    qualified_name: sym.qualified_name.clone(),
                    parent_symbol_id: None,
                });
                present.insert(id);
                batch.symbols.push(sym);
            }
        }
    }
}

fn is_structural_dart_id(id: &str) -> bool {
    id.starts_with("dart_class::")
        || id.starts_with("dart_method::")
        || id.starts_with("dart_fn::")
        || id.starts_with("dart_ctor::")
}

/// Repair tree-sitter structural misparses using the analyzer overlay as
/// ground truth. The vendored Dart grammar (lukepighetti) occasionally
/// fails to recognise a class body that contains function-typed static
/// fields (`static T Function(...) f = _impl;`): the class node is dropped
/// and every static method *after* the offending field degrades into a
/// bogus top-level `dart_fn::<file>#<name>`. Those phantoms have no
/// incoming edges — the real edges bind to the analyzer's correct
/// `dart_method::<file>#<Class>.<name>` id — so they surface as
/// high-confidence dead-code false positives (dogfood regression on
/// hama's `ExportService`).
///
/// When the analyzer (or lightweight) overlay is present and reclassifies
/// such a `dart_fn` as a method / constructor, drop the phantom and
/// substitute the overlay's node, re-homing its parent class when the
/// structural pass missed that too. We only ever drop a `dart_fn` the
/// overlay does **not** also emit as a top-level function, so a file that
/// legitimately has both a top-level `foo()` and a `Class.foo()` keeps
/// both.
fn reconcile_misparsed_callables(
    batch: &mut LanguageIndexBatch,
    overlay_symbols: &[specslice_core::language_batch::SymbolArtifact],
) {
    if overlay_symbols.is_empty() {
        return;
    }

    // The overlay's authoritative set of *top-level function* ids. A
    // tree-sitter `dart_fn` whose id is in here is a genuine top-level
    // function and must never be reclassified.
    let analyzer_fn_ids: BTreeSet<String> = overlay_symbols
        .iter()
        .filter(|s| s.kind == NodeKind::DartFunction)
        .map(|s| s.id.to_string())
        .collect();

    let present_ids: BTreeSet<String> = batch.symbols.iter().map(|s| s.id.to_string()).collect();

    let mut phantom_ids: BTreeSet<String> = BTreeSet::new();
    let mut substitutes: Vec<specslice_core::language_batch::SymbolArtifact> = Vec::new();
    let mut substitute_ids: BTreeSet<String> = BTreeSet::new();

    for s in &batch.symbols {
        if s.kind != NodeKind::DartFunction {
            continue;
        }
        if analyzer_fn_ids.contains(&s.id.to_string()) {
            continue; // overlay agrees: real top-level function.
        }
        // Does the overlay reclassify this (path, name) as a method/ctor?
        let replacements: Vec<&specslice_core::language_batch::SymbolArtifact> = overlay_symbols
            .iter()
            .filter(|a| {
                matches!(a.kind, NodeKind::DartMethod | NodeKind::DartConstructor)
                    && a.path == s.path
                    && a.name == s.name
            })
            .collect();
        if replacements.is_empty() {
            continue;
        }
        phantom_ids.insert(s.id.to_string());
        for a in replacements {
            let aid = a.id.to_string();
            if !present_ids.contains(&aid) && substitute_ids.insert(aid) {
                substitutes.push((*a).clone());
            }
        }
    }

    if phantom_ids.is_empty() {
        return;
    }

    // Re-home any parent class the structural pass missed (the misparse
    // usually drops the enclosing class node entirely).
    let mut parent_adds: Vec<specslice_core::language_batch::SymbolArtifact> = Vec::new();
    let mut parent_ids: BTreeSet<String> = BTreeSet::new();
    for sub in &substitutes {
        let Some(parent) = &sub.parent_symbol_id else {
            continue;
        };
        let pid = parent.to_string();
        if present_ids.contains(&pid) || substitute_ids.contains(&pid) || parent_ids.contains(&pid)
        {
            continue;
        }
        if let Some(cls) = overlay_symbols
            .iter()
            .find(|a| a.kind == NodeKind::DartClass && a.id.to_string() == pid)
        {
            parent_ids.insert(pid);
            parent_adds.push(cls.clone());
        }
    }

    // Drop the phantoms (and their ranges).
    batch
        .symbols
        .retain(|s| !phantom_ids.contains(&s.id.to_string()));
    batch
        .symbol_ranges
        .retain(|r| !phantom_ids.contains(&r.symbol_id.to_string()));

    // Add re-homed parent classes first so the substituted methods can
    // keep their `contains` parent without dangling.
    let mut now_present: BTreeSet<String> =
        batch.symbols.iter().map(|s| s.id.to_string()).collect();
    for cls in parent_adds {
        let cid = cls.id.to_string();
        if now_present.insert(cid) {
            batch.symbol_ranges.push(specslice_core::SymbolRange {
                file_path: cls.path.clone(),
                symbol_id: cls.id.clone(),
                start_line: cls.start_line,
                end_line: cls.end_line,
                symbol_kind: cls.kind,
                qualified_name: cls.qualified_name.clone(),
                parent_symbol_id: cls.parent_symbol_id.clone(),
            });
            batch.symbols.push(cls);
        }
    }

    for mut sub in substitutes {
        // Only keep the parent link when the parent node is actually
        // present; otherwise parent under the file (avoids a dangling
        // `contains` edge), mirroring `backfill_referenced_symbols`.
        if let Some(parent) = &sub.parent_symbol_id {
            if !now_present.contains(&parent.to_string()) {
                sub.parent_symbol_id = None;
            }
        }
        batch.symbol_ranges.push(specslice_core::SymbolRange {
            file_path: sub.path.clone(),
            symbol_id: sub.id.clone(),
            start_line: sub.start_line,
            end_line: sub.end_line,
            symbol_kind: sub.kind,
            qualified_name: sub.qualified_name.clone(),
            parent_symbol_id: sub.parent_symbol_id.clone(),
        });
        now_present.insert(sub.id.to_string());
        batch.symbols.push(sub);
    }
}

/// Build the `evidence_json` payload stored on each Dart `calls` /
/// `references` edge. The shape is deliberately tiny so the engine and
/// the analyzer sidecar can both populate / parse it without a
/// dedicated schema crate.
///
/// ```json
/// { "line": 42, "snippet": "notifier.applyPurchase(p);", "resolver": "dart_lightweight" }
/// ```
fn build_reference_evidence_json(line: u32, snippet: &str, resolver: &str) -> String {
    use serde_json::{Map, Value};
    let mut obj: Map<String, Value> = Map::new();
    if line > 0 {
        obj.insert("line".into(), Value::from(line));
    }
    if !snippet.is_empty() {
        obj.insert("snippet".into(), Value::from(snippet.to_string()));
    }
    let resolver = if resolver.is_empty() {
        "dart_lightweight"
    } else {
        resolver
    };
    obj.insert("resolver".into(), Value::from(resolver.to_string()));
    Value::Object(obj).to_string()
}

/// Minimal glob matcher. Recognises `**` (cross-directory wildcard) and `*`
/// (within a single segment). Implemented locally so we do not have to pull
/// in a heavier dependency for what is essentially `path.ends_with(".g.dart")`.
fn path_matches_glob(pattern: &str, path: &str) -> bool {
    let pat = pattern.replace('\\', "/");
    let p = path.replace('\\', "/");
    let mut pi = pat.as_bytes();
    let si = p.as_bytes();

    // Drop leading `./`.
    if pi.starts_with(b"./") {
        pi = &pi[2..];
    }

    if let Some(rest) = pi.strip_prefix(b"**/") {
        let suffix = std::str::from_utf8(rest).unwrap_or("");
        return path_contains_pattern_segment(suffix, std::str::from_utf8(si).unwrap_or(""));
    }
    if pi == b"**" {
        return true;
    }

    // Direct prefix match (e.g. `.dart_tool`, `build`, `generated`).
    let pat_str = std::str::from_utf8(pi).unwrap_or("");
    let path_str = std::str::from_utf8(si).unwrap_or("");
    if !pat_str.contains('*') && !pat_str.contains('?') {
        return path_str == pat_str
            || path_str.starts_with(&format!("{pat_str}/"))
            || path_str.contains(&format!("/{pat_str}/"));
    }

    path_contains_pattern_segment(pat_str, path_str)
}

fn path_contains_pattern_segment(pattern: &str, path: &str) -> bool {
    // We only handle one wildcard tail of the form `*.ext` or literal suffix.
    if let Some(stripped) = pattern.strip_prefix("*.") {
        let needle = format!(".{stripped}");
        return path.ends_with(&needle);
    }
    if !pattern.contains('*') {
        return path == pattern || path.ends_with(&format!("/{pattern}"));
    }
    // Fallback: treat `*` as ".*" and do an end-anchored check.
    let regex_like = pattern.replace('.', "\\.").replace('*', ".*");
    let re = format!("(?:^|/){regex_like}$");
    matches_regex_like(&re, path)
}

fn matches_regex_like(escaped: &str, path: &str) -> bool {
    // Cheap regex emulation: split on `.*` and require each literal piece
    // in order, anchored as documented.
    let pieces: Vec<&str> = escaped.splitn(2, ".*").collect();
    match pieces.as_slice() {
        [single] => path.ends_with(*single),
        [head, tail] => {
            let head_lit = head.trim_start_matches("(?:^|/)").trim_start_matches('^');
            let tail_lit = tail.trim_end_matches('$').replace("\\.", ".");
            path.contains(head_lit) && path.ends_with(&tail_lit)
        }
        _ => false,
    }
}

fn ingest(
    store: &mut Store,
    batch: &LanguageIndexBatch,
    resolver_used: &str,
) -> Result<DartIndexResult> {
    let indexer_name = if resolver_used.is_empty() {
        DART_INDEXER_NAME
    } else {
        resolver_used
    };
    let mut result = DartIndexResult {
        files: batch.files.len(),
        ..Default::default()
    };

    for file in &batch.files {
        let mut node = Node::new(file.id.clone(), NodeKind::File);
        node.path = Some(file.path.clone());
        node.name = std::path::Path::new(&file.path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned());
        node.content_hash = Some(file.content_hash.clone());
        node.indexer = Some(indexer_name.into());
        store.upsert_node(&node)?;
    }

    for symbol in &batch.symbols {
        let mut node = Node::new(symbol.id.clone(), symbol.kind);
        node.path = Some(symbol.path.clone());
        node.name = Some(symbol.name.clone());
        node.stable_key = Some(symbol.qualified_name.clone());
        node.start_line = Some(symbol.start_line);
        node.end_line = Some(symbol.end_line);
        node.indexer = Some(indexer_name.into());
        store.upsert_node(&node)?;
        result.symbols += 1;

        let mut contains = if let Some(parent) = &symbol.parent_symbol_id {
            EdgeAssertion::fact(
                parent.clone(),
                symbol.id.clone(),
                EdgeKind::Contains,
                EdgeSource::LanguageAdapter,
            )
        } else {
            EdgeAssertion::fact(
                specslice_core::artifact_id::file_id(&symbol.path),
                symbol.id.clone(),
                EdgeKind::Contains,
                EdgeSource::LanguageAdapter,
            )
        };
        contains.indexer = Some(indexer_name.into());
        store.upsert_edge(&contains)?;
    }

    for test in &batch.tests {
        let mut node = Node::new(test.id.clone(), test.kind);
        node.path = Some(test.path.clone());
        node.name = Some(test.name.clone());
        node.start_line = Some(test.start_line);
        node.end_line = Some(test.end_line);
        node.indexer = Some(indexer_name.into());
        store.upsert_node(&node)?;
        if test.kind == NodeKind::TestCase {
            result.tests += 1;
        }
        let parent_id = test
            .parent_symbol_id
            .clone()
            .unwrap_or_else(|| specslice_core::artifact_id::file_id(&test.path));
        let mut contains = EdgeAssertion::fact(
            parent_id,
            test.id.clone(),
            EdgeKind::Contains,
            EdgeSource::LanguageAdapter,
        );
        contains.indexer = Some(indexer_name.into());
        store.upsert_edge(&contains)?;
    }

    for import in &batch.imports {
        let mut edge = EdgeAssertion::fact(
            import.from_file.clone(),
            specslice_core::artifact_id::file_id(&import.to_path),
            EdgeKind::Imports,
            EdgeSource::LanguageAdapter,
        );
        edge.indexer = Some(indexer_name.into());
        store.upsert_edge(&edge)?;
    }

    // P8 — emit synthetic targets (routes, storage buckets, top-level
    // providers we did not pick up as symbols) BEFORE the edges that point
    // at them so the foreign-key-ish contains chain stays consistent.
    for synth in &batch.synthetic_nodes {
        if !matches!(
            synth.kind,
            NodeKind::Route | NodeKind::Storage | NodeKind::DartProvider
        ) {
            // Adapter contract: only these synthetic kinds are allowed.
            continue;
        }
        let mut node = Node::new(synth.id.clone(), synth.kind);
        node.name = Some(synth.label.clone());
        node.indexer = Some(indexer_name.into());
        store.upsert_node(&node)?;
    }

    for reference in &batch.references {
        if !matches!(
            reference.kind,
            EdgeKind::References
                | EdgeKind::Calls
                | EdgeKind::ReadsProvider
                | EdgeKind::NavigatesTo
                | EdgeKind::PersistsTo
                | EdgeKind::SubscribesStream
        ) {
            // Defensive: adapter contract restricts these to a fixed set;
            // ignore any future kinds we do not yet understand instead of
            // corrupting the store.
            continue;
        }
        let mut edge = EdgeAssertion::fact(
            reference.from_symbol_id.clone(),
            reference.to_symbol_id.clone(),
            reference.kind,
            EdgeSource::LanguageAdapter,
        );
        edge.indexer = Some(indexer_name.into());
        // P6.3 — propagate evidence so the UI can show file:line + snippet
        // and the user can judge how trustworthy a heuristic edge is.
        if !reference.source_file.is_empty() {
            edge.source_file = Some(reference.source_file.clone());
        }
        if reference.line > 0 || !reference.snippet.is_empty() || !reference.resolver.is_empty() {
            edge.evidence_json = Some(build_reference_evidence_json(
                reference.line,
                &reference.snippet,
                &reference.resolver,
            ));
        }
        store.upsert_edge(&edge)?;
    }

    for range in &batch.symbol_ranges {
        store.upsert_symbol_range(range)?;
    }
    for symbol in &batch.symbols {
        store.upsert_symbol_range(&specslice_core::SymbolRange {
            file_path: symbol.path.clone(),
            symbol_id: symbol.id.clone(),
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            symbol_kind: symbol.kind,
            qualified_name: symbol.qualified_name.clone(),
            parent_symbol_id: symbol.parent_symbol_id.clone(),
        })?;
    }
    for test in &batch.tests {
        store.upsert_symbol_range(&specslice_core::SymbolRange {
            file_path: test.path.clone(),
            symbol_id: test.id.clone(),
            start_line: test.start_line,
            end_line: test.end_line,
            symbol_kind: test.kind,
            qualified_name: test.name.clone(),
            parent_symbol_id: test.parent_symbol_id.clone(),
        })?;
    }

    Ok(result)
}

/// Language-agnostic ingestion helper used by the P11 LSP adapters
/// (Swift / Go). Mirrors the structural subset of [`ingest`] without
/// the Dart-specific bookkeeping (analyzer-vs-lightweight resolver
/// tags, `declared_implementations` counts, etc.). The caller passes
/// its own indexer label so per-language facts can be cleared
/// independently before the next index run.
///
/// Specifically, this writes:
/// - `file` nodes (one per [`LanguageIndexBatch::files`] entry)
/// - symbol nodes plus `File → Symbol` (or `Parent → Symbol`)
///   `contains` edges
/// - test nodes plus their `contains` edges
/// - `imports` edges
/// - `symbol_ranges`
/// - `references` edges, but only when the adapter sets the same
///   restricted set of [`EdgeKind`] values the Dart adapter is
///   allowed to emit (so a buggy adapter cannot corrupt the store
///   with arbitrary edge kinds)
///
/// Synthetic nodes (routes / storage / Dart providers) are
/// **not** ingested here — they are Dart-only and live in the
/// per-language Dart path.
pub fn ingest_language_batch_minimal(
    store: &mut Store,
    batch: &LanguageIndexBatch,
    indexer_name: &str,
) -> Result<()> {
    for file in &batch.files {
        let mut node = Node::new(file.id.clone(), NodeKind::File);
        node.path = Some(file.path.clone());
        node.name = std::path::Path::new(&file.path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned());
        node.content_hash = Some(file.content_hash.clone());
        node.indexer = Some(indexer_name.into());
        store.upsert_node(&node)?;
    }

    for symbol in &batch.symbols {
        let mut node = Node::new(symbol.id.clone(), symbol.kind);
        node.path = Some(symbol.path.clone());
        node.name = Some(symbol.name.clone());
        node.stable_key = Some(symbol.qualified_name.clone());
        node.start_line = Some(symbol.start_line);
        node.end_line = Some(symbol.end_line);
        node.indexer = Some(indexer_name.into());
        if let Some(meta) = symbol.metadata_json.clone() {
            node.metadata_json = Some(meta);
        }
        store.upsert_node(&node)?;

        let mut contains = if let Some(parent) = &symbol.parent_symbol_id {
            EdgeAssertion::fact(
                parent.clone(),
                symbol.id.clone(),
                EdgeKind::Contains,
                EdgeSource::LanguageAdapter,
            )
        } else {
            EdgeAssertion::fact(
                specslice_core::artifact_id::file_id(&symbol.path),
                symbol.id.clone(),
                EdgeKind::Contains,
                EdgeSource::LanguageAdapter,
            )
        };
        contains.indexer = Some(indexer_name.into());
        store.upsert_edge(&contains)?;
    }

    for test in &batch.tests {
        let mut node = Node::new(test.id.clone(), test.kind);
        node.path = Some(test.path.clone());
        node.name = Some(test.name.clone());
        node.start_line = Some(test.start_line);
        node.end_line = Some(test.end_line);
        node.indexer = Some(indexer_name.into());
        store.upsert_node(&node)?;
        let parent_id = test
            .parent_symbol_id
            .clone()
            .unwrap_or_else(|| specslice_core::artifact_id::file_id(&test.path));
        let mut contains = EdgeAssertion::fact(
            parent_id,
            test.id.clone(),
            EdgeKind::Contains,
            EdgeSource::LanguageAdapter,
        );
        contains.indexer = Some(indexer_name.into());
        store.upsert_edge(&contains)?;
    }

    for import in &batch.imports {
        let mut edge = EdgeAssertion::fact(
            import.from_file.clone(),
            specslice_core::artifact_id::file_id(&import.to_path),
            EdgeKind::Imports,
            EdgeSource::LanguageAdapter,
        );
        edge.indexer = Some(indexer_name.into());
        store.upsert_edge(&edge)?;
    }

    for reference in &batch.references {
        if !matches!(
            reference.kind,
            EdgeKind::References
                | EdgeKind::Calls
                | EdgeKind::ReadsProvider
                | EdgeKind::NavigatesTo
                | EdgeKind::PersistsTo
                | EdgeKind::SubscribesStream
        ) {
            continue;
        }
        let mut edge = EdgeAssertion::fact(
            reference.from_symbol_id.clone(),
            reference.to_symbol_id.clone(),
            reference.kind,
            EdgeSource::LanguageAdapter,
        );
        edge.indexer = Some(indexer_name.into());
        if !reference.source_file.is_empty() {
            edge.source_file = Some(reference.source_file.clone());
        }
        if reference.line > 0 || !reference.snippet.is_empty() || !reference.resolver.is_empty() {
            edge.evidence_json = Some(build_reference_evidence_json(
                reference.line,
                &reference.snippet,
                &reference.resolver,
            ));
        }
        store.upsert_edge(&edge)?;
    }

    for range in &batch.symbol_ranges {
        store.upsert_symbol_range(range)?;
    }
    for symbol in &batch.symbols {
        store.upsert_symbol_range(&specslice_core::SymbolRange {
            file_path: symbol.path.clone(),
            symbol_id: symbol.id.clone(),
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            symbol_kind: symbol.kind,
            qualified_name: symbol.qualified_name.clone(),
            parent_symbol_id: symbol.parent_symbol_id.clone(),
        })?;
    }
    for test in &batch.tests {
        store.upsert_symbol_range(&specslice_core::SymbolRange {
            file_path: test.path.clone(),
            symbol_id: test.id.clone(),
            start_line: test.start_line,
            end_line: test.end_line,
            symbol_kind: test.kind,
            qualified_name: test.name.clone(),
            parent_symbol_id: test.parent_symbol_id.clone(),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod reconcile_tests {
    use super::*;
    use specslice_core::artifact_id::{dart_class_id, dart_function_id, dart_method_id};
    use specslice_core::language_batch::{SymbolArtifact, SymbolRange};
    use specslice_core::NodeKind;

    fn sym(
        id: specslice_core::ArtifactId,
        kind: NodeKind,
        path: &str,
        name: &str,
    ) -> SymbolArtifact {
        SymbolArtifact {
            id,
            kind,
            path: path.into(),
            name: name.into(),
            qualified_name: name.into(),
            start_line: 100,
            end_line: 120,
            parent_symbol_id: None,
            metadata_json: None,
        }
    }

    fn range_for(s: &SymbolArtifact) -> SymbolRange {
        SymbolRange {
            file_path: s.path.clone(),
            symbol_id: s.id.clone(),
            start_line: s.start_line,
            end_line: s.end_line,
            symbol_kind: s.kind,
            qualified_name: s.qualified_name.clone(),
            parent_symbol_id: s.parent_symbol_id.clone(),
        }
    }

    fn ids(batch: &LanguageIndexBatch) -> Vec<String> {
        batch.symbols.iter().map(|s| s.id.to_string()).collect()
    }

    #[test]
    fn drops_treesitter_phantom_fn_the_analyzer_calls_a_method() {
        // Tree-sitter misparsed a static method as a top-level function
        // (the function-typed-static-field grammar bug). The phantom has
        // no incoming edges and surfaces as a dead-code false positive.
        let phantom = sym(
            dart_function_id("lib/x.dart", "_shareXFiles"),
            NodeKind::DartFunction,
            "lib/x.dart",
            "_shareXFiles",
        );
        let mut batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        batch.symbol_ranges.push(range_for(&phantom));
        batch.symbols.push(phantom);

        // Analyzer ground truth: it is a method of ExportService.
        let cls = sym(
            dart_class_id("lib/x.dart", "ExportService"),
            NodeKind::DartClass,
            "lib/x.dart",
            "ExportService",
        );
        let mut method = sym(
            dart_method_id("lib/x.dart", "ExportService", "_shareXFiles"),
            NodeKind::DartMethod,
            "lib/x.dart",
            "_shareXFiles",
        );
        method.qualified_name = "ExportService._shareXFiles".into();
        method.parent_symbol_id = Some(cls.id.clone());
        let overlay = vec![cls.clone(), method.clone()];

        reconcile_misparsed_callables(&mut batch, &overlay);

        let got = ids(&batch);
        assert!(
            !got.contains(&"dart_fn::lib/x.dart#_shareXFiles".to_string()),
            "phantom dart_fn must be dropped: {got:?}"
        );
        assert!(
            got.contains(&"dart_method::lib/x.dart#ExportService._shareXFiles".to_string()),
            "analyzer method must be substituted: {got:?}"
        );
        assert!(
            got.contains(&"dart_class::lib/x.dart#ExportService".to_string()),
            "missing parent class must be backfilled: {got:?}"
        );
        // The phantom's range is gone; the method's range is present.
        assert!(
            !batch
                .symbol_ranges
                .iter()
                .any(|r| r.symbol_id.to_string() == "dart_fn::lib/x.dart#_shareXFiles"),
            "phantom range must be dropped"
        );
        assert!(batch.symbol_ranges.iter().any(|r| r.symbol_id == method.id));
    }

    #[test]
    fn drops_phantom_fn_for_extension_member_with_synthetic_parent() {
        // Dogfood (turing `game_screen_editor.dart`): tree-sitter cannot parse
        // a Dart-3 file (ERROR-node cascade), so an `extension _X on _State`
        // member degrades into a phantom top-level `dart_fn`. The analyzer
        // overlay carries the real `dart_method::<file>#_State.member` whose
        // parent is the *synthetic* `dart_extension::<file>#_State` id (the
        // `on` type may live in another part file). Reconcile must drop the
        // phantom, substitute the method, and — since the synthetic parent is
        // absent — re-home it under the file (parent → None) rather than
        // leave a dangling `contains` edge.
        let phantom = sym(
            dart_function_id("lib/game_screen_editor.dart", "_showSnackBar"),
            NodeKind::DartFunction,
            "lib/game_screen_editor.dart",
            "_showSnackBar",
        );
        let mut batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        batch.symbol_ranges.push(range_for(&phantom));
        batch.symbols.push(phantom);

        let mut method = sym(
            dart_method_id(
                "lib/game_screen_editor.dart",
                "_GameScreenState",
                "_showSnackBar",
            ),
            NodeKind::DartMethod,
            "lib/game_screen_editor.dart",
            "_showSnackBar",
        );
        method.qualified_name = "_GameScreenState._showSnackBar".into();
        method.parent_symbol_id = Some(specslice_core::ArtifactId::new(
            "dart_extension::lib/game_screen_editor.dart#_GameScreenState".to_string(),
        ));
        let overlay = vec![method.clone()];

        reconcile_misparsed_callables(&mut batch, &overlay);

        let got = ids(&batch);
        assert!(
            !got.contains(&"dart_fn::lib/game_screen_editor.dart#_showSnackBar".to_string()),
            "phantom dart_fn must be dropped: {got:?}"
        );
        assert!(
            got.contains(
                &"dart_method::lib/game_screen_editor.dart#_GameScreenState._showSnackBar"
                    .to_string()
            ),
            "extension method must be substituted: {got:?}"
        );
        // The synthetic extension parent is absent, so the method is re-homed
        // under the file (no dangling `contains`).
        let substituted = batch
            .symbols
            .iter()
            .find(|s| s.id == method.id)
            .expect("substituted method present");
        assert!(
            substituted.parent_symbol_id.is_none(),
            "synthetic dart_extension parent must be nulled: {:?}",
            substituted.parent_symbol_id
        );
        // No phantom `dart_extension::…` node is fabricated.
        assert!(
            !got.iter().any(|i| i.starts_with("dart_extension::")),
            "must not fabricate a dart_extension node: {got:?}"
        );
    }

    #[test]
    fn keeps_real_top_level_fn_even_when_a_same_named_method_exists() {
        // A genuine top-level function `helper` that the analyzer also
        // classifies as a top-level fn must survive, even if some class
        // happens to declare a method named `helper` too.
        let real_fn = sym(
            dart_function_id("lib/y.dart", "helper"),
            NodeKind::DartFunction,
            "lib/y.dart",
            "helper",
        );
        let mut batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        batch.symbol_ranges.push(range_for(&real_fn));
        batch.symbols.push(real_fn);

        let analyzer_fn = sym(
            dart_function_id("lib/y.dart", "helper"),
            NodeKind::DartFunction,
            "lib/y.dart",
            "helper",
        );
        let mut method = sym(
            dart_method_id("lib/y.dart", "C", "helper"),
            NodeKind::DartMethod,
            "lib/y.dart",
            "helper",
        );
        method.qualified_name = "C.helper".into();
        let overlay = vec![analyzer_fn, method];

        reconcile_misparsed_callables(&mut batch, &overlay);

        assert!(
            ids(&batch).contains(&"dart_fn::lib/y.dart#helper".to_string()),
            "real top-level fn confirmed by the analyzer must be kept"
        );
    }

    #[test]
    fn no_op_when_overlay_has_no_symbols() {
        // Lightweight/analyzer produced no structural symbols (or failed):
        // never touch the tree-sitter structure.
        let f = sym(
            dart_function_id("lib/z.dart", "top"),
            NodeKind::DartFunction,
            "lib/z.dart",
            "top",
        );
        let mut batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        batch.symbols.push(f);
        reconcile_misparsed_callables(&mut batch, &[]);
        assert_eq!(batch.symbols.len(), 1);
    }
}

#[cfg(test)]
mod glob_tests {
    use super::path_matches_glob;

    #[test]
    fn double_star_matches_anything() {
        assert!(path_matches_glob("**", "lib/a.dart"));
        assert!(path_matches_glob("**", "anything"));
    }

    #[test]
    fn double_star_slash_extension_matches_any_directory() {
        assert!(path_matches_glob("**/*.g.dart", "lib/gen/a.g.dart"));
        assert!(path_matches_glob(
            "**/*.freezed.dart",
            "build/x.freezed.dart"
        ));
        assert!(!path_matches_glob("**/*.g.dart", "lib/normal.dart"));
    }

    #[test]
    fn double_star_with_literal_tail_matches_basename() {
        assert!(path_matches_glob("**/Foo.dart", "lib/sub/Foo.dart"));
        assert!(!path_matches_glob("**/Foo.dart", "lib/Bar.dart"));
    }

    #[test]
    fn literal_directory_matches_top_or_nested_paths() {
        assert!(path_matches_glob("build", "build/x.dart"));
        assert!(path_matches_glob("build", "build"));
        assert!(path_matches_glob("build", "lib/build/x.dart"));
        assert!(!path_matches_glob("build", "lib/x.dart"));
    }

    #[test]
    fn dot_prefix_is_stripped_from_pattern() {
        assert!(path_matches_glob("./build", "build/x.dart"));
    }

    #[test]
    fn pattern_with_explicit_star_extension_falls_back_to_regex_like() {
        assert!(path_matches_glob("*.dart", "x.dart"));
        assert!(path_matches_glob("*.dart", "lib/x.dart"));
        assert!(!path_matches_glob("*.dart", "x.yaml"));
    }

    #[test]
    fn regex_like_fallback_handles_star_in_middle_of_pattern() {
        // `lib/*.dart` exercises the head/tail split in matches_regex_like
        // (head literal: `(?:^|/)lib/`, tail literal: `.dart$`).
        assert!(path_matches_glob("lib/*.dart", "lib/x.dart"));
        assert!(!path_matches_glob("lib/*.dart", "src/x.dart"));
        // Pattern with two `*`s is unsupported and must fall through to
        // false rather than panic.
        assert!(!path_matches_glob("a*b*c", "anything"));
    }
}

#[cfg(test)]
mod ingest_tests {
    use super::*;
    use specslice_core::{
        artifact_id::{dart_method_id, file_id},
        EdgeKind, FileArtifact, LanguageIndexBatch, ReferenceEdge,
    };

    #[test]
    fn ingest_skips_unknown_edge_kinds_defensively() {
        // The Dart adapter's contract restricts batch.references to
        // References / Calls. If a future kind sneaks in we must not
        // corrupt the store; the loop should just `continue`.
        let tmp = tempfile::TempDir::new().unwrap();
        let mut store = specslice_store::Store::open(tmp.path().join("g.db")).unwrap();
        store.migrate().unwrap();
        let mut batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        batch.files.push(FileArtifact {
            id: file_id("x.dart"),
            path: "x.dart".into(),
            language: "dart".into(),
            content_hash: "h".into(),
        });
        batch.references.push(ReferenceEdge {
            from_symbol_id: dart_method_id("x.dart", "A", "a"),
            to_symbol_id: dart_method_id("x.dart", "B", "b"),
            kind: EdgeKind::Contains, // <-- defensive: must be ignored
            source_file: "x.dart".into(),
            line: 1,
            snippet: "".into(),
            resolver: "dart_lightweight".into(),
        });

        let result = ingest(&mut store, &batch, RESOLVER_DART_LIGHTWEIGHT).unwrap();
        assert_eq!(result.files, 1);

        // No Calls/References edge should have been inserted because the
        // adapter sent a Contains kind that the ingest guard rejects.
        let calls = store.list_edges_by_kind(EdgeKind::Calls).unwrap();
        let refs = store.list_edges_by_kind(EdgeKind::References).unwrap();
        assert!(
            calls.is_empty() && refs.is_empty(),
            "defensive guard must drop unknown reference kinds"
        );
    }

    #[test]
    fn ingest_accepts_calls_and_references_normally() {
        use specslice_core::artifact_id::dart_class_id;
        let tmp = tempfile::TempDir::new().unwrap();
        let mut store = specslice_store::Store::open(tmp.path().join("g.db")).unwrap();
        store.migrate().unwrap();
        let mut batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        batch.files.push(FileArtifact {
            id: file_id("x.dart"),
            path: "x.dart".into(),
            language: "dart".into(),
            content_hash: "h".into(),
        });
        batch.references.push(ReferenceEdge {
            from_symbol_id: dart_method_id("x.dart", "A", "a"),
            to_symbol_id: dart_method_id("x.dart", "B", "b"),
            kind: EdgeKind::Calls,
            source_file: "x.dart".into(),
            line: 4,
            snippet: "b();".into(),
            resolver: "dart_lightweight".into(),
        });
        batch.references.push(ReferenceEdge {
            from_symbol_id: dart_method_id("x.dart", "A", "a"),
            to_symbol_id: dart_class_id("x.dart", "C"),
            kind: EdgeKind::References,
            source_file: "x.dart".into(),
            line: 5,
            snippet: "C().method();".into(),
            resolver: "dart_lightweight".into(),
        });
        ingest(&mut store, &batch, RESOLVER_DART_LIGHTWEIGHT).unwrap();
        assert_eq!(store.list_edges_by_kind(EdgeKind::Calls).unwrap().len(), 1);
        assert_eq!(
            store
                .list_edges_by_kind(EdgeKind::References)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn ingest_uses_analyzer_resolver_as_node_indexer_when_batch_came_from_sidecar() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut store = specslice_store::Store::open(tmp.path().join("g.db")).unwrap();
        store.migrate().unwrap();
        let mut batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        batch.files.push(FileArtifact {
            id: file_id("x.dart"),
            path: "x.dart".into(),
            language: "dart".into(),
            content_hash: "h".into(),
        });
        batch.references.push(ReferenceEdge {
            from_symbol_id: dart_method_id("x.dart", "A", "a"),
            to_symbol_id: dart_method_id("x.dart", "B", "b"),
            kind: EdgeKind::Calls,
            source_file: "x.dart".into(),
            line: 4,
            snippet: "b();".into(),
            resolver: RESOLVER_DART_ANALYZER.into(),
        });

        ingest(&mut store, &batch, RESOLVER_DART_ANALYZER).unwrap();

        let file = store
            .find_node(&file_id("x.dart"))
            .unwrap()
            .expect("file node should be indexed");
        assert_eq!(
            file.indexer.as_deref(),
            Some(RESOLVER_DART_ANALYZER),
            "graph node source should show the analyzer resolver, not the fallback parser"
        );
    }

    #[test]
    fn ingest_normalises_symbol_without_parent_to_file_contains() {
        // Symbol with `parent_symbol_id == None` should be parented under
        // the file via a synthesised contains edge.
        use specslice_core::{artifact_id::dart_function_id, SymbolArtifact};
        let tmp = tempfile::TempDir::new().unwrap();
        let mut store = specslice_store::Store::open(tmp.path().join("g.db")).unwrap();
        store.migrate().unwrap();
        let mut batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        batch.files.push(FileArtifact {
            id: file_id("x.dart"),
            path: "x.dart".into(),
            language: "dart".into(),
            content_hash: "h".into(),
        });
        let fn_id = dart_function_id("x.dart", "top");
        batch.symbols.push(SymbolArtifact {
            id: fn_id.clone(),
            kind: specslice_core::NodeKind::DartFunction,
            path: "x.dart".into(),
            name: "top".into(),
            qualified_name: "top".into(),
            start_line: 1,
            end_line: 5,
            parent_symbol_id: None,
            metadata_json: None,
        });
        ingest(&mut store, &batch, RESOLVER_DART_LIGHTWEIGHT).unwrap();
        let contains = store.list_edges_by_kind(EdgeKind::Contains).unwrap();
        assert!(
            contains
                .iter()
                .any(|e| e.from_id == file_id("x.dart") && e.to_id == fn_id),
            "synthesised file→symbol contains edge missing: {contains:?}"
        );
    }

    #[test]
    fn index_dart_drops_files_matching_exclude_glob() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("lib")).unwrap();
        std::fs::write(tmp.path().join("lib/keep.dart"), "class Keep {}\n").unwrap();
        std::fs::write(tmp.path().join("lib/skip.g.dart"), "class Skip {}\n").unwrap();
        let mut store = specslice_store::Store::open(tmp.path().join("g.db")).unwrap();
        store.migrate().unwrap();
        let result = index_dart(
            &mut store,
            &DartIndexOptions {
                repo_root: tmp.path().to_path_buf(),
                code_roots: vec!["lib".into()],
                exclude_globs: vec!["**/*.g.dart".into()],
                disable_analyzer: false,
            },
        )
        .unwrap();
        assert_eq!(result.symbols, 1, "*.g.dart files must be excluded");
    }

    // -----------------------------------------------------------------
    // P6.3 coverage for the evidence_json builder.
    // -----------------------------------------------------------------

    #[test]
    fn p63_build_reference_evidence_json_round_trips() {
        let raw = build_reference_evidence_json(42, "applyPurchase(p);", "dart_lightweight");
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["line"], 42);
        assert_eq!(parsed["snippet"], "applyPurchase(p);");
        assert_eq!(parsed["resolver"], "dart_lightweight");
    }

    #[test]
    fn p63_build_reference_evidence_json_omits_zero_line_and_empty_snippet() {
        let raw = build_reference_evidence_json(0, "", "dart_lightweight");
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(parsed.get("line").is_none());
        assert!(parsed.get("snippet").is_none());
        assert_eq!(parsed["resolver"], "dart_lightweight");
    }

    #[test]
    fn p63_build_reference_evidence_json_defaults_resolver_when_missing() {
        let raw = build_reference_evidence_json(1, "x", "");
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // Empty resolver gets replaced with the canonical default so the UI
        // never has to render `null` or an empty source label.
        assert_eq!(parsed["resolver"], "dart_lightweight");
    }

    #[test]
    fn p63_ingest_propagates_evidence_to_edge_assertion() {
        // End-to-end: ReferenceEdge with line + snippet flows into the
        // store via EdgeAssertion.evidence_json + EdgeAssertion.source_file.
        use specslice_core::artifact_id::dart_class_id;
        let tmp = tempfile::TempDir::new().unwrap();
        let mut store = specslice_store::Store::open(tmp.path().join("g.db")).unwrap();
        store.migrate().unwrap();
        let mut batch = LanguageIndexBatch {
            language: "dart".into(),
            ..Default::default()
        };
        batch.files.push(FileArtifact {
            id: file_id("x.dart"),
            path: "x.dart".into(),
            language: "dart".into(),
            content_hash: "h".into(),
        });
        batch.references.push(ReferenceEdge {
            from_symbol_id: dart_method_id("x.dart", "A", "a"),
            to_symbol_id: dart_class_id("x.dart", "C"),
            kind: EdgeKind::References,
            source_file: "lib/x.dart".into(),
            line: 17,
            snippet: "C.constant;".into(),
            resolver: "dart_lightweight".into(),
        });
        ingest(&mut store, &batch, RESOLVER_DART_LIGHTWEIGHT).unwrap();
        let edges = store.list_edges_by_kind(EdgeKind::References).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source_file.as_deref(), Some("lib/x.dart"));
        let ev = edges[0].evidence_json.as_deref().expect("evidence_json");
        let parsed: serde_json::Value = serde_json::from_str(ev).unwrap();
        assert_eq!(parsed["line"], 17);
        assert_eq!(parsed["snippet"], "C.constant;");
        assert_eq!(parsed["resolver"], "dart_lightweight");
    }
}
