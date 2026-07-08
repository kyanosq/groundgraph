//! Tier-3 SCIP ingestion overlay (ADR-0001 R1).
//!
//! SCIP (Sourcegraph Code Intelligence Protocol) indexers — `rust-analyzer
//! scip`, `scip-typescript`, `scip-go`, `scip-java`, `scip-python` — run a
//! language's *real* compiler frontend offline and emit a protobuf
//! `index.scip` of fully-resolved definitions and references. This module
//! ingests that file and *overlays* high-confidence `Calls` / `References`
//! edges onto the symbols the tree-sitter structural pass already produced —
//! the same "bind to the existing structure, never introduce a second id"
//! discipline the Dart analyzer sidecar uses (ADR-0001 §3.2; decision driver
//! D2 "single structural source").
//!
//! Binding is purely positional, so the SCIP `symbol` string is never
//! translated into our [`ArtifactId`](groundgraph_core::ArtifactId): every SCIP
//! occurrence carries a `range`, and we map that range's *line* to the
//! innermost [`SymbolRange`] that contains it (the symbol whose body the line
//! lives in). A *definition* occurrence therefore names the **defined** node;
//! every other occurrence names the **enclosing** node (the caller). The edge
//! is `enclosing --Calls/References--> defined`.
//!
//! Anything that does not land inside a known range — references into external
//! crates, generated files we did not index, `local …` document-scoped
//! symbols — is silently dropped. The overlay only ever *adds* edges between
//! nodes that already exist; it never creates a node and never panics on
//! malformed input (D1 non-invasive, D4 determinism).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use groundgraph_core::edge::EdgeKind;
use groundgraph_core::language_batch::{ReferenceEdge, SymbolRange};
use groundgraph_core::{EdgeAssertion, EdgeSource};
use groundgraph_store::Store;
use protobuf::Message;
use scip::types::{Index, Occurrence};

/// Resolver tag stamped on every overlay edge. The ingest path copies it into
/// the edge's `indexer` column, so SCIP edges are filterable and rank above
/// the medium-confidence heuristic `Calls`/`References` synthesised in-process.
pub const RESOLVER_SCIP: &str = "scip";

/// SCIP `SymbolRole` bitset value for a definition occurrence (scip.proto:
/// `Definition = 0x1`). Tested as a bit against `Occurrence.symbol_roles`
/// rather than via the generated enum so a future role addition cannot shift
/// the meaning out from under us.
const ROLE_DEFINITION: i32 = 0x1;

/// Parse a SCIP index from protobuf bytes (the contents of an `index.scip`).
pub fn parse_index(bytes: &[u8]) -> Result<Index> {
    Index::parse_from_bytes(bytes).context("parsing SCIP index protobuf")
}

/// Bind every cross-symbol SCIP occurrence to the tree-sitter symbol ranges
/// and return the `Calls` / `References` edges to overlay.
///
/// `ranges` must contain every [`SymbolRange`] for the files the index covers
/// (extra ranges for other files are harmless). Output edges carry
/// `resolver = "scip"`; the caller ingests them like any other
/// [`ReferenceEdge`].
///
/// One [`OverlayEdge`] is returned per distinct `(caller, callee, kind)` triple,
/// carrying **every** call-site line that produced it (#75).
pub struct OverlayEdge {
    /// The edge to ingest. `edge.line` mirrors the first (lowest) call site.
    pub edge: ReferenceEdge,
    /// Every distinct call-site line, ascending. SCIP can see the same
    /// `(caller, callee, kind)` at many lines (a loop, repeated branches); the
    /// old dedup kept only the first, so the stored evidence under-reported the
    /// call sites (#75). Keeping them all lets the trace/UI list each one.
    pub lines: Vec<u32>,
}

