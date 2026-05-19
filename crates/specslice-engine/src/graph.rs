//! P6 / P6.1: read-only `GraphViewModel` for SpecSlice visualisation.
//!
//! P6 shipped a lane-based read-only export. P6.1 rebuilds the model into a
//! navigable code-graph explorer: module aggregations, parent_id chains and
//! per-view default visibility so that a 1.7k-symbol repo renders as ~20
//! top-level modules rather than 1.7k buttons.
//!
//! Layers (cross-cutting trust dimension):
//!
//! | Layer       | Source                                              |
//! |-------------|-----------------------------------------------------|
//! | `fact`      | Filesystem / parser / docs indexer                  |
//! | `confirmed` | External manifest links (`.specslice/links.yaml`)   |
//! | `candidate` | Future `.specslice/candidates/`                     |
//! | `risk`      | Check / impact findings                             |
//!
//! Columns (lane the UI uses for layout): `documents`, `business`, `code`,
//! `tests`, `risks`.
//!
//! Views (default visible surface a UI renders):
//!
//! | View       | Default visible              | Use case               |
//! |------------|------------------------------|------------------------|
//! | overview   | Top-level modules            | Explore a fresh repo   |
//! | code       | Same as overview             | Code-structure focus   |
//! | business   | Requirement + 1-hop          | Compliance / coverage  |
//! | focus      | Focus + 1-hop neighbourhood  | Drill into one symbol  |
//!
//! Aggregation:
//!
//! - `module::<dir>` virtual nodes are derived from file paths. Nested module
//!   chains (`lib → lib/features → lib/features/editor`) make the UI tree
//!   navigable.
//! - File nodes get `parent_id = module::<dirname>`.
//! - Dart classes / methods / functions / constructors and test cases /
//!   groups get `parent_id = file::<path>` when the file node exists.
//! - `child_count` is the number of direct children — populated for every
//!   aggregator so the UI can render "12 files" badges.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{EdgeAssertion, EdgeCertainty, EdgeSource, EdgeStatus, Node, NodeKind};
use specslice_store::Store;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::checks::{compute_checks_with_policy, CheckFinding, CheckPolicy, CheckSeverity};
use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};

