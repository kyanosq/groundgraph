//! PR Impact engine.
//!
//! MVP-4 (PRD §4 / implementation plan §MVP-4):
//! - Read `git diff --unified=0 base..head`.
//! - Resolve changed files to changed symbols via `symbol_ranges`.
//! - Walk manifest-declared relationships (direct + parent class).
//! - For changed doc sections, walk `documents` → Requirement → impl/tests.
//! - Report changed_symbols, affected_requirements, affected_docs, linked_tests
//!   and warnings.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use groundgraph_core::{ArtifactId, EdgeKind, NodeKind};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

use crate::config::{resolve_storage_path, EngineConfig};
use crate::error::EngineResult;
use crate::git_diff::{git_diff, parse_unified_diff, ChangedFile, Hunk};
use crate::index::{index_repository, IndexOptions};
use crate::slice::SliceItem;

/// P15 — one real graph edge traversed while building an
/// [`ImpactReport`]. The Mermaid exporter renders these edges
/// verbatim so the diagram cannot show relationships that are not
/// backed by the store.
///
/// `kind` mirrors [`EdgeKind::as_str`] lowercased (`"calls"`,
/// `"declares_implementation"`, …). We carry it as a string so we
/// can also surface synthetic-but-structural kinds (`"contains"`
/// for `file → changed_symbol`) without inflating `EdgeKind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct ImpactOptions {
    pub repo_root: PathBuf,
    pub base_ref: String,
    pub head_ref: String,
    /// If true, run a full re-index before computing impact (default true).
    pub reindex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactPolicy {
    pub propagate_to_parent_symbol: bool,
    pub include_doc_changes: bool,
    pub stale_doc_level: String,
    pub missing_test_change_level: String,
    pub propagation: ImpactPropagation,
}

/// P14 — knobs that control fact-edge propagation from
/// `changed_symbols`. The default follows `EdgeKind::Calls` /
/// `EdgeKind::References` one hop outward (callers of the changed
/// symbols) and lifts any reachable `TestCase` / `TestGroup` into
/// `linked_tests`. Set `call_depth = 0` to disable for repos where the
/// LSP / analyzer edges are noisy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactPropagation {
    /// Maximum BFS depth on reverse `Calls` / `References` edges from
    /// `changed_symbols`. `0` disables propagation entirely.
    pub call_depth: usize,
    /// Hard cap on the total number of propagated symbols to keep
    /// blast-radius bounded on big repos.
    pub max_propagated_symbols: usize,
}

impl Default for ImpactPropagation {
    fn default() -> Self {
        Self {
            call_depth: 1,
            max_propagated_symbols: 256,
        }
    }
}

impl Default for ImpactPolicy {
    fn default() -> Self {
        Self {
            propagate_to_parent_symbol: true,
            include_doc_changes: true,
            stale_doc_level: "info".into(),
            missing_test_change_level: "warning".into(),
            propagation: ImpactPropagation::default(),
        }
    }
}