pub fn overlay_edges(index: &Index, ranges: &[SymbolRange]) -> Vec<OverlayEdge> {
    // Index our ranges by file so each occurrence only searches its own file.
    let mut by_file: HashMap<&str, Vec<&SymbolRange>> = HashMap::new();
    for r in ranges {
        by_file.entry(r.file_path.as_str()).or_default().push(r);
    }

    // Pass 1 — definitions: a definition occurrence's line falls inside the
    // defined symbol's own body, so the innermost containing range *is* that
    // symbol. This gives every global SCIP symbol a home node (cross-file).
    let mut def_of: HashMap<&str, &SymbolRange> = HashMap::new();
    for doc in &index.documents {
        let Some(file_ranges) = by_file.get(doc.relative_path.as_str()) else {
            continue;
        };
        for occ in &doc.occurrences {
            if !is_global(&occ.symbol) || occ.symbol_roles & ROLE_DEFINITION == 0 {
                continue;
            }
            let Some(line) = occ_line(occ) else { continue };
            if let Some(node) = innermost_containing(file_ranges, line) {
                def_of.insert(occ.symbol.as_str(), node);
            }
        }
    }

    // Pass 2 — uses: every non-definition occurrence is attributed to the
    // innermost range that encloses it (the caller) and linked to the symbol's
    // definition node. Self-edges, unresolved targets and references to
    // symbols defined outside the indexed files are dropped.
    // Preserve first-seen pair order for deterministic output while collecting
    // *all* call-site lines for each (caller, callee, kind) triple (#75).
    let mut order: Vec<(String, String, EdgeKind)> = Vec::new();
    let mut acc: HashMap<(String, String, EdgeKind), (ReferenceEdge, Vec<u32>)> = HashMap::new();
    for doc in &index.documents {
        let Some(file_ranges) = by_file.get(doc.relative_path.as_str()) else {
            continue;
        };
        for occ in &doc.occurrences {
            if !is_global(&occ.symbol) || occ.symbol_roles & ROLE_DEFINITION != 0 {
                continue;
            }
            let Some(&to) = def_of.get(occ.symbol.as_str()) else {
                continue;
            };
            let Some(line) = occ_line(occ) else { continue };
            let Some(from) = innermost_containing(file_ranges, line) else {
                continue;
            };
            if from.symbol_id == to.symbol_id {
                continue;
            }
            let kind = if to.symbol_kind.is_callable() {
                EdgeKind::Calls
            } else {
                EdgeKind::References
            };
            let key = (
                from.symbol_id.as_str().to_string(),
                to.symbol_id.as_str().to_string(),
                kind,
            );
            match acc.entry(key.clone()) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    order.push(key);
                    slot.insert((
                        ReferenceEdge {
                            from_symbol_id: from.symbol_id.clone(),
                            to_symbol_id: to.symbol_id.clone(),
                            kind,
                            source_file: doc.relative_path.clone(),
                            line,
                            snippet: String::new(),
                            resolver: RESOLVER_SCIP.to_string(),
                        },
                        vec![line],
                    ));
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    slot.get_mut().1.push(line);
                }
            }
        }
    }
    order
        .into_iter()
        .map(|key| {
            let (mut edge, mut lines) = acc.remove(&key).expect("key was just inserted");
            lines.sort_unstable();
            lines.dedup();
            // `edge.line` mirrors the first (lowest) call site for back-compat.
            edge.line = *lines.first().unwrap_or(&edge.line);
            OverlayEdge { edge, lines }
        })
        .collect()
}

/// A SCIP symbol we link on. Empty strings and document-scoped `local …`
/// symbols are skipped: locals are reused verbatim across files, so keying a
/// global map on them would alias unrelated nodes, and they never name a
/// cross-symbol call edge worth surfacing.
fn is_global(symbol: &str) -> bool {
    !symbol.is_empty() && !symbol.starts_with("local ")
}

/// The 1-based start line of an occurrence, or `None` for a malformed/empty
/// range. SCIP ranges are `[startLine, startCol, …]`, 0-based; our
/// [`SymbolRange`] lines are 1-based (`row + 1`), so we add one. A negative
/// line (never emitted by real indexers) fails the `try_from` and is dropped.
fn occ_line(occ: &Occurrence) -> Option<u32> {
    let start = *occ.range.first()?;
    u32::try_from(start).ok().map(|l| l.saturating_add(1))
}

