//! P6: read-only `GraphViewModel` for SpecSlice visualisation.
//!
//! This module is the stable contract between the SpecSlice graph store and
//! any renderer (CLI JSON, self-contained HTML, Mermaid, future TUIs). It
//! never writes to the store, never touches business code, docs, or tests,
//! and always returns a deterministic, sortable view.
//!
//! The view model captures four orthogonal layers that the UI distinguishes:
//!
//! | Layer       | Source                                        |
//! |-------------|-----------------------------------------------|
//! | `fact`      | Filesystem / parser / docs indexer            |
//! | `confirmed` | External manifest links (`.specslice/links.yaml`)|
//! | `candidate` | Future `.specslice/candidates/` store         |
//! | `risk`      | Check / impact findings                       |
//!
//! Focus and `max_nodes` truncation are applied **after** the raw view is
//! collected so that filtering is testable in isolation.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{EdgeAssertion, EdgeCertainty, EdgeSource, EdgeStatus, Node, NodeKind};
use specslice_store::Store;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::checks::{compute_checks_with_policy, CheckFinding, CheckPolicy, CheckSeverity};
use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};

/// Bump this whenever the JSON shape changes in a way readers must observe.
pub const GRAPH_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Public data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphViewModel {
    pub schema_version: u32,
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
    pub documents: usize,
    pub business_logic: usize,
    pub code_symbols: usize,
    pub tests: usize,
    pub confirmed_edges: usize,
    pub candidate_edges: usize,
    pub risks: usize,
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

#[derive(Debug, Clone, Default)]
pub struct GraphOptions {
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

    let mut findings: Vec<GraphFinding> = Vec::new();
    if options.include_risks {
        let report = compute_checks_with_policy(&store, None, CheckPolicy::from(&config.checks))
            .context("computing checks for graph view")?;
        findings.extend(report.findings.iter().map(map_check));
    }
    // Candidate overlay: not implemented yet. The placeholder must stay stable
    // so CLI consumers can call `--include-candidates` today without errors.
    let _ = options.include_candidates;

    // Focus filtering.
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

    // max_nodes truncation.
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

    sort_nodes(&mut nodes);
    sort_edges(&mut edges);
    sort_findings(&mut findings);

    let stats = compute_stats(&nodes, &edges, &findings);
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into());

    Ok(GraphViewModel {
        schema_version: GRAPH_SCHEMA_VERSION,
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
    if matches!(edge.source, EdgeSource::ExternalManifest) {
        GraphLayer::Confirmed
    } else if matches!(edge.certainty, EdgeCertainty::Fact) {
        GraphLayer::Fact
    } else {
        // Declared but not external manifest — keep on Fact layer for now.
        GraphLayer::Fact
    }
}

// ---------------------------------------------------------------------------
// Focus + truncation helpers
// ---------------------------------------------------------------------------

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

/// Deterministic priority list used by `max_nodes` truncation.
///
/// 1. Focus node (if any)
/// 2. Confirmed business nodes
/// 3. Direct neighbours of confirmed business nodes
/// 4. Remaining nodes by (column, path, start_line, id)
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
        a.path.as_deref().unwrap_or(""),
        a.line_range.map(|(s, _)| s).unwrap_or(0),
        a.label.as_str(),
        a.id.as_str(),
    );
    let key_b = (
        column_rank(b.column),
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
    let documents = nodes
        .iter()
        .filter(|n| n.column == GraphColumn::Documents)
        .count();
    let business_logic = nodes
        .iter()
        .filter(|n| n.column == GraphColumn::Business)
        .count();
    let code_symbols = nodes
        .iter()
        .filter(|n| n.column == GraphColumn::Code && n.kind != "file")
        .count();
    let tests = nodes
        .iter()
        .filter(|n| n.column == GraphColumn::Tests)
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
    GraphStats {
        documents,
        business_logic,
        code_symbols,
        tests,
        confirmed_edges,
        candidate_edges,
        risks,
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
