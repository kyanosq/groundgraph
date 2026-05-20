//! Bridge between the Dart lightweight adapter and the SpecSlice store.
//!
//! MVP-2 scope (PRD §3.1 / implementation plan §MVP-2):
//! - Walk `lib/` and `test/` via [`specslice_lang_dart::index_dart_paths`].
//! - Ingest the [`LanguageIndexBatch`] into the store:
//!   - File / class / method / function / constructor / test-case nodes.
//!   - `File --contains--> Symbol` (Fact) edges and Class -> Method `contains` edges.
//!   - `File --imports--> File` (Fact) edges when the target file resolves locally.
//!   - Symbol ranges and parent-child hierarchy.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::language_batch::LanguageIndexBatch;
use specslice_core::{EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use specslice_lang_dart::index_dart_paths;
use specslice_store::Store;

use crate::dart_sidecar::{self, SidecarOutcome};

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
    // P7+P2: the Dart analyzer sidecar is now the default high-precision
    // path. It runs whenever a `dart` binary is on PATH and the
    // workspace ships `tool/specslice_dart_analyzer/`. Users without a
    // Dart SDK silently fall back to the lightweight heuristic adapter
    // (resolver_used = "dart_lightweight"). Opt out with
    // `SPECSLICE_DART_ANALYZER=0`.
    let (mut batch, resolver_used, skip_reason) = match dart_sidecar::try_run(
        &options.repo_root,
        &options.code_roots,
        &options.exclude_globs,
    ) {
        SidecarOutcome::Used(b) => (b, RESOLVER_DART_ANALYZER.to_string(), String::new()),
        SidecarOutcome::Skipped { reason } => (
            index_dart_paths(&options.repo_root, &options.code_roots)
                .context("scanning Dart sources")?,
            RESOLVER_DART_LIGHTWEIGHT.to_string(),
            reason,
        ),
    };
    if !options.exclude_globs.is_empty() {
        let exclude = options.exclude_globs.clone();
        let drop_file = |path: &str| exclude.iter().any(|g| path_matches_glob(g, path));
        batch.files.retain(|f| !drop_file(&f.path));
        batch.symbols.retain(|s| !drop_file(&s.path));
        batch.tests.retain(|t| !drop_file(&t.path));
        batch.imports.retain(|i| {
            // Imports are keyed by the *file id* they came from, not by path,
            // so we conservatively keep them all. They will become dangling
            // for excluded files but Impact already handles missing nodes.
            let _ = i;
            true
        });
        batch.symbol_ranges.retain(|r| !drop_file(&r.file_path));
    }
    let mut result = ingest(store, &batch, &resolver_used)?;
    result.resolver_used = resolver_used;
    result.sidecar_skip_reason = skip_reason;
    Ok(result)
}

/// Minimal glob matcher. Recognises `**` (cross-directory wildcard) and `*`
/// (within a single segment). Implemented locally so we do not have to pull
/// in a heavier dependency for what is essentially `path.ends_with(".g.dart")`.
/// Build the `evidence_json` payload stored on each Dart `calls` /
/// `references` edge. The shape is deliberately tiny so the engine and
/// the future analyzer sidecar can both populate / parse it without a
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