/// The smallest range that contains `line` (the deepest enclosing symbol).
/// Ties on span prefer the one that starts later, i.e. the more deeply nested
/// declaration, which is what a method-inside-impl-inside-module wants.
fn innermost_containing<'a>(ranges: &[&'a SymbolRange], line: u32) -> Option<&'a SymbolRange> {
    ranges
        .iter()
        .copied()
        // `start_line > 0`: lines are 1-based here, so a range starting at 0 is
        // a degenerate sentinel (e.g. an external `DbTable` range). It would
        // otherwise "contain" any line and mis-attribute SCIP calls (#204).
        .filter(|r| r.start_line > 0 && r.start_line <= line && line <= r.end_line)
        .min_by(|a, b| {
            a.end_line
                .saturating_sub(a.start_line)
                .cmp(&b.end_line.saturating_sub(b.start_line))
                .then_with(|| b.start_line.cmp(&a.start_line))
        })
}

/// Stats from one [`ingest_scip_overlay`] run, surfaced in the index result.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScipOverlayResult {
    /// `.scip` files ingested under `.groundgraph/scip/`.
    pub scip_files: usize,
    /// SCIP documents seen across those files.
    pub documents: usize,
    /// `Calls` / `References` edges overlaid (post-dedup).
    pub edges: usize,
    /// Heuristic/LSP `Calls`/`References` edges suppressed because SCIP now
    /// authoritatively covers their source file (ADR-0001 §3.2, D2).
    pub suppressed: usize,
}

/// Ingest every `.groundgraph/scip/*.scip` under `repo_root` as a high-confidence
/// overlay onto the symbol ranges already in `store`.
///
/// This runs *after* the structural passes (so every symbol range exists) and
/// only ever adds `Calls`/`References` edges tagged `indexer = "scip"` between
/// nodes that already exist. It is a silent no-op when the directory is absent
/// or holds no `.scip` file, and idempotent across re-indexes: prior `scip`
/// edges are cleared first, and because it runs last a triple the heuristic
/// resolver also found is *upgraded* (re-tagged `scip`) rather than duplicated.
pub fn ingest_scip_overlay(store: &mut Store, repo_root: &Path) -> Result<ScipOverlayResult> {
    let scip_files =
        collect_scip_files(&crate::config::workspace_dir_for_repo(repo_root).join("scip"));
    let mut result = ScipOverlayResult::default();
    if scip_files.is_empty() {
        return Ok(result);
    }
    store
        .clear_indexer_outputs(RESOLVER_SCIP)
        .context("clearing previous SCIP overlay edges")?;
    // Files SCIP authoritatively analysed (≥1 occurrence). Heuristic/LSP
    // precision on these is suppressed after ingest so a covered file carries a
    // single precision source — SCIP. A broken indexer that writes a 0-document
    // index therefore suppresses nothing and the heuristic gap-fill stands.
    let mut covered_files: BTreeSet<String> = BTreeSet::new();
    for path in scip_files {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading SCIP index {}", path.display()))?;
        let mut index = parse_index(&bytes)
            .with_context(|| format!("parsing SCIP index {}", path.display()))?;
        // A SCIP index produced by running the indexer inside a sub-module roots
        // its document paths at `metadata.project_root` (e.g. `scip-go` run in
        // `asc-cli/`, whose go.mod is not at the repo root). Rebase those onto
        // repo-root-relative paths so they match the symbol ranges in the store;
        // a no-op when the index is already rooted at the repo.
        rebase_index_paths(&mut index, repo_root);

        // Load symbol ranges for exactly the files this index covers — no more,
        // so a huge multi-language repo only pays for the indexed slice.
        let mut seen_files = BTreeSet::new();
        let mut ranges = Vec::new();
        for doc in &index.documents {
            if !doc.occurrences.is_empty() {
                covered_files.insert(doc.relative_path.clone());
            }
            if seen_files.insert(doc.relative_path.clone()) {
                ranges.extend(
                    store
                        .list_symbol_ranges_for_file(&doc.relative_path)
                        .with_context(|| {
                            format!("loading symbol ranges for {}", doc.relative_path)
                        })?,
                );
            }
        }

        for OverlayEdge { edge: e, lines } in overlay_edges(&index, &ranges) {
            let mut edge = EdgeAssertion::fact(
                e.from_symbol_id.clone(),
                e.to_symbol_id.clone(),
                e.kind,
                EdgeSource::LanguageAdapter,
            );
            edge.indexer = Some(RESOLVER_SCIP.to_string());
            if !e.source_file.is_empty() {
                edge.source_file = Some(e.source_file.clone());
            }
            edge.evidence_json = Some(evidence_json(&lines, &e.resolver));
            store
                .upsert_edge(&edge)
                .context("upserting SCIP overlay edge")?;
            result.edges += 1;
        }
        result.scip_files += 1;
        result.documents += index.documents.len();
    }

    // SCIP-authoritative gap-fill: drop heuristic/LSP `Calls`/`References` on
    // every file SCIP covered, leaving SCIP's own edges as the lone precision
    // source there. Files SCIP never analysed keep their heuristic edges.
    let covered: Vec<String> = covered_files.into_iter().collect();
    result.suppressed = store
        .delete_precision_edges_for_files_except(&covered, RESOLVER_SCIP)
        .context("suppressing heuristic precision on SCIP-covered files")?;

    Ok(result)
}