/// Bump whenever the JSON shape changes in a way readers must observe.
pub const GRAPH_SCHEMA_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// Public data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphViewModel {
    pub schema_version: u32,
    pub view: String,
    pub repo_root: String,
    pub generated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    pub stats: GraphStats,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    #[serde(default)]
    pub findings: Vec<GraphFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GraphStats {
    pub modules: usize,
    pub documents: usize,
    pub business_logic: usize,
    pub code_symbols: usize,
    pub tests: usize,
    pub confirmed_edges: usize,
    pub candidate_edges: usize,
    pub risks: usize,
    pub default_visible: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub column: GraphColumn,
    pub layer: GraphLayer,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<(u32, u32)>,
    pub status: GraphStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub child_count: u32,
    #[serde(default)]
    pub default_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub layer: GraphLayer,
    pub status: GraphStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphFinding {
    pub code: String,
    pub severity: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GraphLayer {
    Fact,
    Confirmed,
    Candidate,
    Risk,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GraphStatus {
    Confirmed,
    Proposed,
    Rejected,
    Stale,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GraphColumn {
    Documents,
    Business,
    Code,
    Tests,
    Risks,
}

/// Selects what the UI should render by default. The full graph is always
/// available — `default_visible` per node is the only thing that changes.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GraphView {
    #[default]
    Overview,
    Code,
    Business,
    Focus,
}

impl GraphView {
    pub fn as_str(self) -> &'static str {
        match self {
            GraphView::Overview => "overview",
            GraphView::Code => "code",
            GraphView::Business => "business",
            GraphView::Focus => "focus",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GraphOptions {
    pub view: GraphView,
    pub focus: Option<String>,
    pub include_risks: bool,
    pub include_candidates: bool,
    pub max_nodes: Option<usize>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Build the view model from `.specslice/graph.db`. Read-only.
pub fn build_graph_view(repo_root: &Path, options: GraphOptions) -> Result<GraphViewModel> {
    let config = load_config(repo_root)?;
    let db_path = resolve_storage_path(repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;

    // 1. Materialise raw nodes/edges.
    let mut nodes: Vec<GraphNode> = store
        .list_all_nodes()
        .context("listing nodes for graph view")?
        .iter()
        .map(map_node)
        .collect();
    let mut edges: Vec<GraphEdge> = store
        .list_all_edges()
        .context("listing edges for graph view")?
        .iter()
        .map(map_edge)
        .collect();

    // 2. Inject synthetic module aggregations from file paths.
    inject_modules(&mut nodes);

    // 3. Wire parent_id chains: symbol → file → module.
    wire_parents(&mut nodes);

    // 4. Compute child_count for every aggregator.
    compute_child_counts(&mut nodes);

    // 5. Risk findings, candidate placeholder.
    let mut findings: Vec<GraphFinding> = Vec::new();
    if options.include_risks {
        let report = compute_checks_with_policy(&store, None, CheckPolicy::from(&config.checks))
            .context("computing checks for graph view")?;
        findings.extend(report.findings.iter().map(map_check));
    }
    let _ = options.include_candidates;

    // 6. Focus narrowing — applied before view filter so visibility is sane.
    if let Some(focus_raw) = options.focus.as_deref() {
        match resolve_focus(&nodes, focus_raw) {
            Some(target_id) => {
                let kept = neighbourhood(&nodes, &edges, &target_id);
                nodes.retain(|n| kept.contains(n.id.as_str()));
                edges.retain(|e| kept.contains(e.from.as_str()) && kept.contains(e.to.as_str()));
                findings.retain(|f| match &f.target_id {
                    Some(t) => kept.contains(t.as_str()),
                    None => false,
                });
            }
            None => {
                nodes.clear();
                edges.clear();
                findings.clear();
                findings.push(GraphFinding {
                    code: "focus_not_found".into(),
                    severity: "warning".into(),
                    message: format!("focus id `{focus_raw}` did not match any node or stable key"),
                    target_id: None,
                });
            }
        }
    }

    // 7. max_nodes truncation: priority order keeps focus + confirmed first.
    if let Some(limit) = options.max_nodes {
        if nodes.len() > limit {
            let priority = priority_order(&nodes, &edges, options.focus.as_deref());
            let kept: HashSet<String> = priority.into_iter().take(limit).collect();
            nodes.retain(|n| kept.contains(&n.id));
            edges.retain(|e| kept.contains(&e.from) && kept.contains(&e.to));
            findings.retain(|f| match &f.target_id {
                Some(t) => kept.contains(t),
                None => true,
            });
            findings.push(GraphFinding {
                code: "graph_truncated".into(),
                severity: "warning".into(),
                message: format!("graph exceeded max_nodes={limit}; dropped lower-priority nodes"),
                target_id: None,
            });
        }
    }

    // 8. Apply view-specific default_visible.
    apply_view(&mut nodes, &edges, options.view, options.focus.as_deref());
    if matches!(options.view, GraphView::Business)
        && !nodes
            .iter()
            .any(|n| n.kind == "requirement" && n.default_visible)
    {
        findings.push(GraphFinding {
            code: "no_business_logic".into(),
            severity: "info".into(),
            message:
                "No confirmed business logic in graph. Run `specslice connect propose` to seed candidates."
                    .into(),
            target_id: None,
        });
    }

    sort_nodes(&mut nodes);
    sort_edges(&mut edges);
    sort_findings(&mut findings);

    let stats = compute_stats(&nodes, &edges, &findings);
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into());

    Ok(GraphViewModel {
        schema_version: GRAPH_SCHEMA_VERSION,
        view: options.view.as_str().into(),
        repo_root: repo_root.to_string_lossy().into_owned(),
        generated_at,
        focus: options.focus,
        stats,
        nodes,
        edges,
        findings,
    })
}

// ---------------------------------------------------------------------------
// Module aggregation
// ---------------------------------------------------------------------------

fn inject_modules(nodes: &mut Vec<GraphNode>) {
    let mut module_paths: BTreeSet<String> = BTreeSet::new();
    for n in nodes.iter() {
        let Some(path) = n.path.as_deref() else {
            continue;
        };
        if n.kind == "module" {
            continue;
        }
        // For file-like nodes, the module is the parent directory of the file.
        // For symbol nodes whose path is the source file, this is also the
        // file's parent directory — which is what we want.
        let dir = parent_dir(path);
        for ancestor in ancestor_dirs(&dir) {
            module_paths.insert(ancestor);
        }
    }
    for dir in module_paths {
        let id = format!("module::{dir}");
        if nodes.iter().any(|n| n.id == id) {
            continue;
        }
        let label = if dir.is_empty() {
            "(root)".to_string()
        } else {
            dir.rsplit('/').next().unwrap_or(&dir).to_string()
        };
        nodes.push(GraphNode {
            id,
            kind: "module".into(),
            column: column_for_dir(&dir),
            layer: GraphLayer::Fact,
            label,
            path: Some(dir.clone()),
            line_range: None,
            status: GraphStatus::Confirmed,
            parent_id: None,
            child_count: 0,
            default_visible: false,
            confidence: None,
            source: Some("module_aggregator".into()),
            badges: Vec::new(),
        });
    }
}

fn wire_parents(nodes: &mut [GraphNode]) {
    let id_set: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    // We mutate in two passes to avoid borrowing issues: first pre-compute
    // each node's parent, then assign.
    let mut parents: HashMap<String, String> = HashMap::new();
    for n in nodes.iter() {
        let Some(path) = n.path.as_deref() else {
            continue;
        };
        let parent = if n.kind == "module" {
            let dir = path;
            if dir.is_empty() {
                None
            } else {
                let pd = parent_dir(dir);
                if pd == *dir {
                    None
                } else {
                    Some(format!("module::{pd}"))
                }
            }
        } else if n.kind == "file" {
            Some(format!("module::{}", parent_dir(path)))
        } else {
            // Symbol or test_case style: prefer pointing at the file node when
            // available; otherwise fall back to the directory module.
            let file_id = format!("file::{path}");
            if id_set.contains(&file_id) {
                Some(file_id)
            } else {
                Some(format!("module::{}", parent_dir(path)))
            }
        };
        if let Some(p) = parent {
            // Only retain pointers to nodes we actually have.
            if id_set.contains(&p) && p != n.id {
                parents.insert(n.id.clone(), p);
            }
        }
    }
    for n in nodes.iter_mut() {
        n.parent_id = parents.remove(&n.id);
    }
}

fn compute_child_counts(nodes: &mut [GraphNode]) {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for n in nodes.iter() {
        if let Some(p) = &n.parent_id {
            *counts.entry(p.clone()).or_insert(0) += 1;
        }
    }
    for n in nodes.iter_mut() {
        n.child_count = counts.remove(&n.id).unwrap_or(0);
    }
}

fn parent_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => String::new(),
    }
}

fn ancestor_dirs(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for seg in dir.split('/').filter(|s| !s.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        out.push(acc.clone());
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn column_for_dir(dir: &str) -> GraphColumn {
    let top = dir.split('/').next().unwrap_or("");
    match top {
        "docs" | "doc" => GraphColumn::Documents,
        "test" | "tests" | "integration_test" => GraphColumn::Tests,
        _ => GraphColumn::Code,
    }
}

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

fn map_node(node: &Node) -> GraphNode {
    let column = column_for(node.kind);
    let layer = layer_for_node(node.kind);
    let label = node
        .name
        .clone()
        .or_else(|| node.stable_key.clone())
        .unwrap_or_else(|| node.id.to_string());
    let line_range = match (node.start_line, node.end_line) {
        (Some(s), Some(e)) => Some((s, e)),
        _ => None,
    };
    let mut badges = Vec::new();
    if let Some(key) = &node.stable_key {
        if key != &label {
            badges.push(key.clone());
        }
    }
    GraphNode {
        id: node.id.to_string(),
        kind: node.kind.as_str().into(),
        column,
        layer,
        label,
        path: node.path.clone(),
        line_range,
        status: GraphStatus::Confirmed,
        parent_id: None,
        child_count: 0,
        default_visible: false,
        confidence: None,
        source: node.indexer.clone(),
        badges,
    }
}

fn map_edge(edge: &EdgeAssertion) -> GraphEdge {
    let layer = layer_for_edge(edge);
    let status = match edge.status {
        EdgeStatus::Confirmed => GraphStatus::Confirmed,
        EdgeStatus::Deprecated => GraphStatus::Stale,
    };
    GraphEdge {
        id: edge.id.to_string(),
        from: edge.from_id.to_string(),
        to: edge.to_id.to_string(),
        kind: edge.kind.as_str().into(),
        layer,
        status,
        confidence: Some(edge.confidence),
        source: Some(edge.source.as_str().into()),
        rationale: None,
    }
}

fn map_check(finding: &CheckFinding) -> GraphFinding {
    GraphFinding {
        code: finding.code.clone(),
        severity: severity_to_str(finding.severity).into(),
        message: finding.message.clone(),
        target_id: finding.artifact_id.clone(),
    }
}

fn severity_to_str(s: CheckSeverity) -> &'static str {
    match s {
        CheckSeverity::Error => "error",
        CheckSeverity::Warning => "warning",
        CheckSeverity::Info => "info",
    }
}

fn column_for(kind: NodeKind) -> GraphColumn {
    match kind {
        NodeKind::DocSection | NodeKind::AcceptanceCriterion | NodeKind::Adr => {
            GraphColumn::Documents
        }
        NodeKind::Requirement => GraphColumn::Business,
        NodeKind::File
        | NodeKind::DartClass
        | NodeKind::DartMethod
        | NodeKind::DartFunction
        | NodeKind::DartConstructor => GraphColumn::Code,
        NodeKind::TestCase | NodeKind::TestGroup => GraphColumn::Tests,
    }
}

fn layer_for_node(kind: NodeKind) -> GraphLayer {
    if matches!(kind, NodeKind::Requirement) {
        GraphLayer::Confirmed
    } else {
        GraphLayer::Fact
    }
}

fn layer_for_edge(edge: &EdgeAssertion) -> GraphLayer {
    // Manifest-declared links are the only ones we lift to the Confirmed
    // layer; everything else (filesystem `contains`, parser facts, declared
    // edges without a manifest origin) stays in the Fact layer for now.
    if matches!(edge.source, EdgeSource::ExternalManifest) {
        GraphLayer::Confirmed
    } else {
        let _ = EdgeCertainty::Fact;
        GraphLayer::Fact
    }
}

// ---------------------------------------------------------------------------
// View filter + focus + truncation helpers
// ---------------------------------------------------------------------------

fn apply_view(
    nodes: &mut [GraphNode],
    edges: &[GraphEdge],
    view: GraphView,
    focus_raw: Option<&str>,
) {
    for n in nodes.iter_mut() {
        n.default_visible = false;
    }
    match view {
        GraphView::Overview | GraphView::Code => {
            for n in nodes.iter_mut() {
                if n.kind == "module" && n.parent_id.is_none() {
                    n.default_visible = true;
                }
            }
        }
        GraphView::Business => {
            // Identify requirements present and visible by themselves; also
            // make their immediate doc/impl/test neighbours visible.
            let req_ids: HashSet<String> = nodes
                .iter()
                .filter(|n| n.kind == "requirement")
                .map(|n| n.id.clone())
                .collect();
            let mut visible: HashSet<String> = req_ids.clone();
            for edge in edges {
                if req_ids.contains(&edge.from) {
                    visible.insert(edge.to.clone());
                }
                if req_ids.contains(&edge.to) {
                    visible.insert(edge.from.clone());
                }
            }
            for n in nodes.iter_mut() {
                if visible.contains(&n.id) {
                    n.default_visible = true;
                }
            }
        }
        GraphView::Focus => {
            // When focus is set, the focus subgraph already narrowed `nodes`.
            // Mark all survivors as visible. If somehow no focus was given,
            // fall back to overview semantics.
            if focus_raw.is_some() {
                for n in nodes.iter_mut() {
                    n.default_visible = true;
                }
            } else {
                for n in nodes.iter_mut() {
                    if n.kind == "module" && n.parent_id.is_none() {
                        n.default_visible = true;
                    }
                }
            }
        }
    }
}

fn resolve_focus(nodes: &[GraphNode], focus_raw: &str) -> Option<String> {
    let trimmed = focus_raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if nodes.iter().any(|n| n.id == trimmed) {
        return Some(trimmed.into());
    }
    let prefixed = format!("req::{trimmed}");
    if nodes.iter().any(|n| n.id == prefixed) {
        return Some(prefixed);
    }
    // Allow focusing on a module by path (e.g. `lib/features/editor`).
    let module_id = format!("module::{trimmed}");
    if nodes.iter().any(|n| n.id == module_id) {
        return Some(module_id);
    }
    nodes
        .iter()
        .find(|n| n.badges.iter().any(|b| b == trimmed))
        .map(|n| n.id.clone())
}

fn neighbourhood(nodes: &[GraphNode], edges: &[GraphEdge], target_id: &str) -> HashSet<String> {
    let mut kept = HashSet::new();
    kept.insert(target_id.to_string());
    for e in edges {
        if e.from == target_id {
            kept.insert(e.to.clone());
        }
        if e.to == target_id {
            kept.insert(e.from.clone());
        }
    }
    let valid: BTreeSet<_> = nodes.iter().map(|n| n.id.clone()).collect();
    kept.retain(|id| valid.contains(id));
    kept
}

fn priority_order(nodes: &[GraphNode], edges: &[GraphEdge], focus: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push = |id: &str, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        if seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    };

    if let Some(f) = focus {
        if let Some(resolved) = resolve_focus(nodes, f) {
            push(&resolved, &mut out, &mut seen);
        }
    }

    let confirmed: Vec<&GraphNode> = nodes
        .iter()
        .filter(|n| n.layer == GraphLayer::Confirmed)
        .collect();
    let mut sorted_confirmed = confirmed.clone();
    sorted_confirmed.sort_by(|a, b| compare_node_order(a, b));
    for n in &sorted_confirmed {
        push(&n.id, &mut out, &mut seen);
    }

    let confirmed_ids: HashSet<&str> = sorted_confirmed.iter().map(|n| n.id.as_str()).collect();
    let mut neighbours: Vec<&GraphNode> = Vec::new();
    for e in edges {
        if confirmed_ids.contains(e.from.as_str()) {
            if let Some(node) = nodes.iter().find(|n| n.id == e.to) {
                neighbours.push(node);
            }
        }
        if confirmed_ids.contains(e.to.as_str()) {
            if let Some(node) = nodes.iter().find(|n| n.id == e.from) {
                neighbours.push(node);
            }
        }
    }
    neighbours.sort_by(|a, b| compare_node_order(a, b));
    for n in &neighbours {
        push(&n.id, &mut out, &mut seen);
    }

    let mut rest: Vec<&GraphNode> = nodes.iter().filter(|n| !seen.contains(&n.id)).collect();
    rest.sort_by(|a, b| compare_node_order(a, b));
    for n in &rest {
        push(&n.id, &mut out, &mut seen);
    }

    out
}

fn compare_node_order(a: &GraphNode, b: &GraphNode) -> std::cmp::Ordering {
    let key_a = (
        column_rank(a.column),
        kind_rank(&a.kind),
        a.path.as_deref().unwrap_or(""),
        a.line_range.map(|(s, _)| s).unwrap_or(0),
        a.label.as_str(),
        a.id.as_str(),
    );
    let key_b = (
        column_rank(b.column),
        kind_rank(&b.kind),
        b.path.as_deref().unwrap_or(""),
        b.line_range.map(|(s, _)| s).unwrap_or(0),
        b.label.as_str(),
        b.id.as_str(),
    );
    key_a.cmp(&key_b)
}

fn column_rank(c: GraphColumn) -> u8 {
    match c {
        GraphColumn::Documents => 0,
        GraphColumn::Business => 1,
        GraphColumn::Code => 2,
        GraphColumn::Tests => 3,
        GraphColumn::Risks => 4,
    }
}

/// Aggregator kinds sort first within a column so module ancestors land
/// before their descendants in deterministic outputs.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "module" => 0,
        "requirement" => 1,
        "file" => 2,
        "doc_section" | "acceptance_criterion" | "adr" => 3,
        "dart_class" => 4,
        "dart_function" => 5,
        "dart_method" => 6,
        "dart_constructor" => 7,
        "test_group" => 8,
        "test_case" => 9,
        _ => 10,
    }
}

fn sort_nodes(nodes: &mut [GraphNode]) {
    nodes.sort_by(compare_node_order);
}

fn sort_edges(edges: &mut [GraphEdge]) {
    edges.sort_by(|a, b| {
        (
            a.from.as_str(),
            a.to.as_str(),
            a.kind.as_str(),
            a.id.as_str(),
        )
            .cmp(&(
                b.from.as_str(),
                b.to.as_str(),
                b.kind.as_str(),
                b.id.as_str(),
            ))
    });
}

fn sort_findings(findings: &mut [GraphFinding]) {
    findings.sort_by(|a, b| {
        (
            a.severity.as_str(),
            a.code.as_str(),
            a.target_id.as_deref().unwrap_or(""),
        )
            .cmp(&(
                b.severity.as_str(),
                b.code.as_str(),
                b.target_id.as_deref().unwrap_or(""),
            ))
    });
}

fn compute_stats(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    findings: &[GraphFinding],
) -> GraphStats {
    let modules = nodes.iter().filter(|n| n.kind == "module").count();
    let documents = nodes
        .iter()
        .filter(|n| n.column == GraphColumn::Documents && n.kind != "module")
        .count();
    let business_logic = nodes
        .iter()
        .filter(|n| n.column == GraphColumn::Business && n.kind != "module")
        .count();
    let code_symbols = nodes
        .iter()
        .filter(|n| n.column == GraphColumn::Code && n.kind != "file" && n.kind != "module")
        .count();
    let tests = nodes
        .iter()
        .filter(|n| n.column == GraphColumn::Tests && n.kind != "module" && n.kind != "file")
        .count();
    let confirmed_edges = edges
        .iter()
        .filter(|e| e.layer == GraphLayer::Confirmed)
        .count();
    let candidate_edges = edges
        .iter()
        .filter(|e| e.layer == GraphLayer::Candidate)
        .count();
    let risks = findings
        .iter()
        .filter(|f| matches!(f.severity.as_str(), "warning" | "error"))
        .count();
    let default_visible = nodes.iter().filter(|n| n.default_visible).count();
    GraphStats {
        modules,
        documents,
        business_logic,
        code_symbols,
        tests,
        confirmed_edges,
        candidate_edges,
        risks,
        default_visible,
    }
}

// ---------------------------------------------------------------------------
// Config plumbing — local to keep `graph` self-contained.
// ---------------------------------------------------------------------------

fn load_config(repo_root: &Path) -> Result<EngineConfig> {
    let path = repo_root.join(DEFAULT_CONFIG_FILE_NAME);
    if !path.exists() {
        anyhow::bail!(
            "no SpecSlice workspace at {}: run `specslice init` first",
            repo_root.display()
        );
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let cfg: EngineConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("parsing config {}", path.display()))?;
    Ok(cfg)
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = Path::new(&config.storage.path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_dir_handles_root_and_nested() {
        assert_eq!(parent_dir("a.txt"), "");
        assert_eq!(parent_dir("a/b.txt"), "a");
        assert_eq!(parent_dir("a/b/c.txt"), "a/b");
    }

    #[test]
    fn ancestor_dirs_emits_full_chain() {
        assert_eq!(
            ancestor_dirs("a/b/c"),
            vec!["a".to_string(), "a/b".into(), "a/b/c".into()]
        );
        assert_eq!(ancestor_dirs(""), vec![String::new()]);
    }

    #[test]
    fn column_for_dir_uses_top_level_segment() {
        assert_eq!(column_for_dir("docs/api"), GraphColumn::Documents);
        assert_eq!(column_for_dir("test/integration"), GraphColumn::Tests);
        assert_eq!(column_for_dir("lib/features"), GraphColumn::Code);
        assert_eq!(column_for_dir(""), GraphColumn::Code);
    }
}