impl From<&crate::config::ImpactConfig> for ImpactPolicy {
    fn from(value: &crate::config::ImpactConfig) -> Self {
        Self {
            propagate_to_parent_symbol: value.propagate_to_parent_symbol,
            include_doc_changes: value.include_doc_changes,
            stale_doc_level: value.stale_doc_level.clone(),
            missing_test_change_level: value.missing_test_change_level.clone(),
            propagation: ImpactPropagation::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImpactReport {
    pub changed_files: Vec<String>,
    pub changed_symbols: Vec<SliceItem>,
    pub changed_doc_sections: Vec<SliceItem>,
    pub affected_requirements: Vec<SliceItem>,
    pub affected_docs: Vec<SliceItem>,
    pub linked_tests: Vec<SliceItem>,
    /// Implementation symbols that declare any affected requirement.
    /// Populated regardless of whether the implementation was itself changed
    /// — this is what PRD §4.4 "Doc Impact" requires so the report stays
    /// actionable for doc-only changes.
    pub linked_implementations: Vec<SliceItem>,
    /// AI-authored business candidates the human reviewer has accepted
    /// AND whose cited evidence intersects the changed code. These
    /// candidates have promoted into the confirmed graph and a code
    /// change against them now warrants a re-review.
    #[serde(default)]
    pub affected_confirmed_candidates: Vec<SliceItem>,
    /// P14 — symbols reached via reverse `Calls` / `References` BFS
    /// from `changed_symbols`. Empty when propagation is disabled
    /// (depth=0) or when no caller exists in the graph. Order is
    /// stable (id-sorted) so JSON snapshots stay deterministic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub propagated_symbols: Vec<SliceItem>,
    /// P15 — real edges traversed while building the report. The
    /// CLI's `--format mermaid` consumes this trace so diagrams
    /// don't synthesise approximate edges between changed symbols
    /// and downstream artefacts. Sorted lexicographically by
    /// `(from, to, kind)` for deterministic output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub impact_edges: Vec<ImpactEdge>,
    pub warnings: Vec<String>,
    pub info: Vec<String>,
}

pub fn run_impact(options: ImpactOptions) -> EngineResult<ImpactReport> {
    let config = load_config(&options.repo_root)?;
    if options.reindex && config.impact.auto_reindex_changed_files {
        index_repository(IndexOptions::all(options.repo_root.clone()))
            .context("re-indexing repository before impact")?;
    }
    let db_path = resolve_storage_path(&options.repo_root, &config)?;
    let store = Store::open(&db_path)?;

    let diff_text = git_diff(&options.repo_root, &options.base_ref, &options.head_ref)?;
    let changed = parse_unified_diff(&diff_text);
    let mut report =
        compute_impact_with_policy(&store, &changed, ImpactPolicy::from(&config.impact))?;
    // Surface accepted AI candidates whose evidence intersects the
    // changed code. We do this after the core walk so the candidate
    // YAML is loaded once per impact run and only when there's actually
    // a confirmed graph to consult.
    merge_confirmed_candidates(&mut report, &options.repo_root)?;
    Ok(report)
}

/// Compute an impact report from an already-parsed diff. Useful in tests.
pub fn compute_impact(store: &Store, changed: &[ChangedFile]) -> EngineResult<ImpactReport> {
    compute_impact_with_policy(store, changed, ImpactPolicy::default())
}

pub fn compute_impact_with_policy(
    store: &Store,
    changed: &[ChangedFile],
    policy: ImpactPolicy,
) -> EngineResult<ImpactReport> {
    let mut report = ImpactReport::default();
    let mut affected_reqs: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut changed_symbol_ids: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut changed_doc_section_ids: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut edge_trace: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut any_test_changed = false;

    for file in changed {
        report.changed_files.push(file.path.clone());
        // Language-agnostic test detection: Go `_test.go`, Rust `tests/`,
        // JS/TS `*.test.ts`, Swift/Java `FooTests` — not just Dart `test/`.
        // (#247/#248)
        if crate::path_class::is_test_path(&file.path) {
            any_test_changed = true;
        }

        for hunk in &file.hunks {
            let symbols = find_changed_symbols(store, &file.path, *hunk)?;
            for symbol in symbols {
                changed_symbol_ids.insert(symbol.symbol_id.clone());
                report.changed_symbols.push(SliceItem {
                    id: symbol.symbol_id.to_string(),
                    kind: symbol.symbol_kind.as_str().to_string(),
                    path: Some(symbol.file_path.clone()),
                    name: Some(symbol.qualified_name.clone()),
                    line_range: Some((symbol.start_line, symbol.end_line)),
                });
                // P15 — structural `file → symbol` containment so
                // the Mermaid diagram can anchor changed symbols
                // under their file without inventing an edge.
                edge_trace.insert((
                    file.path.clone(),
                    symbol.symbol_id.to_string(),
                    "contains".into(),
                ));

                // Propagate from symbol → declared requirement. By default we
                // walk parent symbols; config can disable that for stricter
                // direct-only impact.
                let file_ranges = store.list_symbol_ranges_for_file(&symbol.file_path)?;
                let mut visited: BTreeSet<ArtifactId> = BTreeSet::new();
                let mut cursor: Option<ArtifactId> = Some(symbol.symbol_id.clone());
                while let Some(id) = cursor.clone() {
                    if !visited.insert(id.clone()) {
                        break;
                    }
                    let mut hit = false;
                    for edge in store.list_edges_from(&id)? {
                        if edge.kind == EdgeKind::DeclaresImplementation {
                            // P15 — record the *real* declarer (which
                            // may be `id` itself or an ancestor when
                            // policy walks parents) instead of pinning
                            // every requirement onto the changed leaf.
                            edge_trace.insert((
                                id.to_string(),
                                edge.to_id.to_string(),
                                "declares_implementation".into(),
                            ));
                            affected_reqs.insert(edge.to_id);
                            hit = true;
                        }
                    }
                    if hit {
                        break;
                    }
                    if !policy.propagate_to_parent_symbol {
                        break;
                    }
                    cursor = file_ranges
                        .iter()
                        .find(|r| r.symbol_id == id)
                        .and_then(|r| r.parent_symbol_id.clone());
                }
            }

            // Markdown change → affected doc sections + their REQ.
            if policy.include_doc_changes
                && (file.path.ends_with(".md") || file.path.ends_with(".mdx"))
            {
                let sections = find_doc_sections_for(store, &file.path, *hunk)?;
                for sec in sections {
                    if !changed_doc_section_ids.contains(&sec.id) {
                        changed_doc_section_ids.insert(sec.id.clone());
                        report.changed_doc_sections.push(SliceItem {
                            id: sec.id.to_string(),
                            kind: sec.kind.as_str().to_string(),
                            path: sec.path.clone(),
                            name: sec.name.clone(),
                            line_range: Some((
                                sec.start_line.unwrap_or(0),
                                sec.end_line.unwrap_or(0),
                            )),
                        });
                    }
                    for edge in store.list_edges_from(&sec.id)? {
                        if edge.kind == EdgeKind::Documents {
                            edge_trace.insert((
                                sec.id.to_string(),
                                edge.to_id.to_string(),
                                "documents".into(),
                            ));
                            affected_reqs.insert(edge.to_id);
                        }
                    }
                }
            }
        }
    }

    // Resolve affected requirements → docs, tests, implementations.
    let mut docs_set: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut tests_set: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut impl_set: BTreeSet<ArtifactId> = BTreeSet::new();
    for req_id in &affected_reqs {
        for edge in store.list_edges_to(req_id)? {
            match edge.kind {
                EdgeKind::Documents => {
                    edge_trace.insert((
                        edge.from_id.to_string(),
                        req_id.to_string(),
                        "documents".into(),
                    ));
                    docs_set.insert(edge.from_id);
                }
                EdgeKind::DeclaresVerification => {
                    edge_trace.insert((
                        edge.from_id.to_string(),
                        req_id.to_string(),
                        "declares_verification".into(),
                    ));
                    tests_set.insert(edge.from_id);
                }
                EdgeKind::DeclaresImplementation => {
                    edge_trace.insert((
                        edge.from_id.to_string(),
                        req_id.to_string(),
                        "declares_implementation".into(),
                    ));
                    impl_set.insert(edge.from_id);
                }
                _ => {}
            }
        }
    }

    for req_id in &affected_reqs {
        if let Some(node) = store.find_node(req_id)? {
            report.affected_requirements.push(SliceItem {
                id: node.id.to_string(),
                kind: node.kind.as_str().to_string(),
                path: node.path,
                name: node.name,
                line_range: None,
            });
        }
    }
    for doc_id in &docs_set {
        if let Some(node) = store.find_node(doc_id)? {
            report.affected_docs.push(SliceItem {
                id: node.id.to_string(),
                kind: node.kind.as_str().to_string(),
                path: node.path,
                name: node.name,
                line_range: None,
            });
        }
    }
    for test_id in &tests_set {
        if let Some(node) = store.find_node(test_id)? {
            report.linked_tests.push(SliceItem {
                id: node.id.to_string(),
                kind: node.kind.as_str().to_string(),
                path: node.path,
                name: node.name,
                line_range: None,
            });
        }
    }
    for impl_id in &impl_set {
        if let Some(node) = store.find_node(impl_id)? {
            report.linked_implementations.push(SliceItem {
                id: node.id.to_string(),
                kind: node.kind.as_str().to_string(),
                path: node.path,
                name: node.name,
                line_range: match (node.start_line, node.end_line) {
                    (Some(s), Some(e)) => Some((s, e)),
                    _ => None,
                },
            });
        }
    }

    // P14 — propagate from changed_symbols along reverse Calls /
    // References edges so callers / tests that exercise the touched
    // code surface even without a manual `declares_verification`
    // manifest. Done before the warning pass so a propagated test
    // suppresses the "no linked test changed" warning.
    propagate_via_calls_and_references(
        store,
        &mut report,
        &changed_symbol_ids,
        policy.propagation.call_depth,
        policy.propagation.max_propagated_symbols,
        &mut edge_trace,
    )?;

    sort_items(&mut report.changed_symbols);
    sort_items(&mut report.changed_doc_sections);
    sort_items(&mut report.affected_requirements);
    sort_items(&mut report.affected_docs);
    sort_items(&mut report.linked_tests);
    sort_items(&mut report.linked_implementations);
    sort_items(&mut report.propagated_symbols);

    // P15 — drain the dedup set into `impact_edges` in stable order.
    // The set already enforces `(from, to, kind)` uniqueness so the
    // Mermaid exporter can iterate without an additional pass.
    report.impact_edges = edge_trace
        .into_iter()
        .map(|(from, to, kind)| ImpactEdge { from, to, kind })
        .collect();

    // Warnings & info.
    if !report.affected_requirements.is_empty()
        && !report.linked_tests.is_empty()
        && !any_test_changed
    {
        push_impact_message(
            &mut report,
            &policy.missing_test_change_level,
            "Affected requirement has linked tests, but no linked test changed in this PR."
                .to_string(),
        );
    }
    if !report.affected_requirements.is_empty() && report.changed_doc_sections.is_empty() {
        push_impact_message(
            &mut report,
            &policy.stale_doc_level,
            "Linked doc sections were not changed. Review whether docs are still accurate."
                .to_string(),
        );
    }

    Ok(report)
}

/// Merge accepted business candidates whose evidence intersects the
/// already-computed changed-code/doc set into
/// `report.affected_confirmed_candidates`.
///
/// This is the bridge between P9 (AI-authored candidates) and P4
/// (PR impact): once a human has accepted a candidate, that candidate
/// is part of the confirmed graph; any subsequent change to its
/// evidence files / symbols must surface in `impact` reports so the
/// reviewer notices.
pub fn merge_confirmed_candidates(report: &mut ImpactReport, repo_root: &Path) -> EngineResult<()> {
    use crate::business_candidates::{
        candidate_artifact_id, load_business_candidates, ReviewStatus,
    };
    let outcome = match load_business_candidates(repo_root) {
        Ok(o) => o,
        Err(_) => return Ok(()),
    };
    // Build the universe of "changed" anchors that an accepted
    // candidate's evidence might intersect: symbols, doc sections,
    // and raw changed file paths (so a per-file evidence id matches
    // even when no symbol survives).
    let changed_anchors: BTreeSet<String> = report
        .changed_symbols
        .iter()
        .chain(report.changed_doc_sections.iter())
        .map(|s| s.id.clone())
        .collect();
    let changed_files: BTreeSet<&str> = report.changed_files.iter().map(|s| s.as_str()).collect();

    for c in &outcome.document.candidates {
        if c.review_status() != Some(ReviewStatus::Accepted) {
            continue;
        }
        let touched = c.evidence.iter().any(|ev| {
            if changed_anchors.contains(ev) {
                return true;
            }
            // Best-effort path-level match: evidence ids encode the
            // source file after `::` and before `#`, e.g.
            // `dart_method::lib/foo.dart#Foo.bar`. If we can pluck the
            // file out and it intersects a changed file, count it as
            // touched.
            let body = ev.split_once("::").map(|(_, t)| t).unwrap_or(ev);
            let file_part = body.split_once('#').map(|(p, _)| p).unwrap_or(body);
            changed_files.contains(file_part)
        });
        if !touched {
            continue;
        }
        report.affected_confirmed_candidates.push(SliceItem {
            id: candidate_artifact_id(&c.id).to_string(),
            kind: "business_candidate".into(),
            path: None,
            name: Some(c.name.clone()),
            line_range: None,
        });
    }
    sort_items(&mut report.affected_confirmed_candidates);
    if !report.affected_confirmed_candidates.is_empty() {
        report.info.push(format!(
            "{} accepted candidate(s) intersect this change — re-review recommended",
            report.affected_confirmed_candidates.len()
        ));
    }
    Ok(())
}

/// P14 — BFS along reverse `EdgeKind::Calls` / `EdgeKind::References`
/// edges from `changed_symbols`. Each newly-reached symbol is appended
/// to `report.propagated_symbols`; `TestCase` / `TestGroup` nodes are
/// additionally lifted into `report.linked_tests` so PR reviewers see
/// downstream tests even when the requirement manifest does not yet
/// link them.
///
/// `depth = 0` is a no-op. `max_total` bounds the total number of
/// propagated symbols (best-effort): once exceeded, we stop expanding
/// and surface a `info` line. The implementation is intentionally
/// dependency-free — we use the store's existing `list_edges_to`
/// surface and a manual visited-set BFS so we never accidentally
/// expand across edge kinds we didn't whitelist.
fn propagate_via_calls_and_references(
    store: &Store,
    report: &mut ImpactReport,
    changed_symbol_ids: &BTreeSet<ArtifactId>,
    depth: usize,
    max_total: usize,
    edge_trace: &mut BTreeSet<(String, String, String)>,
) -> Result<()> {
    if depth == 0 || changed_symbol_ids.is_empty() {
        return Ok(());
    }
    let already_linked_tests: BTreeSet<String> =
        report.linked_tests.iter().map(|t| t.id.clone()).collect();
    let mut visited: BTreeSet<ArtifactId> = changed_symbol_ids.clone();
    let mut frontier: Vec<ArtifactId> = changed_symbol_ids.iter().cloned().collect();
    let mut truncated = false;
    'outer: for _ in 0..depth {
        let mut next: Vec<ArtifactId> = Vec::new();
        for id in &frontier {
            for edge in store.list_edges_to(id)? {
                if !matches!(edge.kind, EdgeKind::Calls | EdgeKind::References) {
                    continue;
                }
                let caller = edge.from_id;
                // P15 — always record the *real* (caller → callee)
                // edge even when we have already visited the caller
                // through a different path. The dedup set below
                // handles uniqueness.
                edge_trace.insert((
                    caller.to_string(),
                    id.to_string(),
                    edge.kind.as_str().to_string(),
                ));
                if !visited.insert(caller.clone()) {
                    continue;
                }
                if report.propagated_symbols.len() >= max_total {
                    truncated = true;
                    break 'outer;
                }
                let Some(node) = store.find_node(&caller)? else {
                    continue;
                };
                let item = SliceItem {
                    id: node.id.to_string(),
                    kind: node.kind.as_str().to_string(),
                    path: node.path.clone(),
                    name: node.name.clone(),
                    line_range: match (node.start_line, node.end_line) {
                        (Some(s), Some(e)) => Some((s, e)),
                        _ => None,
                    },
                };
                // Reachable tests must also appear in linked_tests so
                // the reviewer's "what should I run?" answer benefits
                // from the fact edges without us double-counting.
                if matches!(node.kind, NodeKind::TestCase | NodeKind::TestGroup)
                    && !already_linked_tests.contains(&item.id)
                {
                    report.linked_tests.push(item.clone());
                }
                report.propagated_symbols.push(item);
                next.push(caller);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    if truncated {
        report.info.push(format!(
            "impact: 调用 / 引用 传播达到上限 {max_total}，结果已截断"
        ));
    }
    Ok(())
}

fn push_impact_message(report: &mut ImpactReport, level: &str, message: String) {
    match level.trim().to_ascii_lowercase().as_str() {
        "warning" | "warn" => report.warnings.push(message),
        "info" => report.info.push(message),
        "off" | "none" | "ignore" => {}
        _ => report.info.push(message),
    }
}

fn find_changed_symbols(
    store: &Store,
    path: &str,
    hunk: Hunk,
) -> Result<Vec<groundgraph_core::SymbolRange>> {
    let ranges = store.find_symbols_intersecting(path, hunk.new_start, hunk.new_end)?;
    Ok(filter_most_specific_symbols(ranges))
}

fn filter_most_specific_symbols(
    ranges: Vec<groundgraph_core::SymbolRange>,
) -> Vec<groundgraph_core::SymbolRange> {
    ranges
        .iter()
        .filter(|candidate| {
            !ranges.iter().any(|other| {
                other.symbol_id != candidate.symbol_id
                    && candidate.start_line <= other.start_line
                    && other.end_line <= candidate.end_line
                    // `saturating_sub`: a drifted/corrupt range with
                    // `end < start` must not panic (debug) or wrap (release).
                    && other.end_line.saturating_sub(other.start_line)
                        < candidate.end_line.saturating_sub(candidate.start_line)
            })
        })
        .cloned()
        .collect()
}

fn find_doc_sections_for(
    store: &Store,
    path: &str,
    hunk: Hunk,
) -> Result<Vec<groundgraph_core::Node>> {
    // Doc sections are stored as nodes with start_line/end_line; we iterate
    // by kind here. The fixture has few enough sections that a linear scan
    // is fine.
    let mut hits = Vec::new();
    for node in store.list_nodes_by_kind(NodeKind::DocSection)? {
        if node.path.as_deref() != Some(path) {
            continue;
        }
        let start = node.start_line.unwrap_or(0);
        let end = node.end_line.unwrap_or(u32::MAX);
        if hunk.new_start <= end && start <= hunk.new_end {
            hits.push(node);
        }
    }
    Ok(hits)
}

fn sort_items(items: &mut [SliceItem]) {
    items.sort_by(|a, b| a.id.cmp(&b.id));
}

fn load_config(repo_root: &Path) -> crate::error::EngineResult<EngineConfig> {
    crate::config::load_config(repo_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::SymbolRange;

    fn range(id: &str, start: u32, end: u32) -> SymbolRange {
        SymbolRange {
            file_path: "f.rs".into(),
            symbol_id: ArtifactId::new(id),
            start_line: start,
            end_line: end,
            symbol_kind: NodeKind::DartMethod,
            qualified_name: id.into(),
            parent_symbol_id: None,
        }
    }

    /// A drifted / externally-written `symbol_ranges` row with `end_line <
    /// start_line` must not panic the "most specific" filter. Real indexers
    /// always emit `end >= start` (byte-offset derived), so this is the same
    /// DB-drift / hostile-input hardening the project applies elsewhere
    /// (#63 decode sanitising, #181 git_diff `checked_add`): the `end - start`
    /// span math underflows in debug and wraps in release without a guard.
    #[test]
    fn filter_most_specific_tolerates_reversed_range() {
        let enclosing = range("c", 1, 100);
        let reversed = range("o", 50, 10); // corrupt: end < start
                                           // Before the fix this panics in debug (`10u32 - 50u32`).
        let out = filter_most_specific_symbols(vec![enclosing, reversed]);
        assert!(
            out.iter().any(|r| r.symbol_id == ArtifactId::new("o")),
            "filter must complete and keep the innermost range: {out:?}"
        );
    }
}