/// Rebase every document path in `index` from `metadata.project_root`-relative
/// to `repo_root`-relative, so an index produced by running the indexer inside a
/// sub-module (its `project_root` is a subdir of the repo) still matches the
/// repo-root-relative symbol ranges in the store. A no-op when `project_root` is
/// the repo root itself, absent, or not under `repo_root`.
fn rebase_index_paths(index: &mut Index, repo_root: &Path) {
    let Some(prefix) = project_root_prefix(index, repo_root) else {
        return;
    };
    if prefix.is_empty() {
        return;
    }
    for doc in &mut index.documents {
        doc.relative_path = format!("{prefix}/{}", doc.relative_path);
    }
}

/// `metadata.project_root` (a `file://` URI) expressed `repo_root`-relative with
/// forward slashes. `Some("")` when they are the same directory (no rebasing
/// needed); `None` when `project_root` is missing or not under `repo_root`. Tries
/// the literal paths first, then a canonicalized pair, so it is robust both to
/// unit-test temp dirs (whose sub-module may not exist on disk) and to real
/// macOS `/var`→`/private/var` symlinked roots.
fn project_root_prefix(index: &Index, repo_root: &Path) -> Option<String> {
    let meta = index.metadata.as_ref()?;
    let uri = meta.project_root.trim();
    if uri.is_empty() {
        return None;
    }
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let proj = Path::new(raw.trim_end_matches('/'));
    if let Ok(rel) = proj.strip_prefix(repo_root) {
        return Some(rel.to_string_lossy().replace('\\', "/"));
    }
    if let (Ok(pc), Ok(rc)) = (proj.canonicalize(), repo_root.canonicalize()) {
        if let Ok(rel) = pc.strip_prefix(&rc) {
            return Some(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    None
}

/// Sorted list of `*.scip` files directly under `dir` (non-recursive). Missing
/// directory → empty list (the overlay is opt-in by the file's presence).
fn collect_scip_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "scip"))
        .collect();
    files.sort();
    files
}

/// Minimal evidence blob, same shape the heuristic/analyzer edges carry so the
/// trace/UI can render `file:line` provenance and the resolver name.
/// Serialise the call-site evidence for one overlay edge. `line` (the first,
/// lowest call site) is retained for back-compat with readers that predate #75;
/// `lines` carries **every** distinct call site SCIP observed for this edge so
/// the stored evidence is no longer lossy when a callee is hit at many lines.
fn evidence_json(lines: &[u32], resolver: &str) -> String {
    let first = lines.first().copied().unwrap_or(0);
    serde_json::json!({
        "line": first,
        "lines": lines,
        "snippet": "",
        "resolver": resolver,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::artifact_id::ArtifactId;
    use groundgraph_core::edge::EdgeKind;
    use groundgraph_core::NodeKind;
    use scip::types::{Document, Metadata, Occurrence};

    fn range(file: &str, id: &str, start: u32, end: u32, kind: NodeKind) -> SymbolRange {
        SymbolRange {
            file_path: file.into(),
            symbol_id: ArtifactId::new(id),
            start_line: start,
            end_line: end,
            symbol_kind: kind,
            qualified_name: id.into(),
            parent_symbol_id: None,
        }
    }

    fn occ(line0: i32, symbol: &str, def: bool) -> Occurrence {
        Occurrence {
            range: vec![line0, 0, 10],
            symbol: symbol.into(),
            symbol_roles: if def { ROLE_DEFINITION } else { 0 },
            ..Default::default()
        }
    }

    /// `Foo#bar()` is *defined* in `foo.rs` and *used* from a function body in
    /// `caller.rs`. The overlay must emit exactly one cross-file `Calls` edge
    /// (`caller -> bar`) tagged `scip` — the precise cross-file resolution the
    /// in-process heuristic resolver cannot do.
    #[test]
    fn cross_file_reference_becomes_calls_edge() {
        const BAR: &str = "rust-analyzer cargo demo 0.1.0 demo/Foo#bar().";
        let index = Index {
            documents: vec![
                Document {
                    relative_path: "src/foo.rs".into(),
                    // definition at 0-based line 6 => 1-based line 7 (inside bar)
                    occurrences: vec![occ(6, BAR, true)],
                    ..Default::default()
                },
                Document {
                    relative_path: "src/caller.rs".into(),
                    // use at 0-based line 14 => 1-based line 15 (inside caller)
                    occurrences: vec![occ(14, BAR, false)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let ranges = vec![
            range("src/foo.rs", "S:bar", 7, 9, NodeKind::RustMethod),
            range("src/caller.rs", "S:caller", 12, 20, NodeKind::RustFunction),
        ];

        let edges = overlay_edges(&index, &ranges);

        assert_eq!(edges.len(), 1, "exactly one cross-file edge expected");
        let e = &edges[0].edge;
        assert_eq!(e.from_symbol_id.as_str(), "S:caller");
        assert_eq!(e.to_symbol_id.as_str(), "S:bar");
        assert_eq!(e.kind, EdgeKind::Calls);
        assert_eq!(e.resolver, RESOLVER_SCIP);
        assert_eq!(e.source_file, "src/caller.rs");
        assert_eq!(e.line, 15);
        assert_eq!(edges[0].lines, vec![15], "single call site");
    }

    /// A reference whose definition lives outside the indexed files (an
    /// external crate / std), a self-recursive call, and a document-scoped
    /// `local …` symbol must all be dropped — the overlay only links nodes
    /// that already exist and never emits a self-edge.
    #[test]
    fn external_self_and_local_occurrences_are_dropped() {
        const EXTERNAL: &str = "rust-analyzer cargo std 1.0.0 std/println!().";
        const REC: &str = "rust-analyzer cargo demo 0.1.0 demo/loop_fn().";
        let index = Index {
            documents: vec![Document {
                relative_path: "src/m.rs".into(),
                occurrences: vec![
                    occ(4, REC, true),       // def of loop_fn at line 5
                    occ(6, REC, false),      // recursive self-call (line 7, inside loop_fn)
                    occ(7, EXTERNAL, false), // call into std (no def in index)
                    occ(8, "local 0", false),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let ranges = vec![range("src/m.rs", "S:loop", 5, 12, NodeKind::RustFunction)];
        assert!(
            overlay_edges(&index, &ranges).is_empty(),
            "external / self / local occurrences must not produce edges"
        );
    }

    /// A reference to a *type* (non-callable) yields `References`, and the
    /// caller is the innermost enclosing symbol — the method, not the `impl`
    /// or module that also span the line.
    #[test]
    fn type_reference_picks_innermost_caller_and_references_kind() {
        const POINT: &str = "rust-analyzer cargo demo 0.1.0 demo/Point#";
        let index = Index {
            documents: vec![Document {
                relative_path: "src/g.rs".into(),
                occurrences: vec![
                    occ(2, POINT, true),   // struct Point defined at line 3
                    occ(20, POINT, false), // used inside `make` at line 21
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let ranges = vec![
            range("src/g.rs", "S:mod", 1, 40, NodeKind::RustModule),
            range("src/g.rs", "S:Point", 3, 6, NodeKind::RustStruct),
            range("src/g.rs", "S:impl", 8, 35, NodeKind::RustModule),
            range("src/g.rs", "S:make", 18, 24, NodeKind::RustMethod),
        ];
        let edges = overlay_edges(&index, &ranges);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].edge.from_symbol_id.as_str(),
            "S:make",
            "innermost caller"
        );
        assert_eq!(edges[0].edge.to_symbol_id.as_str(), "S:Point");
        assert_eq!(
            edges[0].edge.kind,
            EdgeKind::References,
            "type ref, not a call"
        );
    }

    /// #75: the same `(caller, callee, kind)` hit at *several* lines collapses to
    /// one edge that nevertheless carries **every** call site — ascending and
    /// de-duplicated — and `edge.line` mirrors the lowest one for back-compat.
    #[test]
    fn repeated_call_sites_aggregate_every_line() {
        const BAR: &str = "rust-analyzer cargo demo 0.1.0 demo/Foo#bar().";
        let index = Index {
            documents: vec![Document {
                relative_path: "src/h.rs".into(),
                occurrences: vec![
                    occ(4, BAR, true),   // def of bar at line 5
                    occ(20, BAR, false), // call at line 21 (inside caller)
                    occ(24, BAR, false), // call at line 25
                    occ(20, BAR, false), // duplicate of line 21 — must collapse
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let ranges = vec![
            range("src/h.rs", "S:bar", 5, 6, NodeKind::RustMethod),
            range("src/h.rs", "S:caller", 18, 30, NodeKind::RustFunction),
        ];

        let edges = overlay_edges(&index, &ranges);
        assert_eq!(edges.len(), 1, "still one edge per (caller, callee, kind)");
        assert_eq!(edges[0].edge.from_symbol_id.as_str(), "S:caller");
        assert_eq!(edges[0].edge.to_symbol_id.as_str(), "S:bar");
        assert_eq!(
            edges[0].lines,
            vec![21, 25],
            "all distinct call sites, ascending, de-duplicated"
        );
        assert_eq!(
            edges[0].edge.line, 21,
            "edge.line mirrors the lowest call site"
        );

        // The serialised evidence carries the full list, with `line` kept for
        // back-compat readers (#75).
        let json: serde_json::Value =
            serde_json::from_str(&evidence_json(&edges[0].lines, RESOLVER_SCIP)).unwrap();
        assert_eq!(json["line"], 21);
        assert_eq!(json["lines"], serde_json::json!([21, 25]));
    }

    #[test]
    fn parse_index_round_trips_bytes() {
        let index = Index {
            documents: vec![Document {
                relative_path: "a.rs".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = index.write_to_bytes().expect("serialize SCIP index");
        let back = parse_index(&bytes).expect("parse SCIP index");
        assert_eq!(back.documents.len(), 1);
        assert_eq!(back.documents[0].relative_path, "a.rs");
    }

    /// End-to-end: an `index.scip` on disk plus symbol ranges in the store
    /// yields a persisted, high-confidence cross-file `Calls` edge tagged
    /// `scip`. Also proves the no-`.scip` case is a clean no-op.
    #[test]
    fn ingest_writes_high_confidence_scip_edge_from_disk() {
        use crate::edge_confidence::{confidence_for_edge, EdgeConfidence};
        use groundgraph_core::{Node, NodeKind};

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        let mut store = Store::open(repo.join("graph.db")).expect("open store");
        store.migrate().expect("migrate");

        // No `.scip` yet → no-op.
        let none = ingest_scip_overlay(&mut store, repo).expect("no-op ok");
        assert_eq!(none, ScipOverlayResult::default());

        // Seed the structural facts the tree-sitter pass would have produced.
        for (id, kind) in [
            ("S:caller", NodeKind::RustFunction),
            ("S:run", NodeKind::RustMethod),
        ] {
            store
                .upsert_node(&Node::new(ArtifactId::new(id), kind))
                .expect("node");
        }
        store
            .upsert_symbol_range(&range("a.rs", "S:caller", 8, 15, NodeKind::RustFunction))
            .expect("range a");
        store
            .upsert_symbol_range(&range("b.rs", "S:run", 5, 9, NodeKind::RustMethod))
            .expect("range b");

        // Write an index.scip: `Svc#run` defined in b.rs, called from a.rs.
        const RUN: &str = "rust-analyzer cargo demo 0.1.0 demo/Svc#run().";
        let index = Index {
            documents: vec![
                Document {
                    relative_path: "b.rs".into(),
                    occurrences: vec![occ(4, RUN, true)], // def -> line 5 (inside run)
                    ..Default::default()
                },
                Document {
                    relative_path: "a.rs".into(),
                    occurrences: vec![occ(9, RUN, false)], // use -> line 10 (inside caller)
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let scip_dir = repo.join(".groundgraph").join("scip");
        std::fs::create_dir_all(&scip_dir).expect("mkdir scip");
        std::fs::write(scip_dir.join("index.scip"), index.write_to_bytes().unwrap())
            .expect("write scip");

        let res = ingest_scip_overlay(&mut store, repo).expect("ingest");
        assert_eq!(
            res,
            ScipOverlayResult {
                scip_files: 1,
                documents: 2,
                edges: 1,
                suppressed: 0
            }
        );

        let edges = store
            .list_edges_from(&ArtifactId::new("S:caller"))
            .expect("edges");
        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert_eq!(edge.to_id.as_str(), "S:run");
        assert_eq!(edge.kind, EdgeKind::Calls);
        assert_eq!(edge.indexer.as_deref(), Some(RESOLVER_SCIP));
        assert_eq!(confidence_for_edge(edge), EdgeConfidence::High);

        // Re-ingesting is idempotent (no duplicate edge).
        let again = ingest_scip_overlay(&mut store, repo).expect("re-ingest");
        assert_eq!(again.edges, 1);
        assert_eq!(
            store
                .list_edges_from(&ArtifactId::new("S:caller"))
                .unwrap()
                .len(),
            1
        );
    }

    /// A SCIP index produced by running the indexer *inside a sub-module* — its
    /// `metadata.project_root` is a subdir of the repo (e.g. `scip-go` run in
    /// `asc-cli/` because go.mod is not at the repo root) — emits module-relative
    /// document paths (`a.go`), but the store keys symbol ranges repo-root
    /// relative (`asc-cli/a.go`). The overlay must rebase doc paths by
    /// `project_root` so the precise edges still bind; without rebasing the
    /// module is invisible and zero edges land.
    #[test]
    fn ingest_rebases_submodule_paths_by_project_root() {
        use groundgraph_core::{Node, NodeKind};

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        let mut store = Store::open(repo.join("graph.db")).expect("open store");
        store.migrate().expect("migrate");

        for (id, kind) in [
            ("S:caller", NodeKind::RustFunction),
            ("S:run", NodeKind::RustMethod),
        ] {
            store
                .upsert_node(&Node::new(ArtifactId::new(id), kind))
                .expect("node");
        }
        // Ranges are keyed repo-root-relative: the module lives under `asc-cli/`.
        store
            .upsert_symbol_range(&range(
                "asc-cli/a.go",
                "S:caller",
                8,
                15,
                NodeKind::RustFunction,
            ))
            .expect("range a");
        store
            .upsert_symbol_range(&range("asc-cli/b.go", "S:run", 5, 9, NodeKind::RustMethod))
            .expect("range b");

        // scip-go run in `asc-cli/`: project_root is the module dir and the
        // document paths are module-relative (`b.go`, not `asc-cli/b.go`).
        const RUN: &str = "scip-go gomod demo 0.1.0 demo/Svc#run().";
        let mut metadata = Metadata::new();
        metadata.project_root = format!("file://{}", repo.join("asc-cli").to_string_lossy());
        let index = Index {
            metadata: protobuf::MessageField::some(metadata),
            documents: vec![
                Document {
                    relative_path: "b.go".into(),
                    occurrences: vec![occ(4, RUN, true)],
                    ..Default::default()
                },
                Document {
                    relative_path: "a.go".into(),
                    occurrences: vec![occ(9, RUN, false)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let scip_dir = repo.join(".groundgraph").join("scip");
        std::fs::create_dir_all(&scip_dir).expect("mkdir scip");
        std::fs::write(scip_dir.join("go.scip"), index.write_to_bytes().unwrap())
            .expect("write scip");

        let res = ingest_scip_overlay(&mut store, repo).expect("ingest");
        assert_eq!(
            res.edges, 1,
            "sub-module edge must bind after rebasing doc paths by project_root"
        );
        let edges = store
            .list_edges_from(&ArtifactId::new("S:caller"))
            .expect("edges");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to_id.as_str(), "S:run");
        assert_eq!(edges[0].kind, EdgeKind::Calls);
    }

    /// SCIP-authoritative gap-fill (ADR-0001 §3.2, D2). On a file SCIP covered,
    /// a *heuristic-only* edge SCIP did not reproduce (a wrong-overload / false
    /// positive) is suppressed — SCIP is the single precision source there. On
    /// a file SCIP never analysed, the heuristic edge survives as gap-fill.
    #[test]
    fn ingest_suppresses_heuristic_edges_only_on_scip_covered_files() {
        use groundgraph_core::Node;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        let mut store = Store::open(repo.join("graph.db")).expect("open store");
        store.migrate().expect("migrate");

        for (id, kind) in [
            ("S:caller", NodeKind::RustFunction),
            ("S:run", NodeKind::RustMethod),
            ("S:bogus", NodeKind::RustMethod),
            ("S:g1", NodeKind::RustFunction),
            ("S:gtarget", NodeKind::RustMethod),
        ] {
            store
                .upsert_node(&Node::new(ArtifactId::new(id), kind))
                .expect("node");
        }
        store
            .upsert_symbol_range(&range(
                "covered.rs",
                "S:caller",
                8,
                15,
                NodeKind::RustFunction,
            ))
            .expect("range caller");
        store
            .upsert_symbol_range(&range("def.rs", "S:run", 5, 9, NodeKind::RustMethod))
            .expect("range run");

        // Heuristic precision the in-process resolver produced. On covered.rs it
        // guessed the WRONG target (S:bogus, not S:run). On gap.rs — a file no
        // SCIP index covers — it is the only precision we have.
        let heuristic = |from: &str, to: &str, file: &str| {
            let mut e = EdgeAssertion::fact(
                ArtifactId::new(from),
                ArtifactId::new(to),
                EdgeKind::Calls,
                EdgeSource::LanguageAdapter,
            );
            e.indexer = Some("rust_treesitter".to_string());
            e.source_file = Some(file.to_string());
            e
        };
        store
            .upsert_edge(&heuristic("S:caller", "S:bogus", "covered.rs"))
            .expect("heur covered");
        store
            .upsert_edge(&heuristic("S:g1", "S:gtarget", "gap.rs"))
            .expect("heur gap");

        // SCIP: Svc#run defined in def.rs, correctly called from covered.rs.
        const RUN: &str = "rust-analyzer cargo demo 0.1.0 demo/Svc#run().";
        let index = Index {
            documents: vec![
                Document {
                    relative_path: "def.rs".into(),
                    occurrences: vec![occ(4, RUN, true)],
                    ..Default::default()
                },
                Document {
                    relative_path: "covered.rs".into(),
                    occurrences: vec![occ(9, RUN, false)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let scip_dir = repo.join(".groundgraph").join("scip");
        std::fs::create_dir_all(&scip_dir).expect("mkdir scip");
        std::fs::write(scip_dir.join("index.scip"), index.write_to_bytes().unwrap())
            .expect("write scip");

        let res = ingest_scip_overlay(&mut store, repo).expect("ingest");
        assert_eq!(res.edges, 1, "one SCIP Calls edge overlaid");
        assert_eq!(
            res.suppressed, 1,
            "the one wrong heuristic edge on covered.rs"
        );

        let covered = store
            .list_edges_from(&ArtifactId::new("S:caller"))
            .expect("covered edges");
        assert_eq!(
            covered.len(),
            1,
            "wrong heuristic edge gone, only SCIP remains"
        );
        assert_eq!(covered[0].to_id.as_str(), "S:run");
        assert_eq!(covered[0].indexer.as_deref(), Some(RESOLVER_SCIP));

        let gap = store
            .list_edges_from(&ArtifactId::new("S:g1"))
            .expect("gap edges");
        assert_eq!(
            gap.len(),
            1,
            "heuristic gap-fill on the uncovered file survives"
        );
        assert_eq!(gap[0].to_id.as_str(), "S:gtarget");
        assert_eq!(gap[0].indexer.as_deref(), Some("rust_treesitter"));
    }
}
