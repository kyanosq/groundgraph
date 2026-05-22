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

use crate::business_candidates::{
    candidate_artifact_id, load_business_candidates, BusinessCandidate, ReviewStatus,
};
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
    // ---- P6.3 evidence -----------------------------------------------------
    /// Repo-relative path of the file the edge was extracted from. Surfaces
    /// for any edge whose parser/indexer recorded a file (today: every
    /// Dart `calls` / `references` edge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    /// `(start_line, end_line)` 1-based; equal values mean a single line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<(u32, u32)>,
    /// Trimmed copy of the source line (≤ 200 chars). Lets users judge
    /// whether the heuristic guessed correctly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Which analyser produced this edge. `dart_lightweight` for the
    /// current heuristic Dart scanner; `dart_analyzer` after the analyzer
    /// sidecar lands; `ai_candidate` for AI-derived edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolver: Option<String>,
    /// P19 — derived three-tier evidence quality: `"high"` /
    /// `"medium"` / `"low"`. Computed at read time from
    /// `(kind, source, certainty, status, indexer)`. Cheap for
    /// consumers (humans + AI) to filter on without re-parsing
    /// the provenance tuple.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_quality: Option<String>,
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

#[derive(Debug, Clone)]
pub struct GraphOptions {
    pub view: GraphView,
    pub focus: Option<String>,
    pub include_risks: bool,
    /// When `true` (the default), P9 business candidates from
    /// `.specslice/candidates/business_logic.yaml` are merged into the
    /// view as `business_candidate` nodes on the Candidate layer. Set
    /// to `false` to render the pure code-fact graph.
    pub include_candidates: bool,
    pub max_nodes: Option<usize>,
    /// When `false` (default) the view drops framework-noise edges:
    /// `toString`, `copyWith`, `dispose`, `initState`, `build`, etc.
    /// The Dart adapter avoids emitting them in the first place, but the
    /// engine also filters defensively at view time so older `.specslice`
    /// stores stay clean.
    pub include_noise: bool,
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self {
            view: GraphView::default(),
            focus: None,
            include_risks: false,
            include_candidates: true,
            max_nodes: None,
            include_noise: false,
        }
    }
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

    // 1b. Default noise filter — drop `calls` edges targeting framework
    // helper methods (toString / dispose / initState / …) so the focus
    // view does not get drowned in same-name calls. Users can opt back in
    // via `include_noise = true`.
    if !options.include_noise {
        edges.retain(|e| !is_noise_edge(e));
    }

    // 2. Inject synthetic module aggregations from file paths.
    inject_modules(&mut nodes);

    // 3. Wire parent_id chains: symbol → file → module.
    wire_parents(&mut nodes);

    // 4. Compute child_count for every aggregator.
    compute_child_counts(&mut nodes);

    // 5. Risk findings.
    let mut findings: Vec<GraphFinding> = Vec::new();
    if options.include_risks {
        let report = compute_checks_with_policy(&store, None, CheckPolicy::from(&config.checks))
            .context("computing checks for graph view")?;
        findings.extend(report.findings.iter().map(map_check));
    }

    // 5b. P9 — merge AI-authored business candidates from
    // `.specslice/candidates/business_logic.yaml`. Read-only: the engine
    // never writes the YAML; if it's malformed we still produce a
    // graph and surface the parser warnings as findings so the user can
    // act on them.
    if options.include_candidates {
        match load_business_candidates(repo_root) {
            Ok(outcome) => {
                let node_ids: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
                for c in &outcome.document.candidates {
                    merge_business_candidate(c, &node_ids, &mut nodes, &mut edges, &mut findings);
                }
                for w in outcome.warnings {
                    findings.push(GraphFinding {
                        code: "business_candidate_warning".into(),
                        severity: "warning".into(),
                        message: w,
                        target_id: None,
                    });
                }
            }
            Err(e) => {
                findings.push(GraphFinding {
                    code: "business_candidate_load_failed".into(),
                    severity: "warning".into(),
                    message: format!("failed to load business candidates: {e}"),
                    target_id: None,
                });
            }
        }
    }

    // 6. Focus narrowing — applied before view filter so visibility is sane.
    if let Some(focus_raw) = options.focus.as_deref() {
        match resolve_focus(&nodes, focus_raw) {
            Some(target_id) => {
                let kept = focus_subgraph(&nodes, &edges, &target_id);
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
    if matches!(options.view, GraphView::Business) && should_emit_no_business_logic_finding(&nodes)
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

fn should_emit_no_business_logic_finding(nodes: &[GraphNode]) -> bool {
    !nodes.iter().any(|n| {
        n.default_visible && matches!(n.kind.as_str(), "requirement" | "business_candidate")
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
    let (line_range, snippet, resolver) = parse_reference_evidence(edge.evidence_json.as_deref());
    let evidence_quality = Some(
        crate::edge_confidence::confidence_for_edge(edge)
            .as_str()
            .to_string(),
    );
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
        source_file: edge.source_file.clone(),
        line_range,
        snippet,
        resolver,
        evidence_quality,
    }
}

/// Names whose `calls` edges are almost always framework noise.
/// Mirrors `FRAMEWORK_NOISE_METHODS` in the Dart adapter — the engine
/// re-applies the filter at view time so existing stores from before
/// P6.3 also benefit without a reindex.
const NOISE_TARGET_METHODS: &[&str] = &[
    "toString",
    "hashCode",
    "noSuchMethod",
    "runtimeType",
    "copyWith",
    "dispose",
    "initState",
    "build",
    "didChangeDependencies",
    "didUpdateWidget",
    "deactivate",
    "reassemble",
    "createState",
    "createElement",
];

/// True when an edge points at a known noise method. We only filter
/// `calls` so `references` (class / constant uses) and structural edges
/// (`contains`, `imports`) are unaffected.
fn is_noise_edge(edge: &GraphEdge) -> bool {
    if edge.kind != "calls" {
        return false;
    }
    let target_name = target_method_name(&edge.to);
    NOISE_TARGET_METHODS.contains(&target_name.as_str())
}

/// Strip the `dart_method::path#Class.method` prefix down to `method`.
/// Returns the empty string for non-symbol ids.
fn target_method_name(id: &str) -> String {
    let Some((_, tail)) = id.split_once('#') else {
        return String::new();
    };
    match tail.rsplit_once('.') {
        Some((_, method)) => method.into(),
        None => tail.into(),
    }
}

/// Parse the tiny `{"line":N,"snippet":"…","resolver":"…"}` envelope the
/// Dart indexer writes onto `calls` / `references` edges. Returns `None`
/// for any field that is missing or malformed so the UI can show `--`.
fn parse_reference_evidence(
    evidence_json: Option<&str>,
) -> (Option<(u32, u32)>, Option<String>, Option<String>) {
    let Some(raw) = evidence_json else {
        return (None, None, None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (None, None, None);
    };
    let obj = match value {
        serde_json::Value::Object(m) => m,
        _ => return (None, None, None),
    };
    let line = obj
        .get("line")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
        .and_then(|n| u32::try_from(n).ok());
    let line_range = line.map(|n| (n, n));
    let snippet = obj
        .get("snippet")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let resolver = obj
        .get("resolver")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    (line_range, snippet, resolver)
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

/// P9 — merge one AI-authored candidate into the in-memory graph.
///
/// Layer / status is now derived from the candidate's `review_status()`:
///
/// | review_status        | layer    | status    | confidence | source            |
/// |----------------------|----------|-----------|------------|-------------------|
/// | Accepted             | Fact     | Confirmed | 1.0        | human_confirmed   |
/// | Rejected             | Candidate| Rejected  | as-is      | ai_candidate      |
/// | NeedsChanges/Pending | Candidate| Proposed  | as-is      | ai_candidate      |
/// | None (un-reviewed)   | Candidate| Proposed  | as-is      | ai_candidate      |
///
/// Accepted candidates with broken evidence (any id in `evidence` not in
/// the graph) get demoted to `GraphStatus::Stale` so the UI surfaces the
/// drift instead of silently claiming confidence.
fn merge_business_candidate(
    c: &BusinessCandidate,
    known_node_ids: &HashSet<String>,
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    findings: &mut Vec<GraphFinding>,
) {
    let candidate_id = candidate_artifact_id(&c.id);
    let review = c.review_status();
    let unresolved_evidence: Vec<&str> = c
        .evidence
        .iter()
        .filter(|ev| !known_node_ids.contains(ev.as_str()))
        .map(String::as_str)
        .collect();
    // Accepted candidates inherit a special "confirmed" rendering — but
    // only when every evidence id still resolves. Empty-evidence or
    // dangling-evidence accepts fall back to Stale so the reviewer
    // notices.
    let (layer, status, confidence, source) = match review {
        Some(ReviewStatus::Accepted)
            if !c.evidence.is_empty() && unresolved_evidence.is_empty() =>
        {
            (
                GraphLayer::Fact,
                GraphStatus::Confirmed,
                1.0_f32,
                "human_confirmed",
            )
        }
        Some(ReviewStatus::Accepted) => (
            GraphLayer::Fact,
            GraphStatus::Stale,
            c.confidence.unwrap_or(0.5).clamp(0.0, 1.0),
            "human_confirmed",
        ),
        Some(ReviewStatus::Rejected) => (
            GraphLayer::Candidate,
            GraphStatus::Rejected,
            c.confidence.unwrap_or(0.5).clamp(0.0, 1.0),
            "ai_candidate",
        ),
        Some(ReviewStatus::NeedsChanges) | Some(ReviewStatus::Pending) | None => (
            GraphLayer::Candidate,
            GraphStatus::Proposed,
            c.confidence.unwrap_or(0.5).clamp(0.0, 1.0),
            "ai_candidate",
        ),
    };

    let mut badges = Vec::new();
    let review_label = match review {
        Some(ReviewStatus::Accepted) => "accepted",
        Some(ReviewStatus::Rejected) => "rejected",
        Some(ReviewStatus::NeedsChanges) => "needs_changes",
        Some(ReviewStatus::Pending) => "pending",
        None => c.status.as_str(),
    };
    badges.push(format!("ai_candidate status={review_label}"));
    if !c.open_questions.is_empty() {
        badges.push(format!("{} open questions", c.open_questions.len()));
    }
    if !c.description.trim().is_empty() {
        badges.push(format!("desc: {}", shorten_for_badge(c.description.trim())));
    }
    for q in &c.open_questions {
        badges.push(format!("Q: {}", shorten_for_badge(q)));
    }

    nodes.push(GraphNode {
        id: candidate_id.clone(),
        kind: "business_candidate".into(),
        column: GraphColumn::Business,
        layer,
        label: c.name.clone(),
        path: None,
        line_range: None,
        status,
        parent_id: None,
        child_count: 0,
        // Accepted candidates ride into the default visible set so the
        // confirmed graph view actually surfaces them without
        // explicit focus.
        default_visible: matches!(status, GraphStatus::Confirmed),
        confidence: Some(confidence),
        source: Some(source.into()),
        badges,
    });

    for (idx, ev) in c.evidence.iter().enumerate() {
        if !known_node_ids.contains(ev) {
            findings.push(GraphFinding {
                code: "business_candidate_dangling_evidence".into(),
                severity: "warning".into(),
                message: format!(
                    "candidate `{}` cites evidence `{}` but no such node exists in the graph",
                    c.id, ev
                ),
                target_id: Some(candidate_id.clone()),
            });
            continue;
        }
        edges.push(GraphEdge {
            id: format!("derives_from::{}#{idx}", c.id),
            from: candidate_id.clone(),
            to: ev.clone(),
            kind: "derives_from".into(),
            layer,
            status,
            confidence: Some(confidence),
            source: Some(source.into()),
            rationale: None,
            source_file: None,
            line_range: None,
            snippet: None,
            resolver: Some(source.into()),
            // `derives_from` edges are AI-authored business
            // candidate evidence — always low until the candidate
            // is confirmed by a human.
            evidence_quality: Some("low".into()),
        });
    }
}

/// Trim a description / open-question down to a 120-char preview so it
/// can ride along on a node badge without blowing up the UI. Word-aware
/// truncation falls back to a hard cut for pathological inputs.
fn shorten_for_badge(s: &str) -> String {
    const LIMIT: usize = 120;
    if s.chars().count() <= LIMIT {
        return s.replace('\n', " ");
    }
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= LIMIT - 1 {
            break;
        }
        if ch == '\n' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out.push('…');
    out
}

fn column_for(kind: NodeKind) -> GraphColumn {
    match kind {
        NodeKind::DocSection | NodeKind::AcceptanceCriterion | NodeKind::Adr => {
            GraphColumn::Documents
        }
        NodeKind::Requirement | NodeKind::BusinessCandidate => GraphColumn::Business,
        NodeKind::File
        | NodeKind::DartClass
        | NodeKind::DartMethod
        | NodeKind::DartFunction
        | NodeKind::DartConstructor
        | NodeKind::DartProvider
        | NodeKind::Route
        | NodeKind::Storage
        | NodeKind::SwiftClass
        | NodeKind::SwiftStruct
        | NodeKind::SwiftEnum
        | NodeKind::SwiftProtocol
        | NodeKind::SwiftMethod
        | NodeKind::SwiftFunction
        | NodeKind::SwiftInitializer
        | NodeKind::GoStruct
        | NodeKind::GoInterface
        | NodeKind::GoMethod
        | NodeKind::GoFunction
        | NodeKind::PythonModule
        | NodeKind::PythonClass
        | NodeKind::PythonFunction
        | NodeKind::PythonMethod => GraphColumn::Code,
        NodeKind::TestCase | NodeKind::TestGroup => GraphColumn::Tests,
    }
}

fn layer_for_node(kind: NodeKind) -> GraphLayer {
    match kind {
        NodeKind::Requirement => GraphLayer::Confirmed,
        NodeKind::BusinessCandidate => GraphLayer::Candidate,
        _ => GraphLayer::Fact,
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
    // When `--focus` is set we already narrowed the model to a useful
    // subgraph in [`focus_subgraph`]. Independently of the requested view,
    // mark every survivor as default-visible so the UI does not collapse
    // back to "only top-level modules".
    if focus_raw.is_some() {
        for n in nodes.iter_mut() {
            n.default_visible = true;
        }
        return;
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
            // Identify requirements *and* P9 business candidates as the
            // "business surface"; pull in their one-hop neighbours so the
            // user sees the immediate code/doc evidence supporting them.
            let business_ids: HashSet<String> = nodes
                .iter()
                .filter(|n| n.kind == "requirement" || n.kind == "business_candidate")
                .map(|n| n.id.clone())
                .collect();
            let mut visible: HashSet<String> = business_ids.clone();
            for edge in edges {
                if business_ids.contains(&edge.from) {
                    visible.insert(edge.to.clone());
                }
                if business_ids.contains(&edge.to) {
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

/// The focus subgraph is what the UI shows when `--focus` is set.
///
/// For aggregator-style focus targets (module / file / class), returning
/// only the focus node + immediate edge neighbours is useless: a module has
/// no outgoing edges, so the focus collapses to a single button. Instead
/// we expand to:
///
/// 1. The focus node itself.
/// 2. Every transitive descendant via `parent_id` *and* via outgoing
///    `contains` edges. `parent_id` covers module → file containment;
///    `contains` covers class → method, since methods are parented under
///    the file (not the class) for layout reasons but are reachable from
///    the class via the parser-emitted `contains` edge.
/// 3. The one-hop edge neighbourhood of every node already in the set.
///
/// Parent chains of edge neighbours are intentionally not pulled in: that
/// keeps method-level focus tight (the chain "caller → method → referenced
/// class" stays visible without dragging the whole module tree along).
/// Walk `contains` edges and `parent_id` links from `target_id` to collect
/// every transitively-contained node (file → class → method → …). Used by
/// `priority_order` so descendants of the focus win truncation over
/// 1-hop neighbours.
fn focus_descendants(nodes: &[GraphNode], edges: &[GraphEdge], target_id: &str) -> HashSet<String> {
    let mut kept: HashSet<String> = HashSet::new();
    kept.insert(target_id.to_string());
    loop {
        let before = kept.len();
        let snapshot = kept.clone();
        for n in nodes {
            if let Some(p) = n.parent_id.as_deref() {
                if snapshot.contains(p) {
                    kept.insert(n.id.clone());
                }
            }
        }
        for e in edges {
            if e.kind == "contains" && snapshot.contains(&e.from) {
                kept.insert(e.to.clone());
            }
        }
        if kept.len() == before {
            break;
        }
    }
    kept.remove(target_id); // caller already counts the focus itself
    kept
}

fn focus_subgraph(nodes: &[GraphNode], edges: &[GraphEdge], target_id: &str) -> HashSet<String> {
    let valid: BTreeSet<String> = nodes.iter().map(|n| n.id.clone()).collect();

    let mut kept: HashSet<String> = HashSet::new();
    kept.insert(target_id.to_string());

    loop {
        let before = kept.len();
        let snapshot: HashSet<String> = kept.clone();
        // parent_id descendants (module → file)
        for n in nodes {
            if kept.contains(n.id.as_str()) {
                continue;
            }
            if let Some(p) = n.parent_id.as_deref() {
                if snapshot.contains(p) {
                    kept.insert(n.id.clone());
                }
            }
        }
        // contains-edge descendants (class → method)
        for e in edges {
            if e.kind == "contains" && snapshot.contains(&e.from) {
                kept.insert(e.to.clone());
            }
        }
        if kept.len() == before {
            break;
        }
    }

    let mut neighbours: Vec<String> = Vec::new();
    for e in edges {
        if kept.contains(&e.from) {
            neighbours.push(e.to.clone());
        }
        if kept.contains(&e.to) {
            neighbours.push(e.from.clone());
        }
    }
    for id in neighbours {
        kept.insert(id);
    }

    // Pull one extra hop of high-signal semantic context from the already
    // included neighbourhood. This keeps handler/listener focus useful in
    // Flutter apps: the focused callback may be referenced by `initState`,
    // while the actual stream/provider/storage/route edge is attached to that
    // one-hop caller. We intentionally do not follow generic calls/references
    // here; otherwise method focus can balloon into a whole module graph.
    let semantic_context_sources = kept.clone();
    for e in edges {
        if !is_focus_semantic_context_edge(&e.kind) {
            continue;
        }
        if semantic_context_sources.contains(&e.from) {
            kept.insert(e.to.clone());
        }
        if semantic_context_sources.contains(&e.to) {
            kept.insert(e.from.clone());
        }
    }

    kept.retain(|id| valid.contains(id));
    kept
}

fn is_focus_semantic_context_edge(kind: &str) -> bool {
    matches!(
        kind,
        "reads_provider" | "navigates_to" | "persists_to" | "subscribes_stream"
    )
}

fn priority_order(nodes: &[GraphNode], edges: &[GraphEdge], focus: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push = |id: &str, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        if seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    };

    // Tier 1 — the focus itself.
    let focus_id = focus.and_then(|f| resolve_focus(nodes, f));
    if let Some(ref resolved) = focus_id {
        push(resolved, &mut out, &mut seen);
    }

    // Tier 2 — descendants of the focus (via `contains` edges + parent_id).
    // These are the nodes the user almost certainly cares about: PaywallScreen
    // is a more useful neighbour than IapProductIds when you focused on the
    // paywall module.
    if let Some(ref resolved) = focus_id {
        let descendants = focus_descendants(nodes, edges, resolved);
        let mut ordered: Vec<&GraphNode> = nodes
            .iter()
            .filter(|n| descendants.contains(&n.id) && !seen.contains(&n.id))
            .collect();
        ordered.sort_by(|a, b| compare_business_priority(a, b).then(compare_node_order(a, b)));
        for n in &ordered {
            push(&n.id, &mut out, &mut seen);
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

    // P6.3: when nothing is confirmed (the common case for a freshly
    // indexed repo with no manifest yet), the "rest" bucket carries
    // everything. Truncation will then trim from the tail of `rest`, so
    // we must pull business-keyword nodes (Pay / Purchase / Subscription
    // / Entitlement / Iap / Pro / Premium / Restore) to the head and
    // push framework-noise method nodes (initState / dispose / build /
    // …) to the tail.
    let mut rest: Vec<&GraphNode> = nodes.iter().filter(|n| !seen.contains(&n.id)).collect();
    rest.sort_by(|a, b| compare_business_priority(a, b).then(compare_node_order(a, b)));
    for n in &rest {
        push(&n.id, &mut out, &mut seen);
    }

    out
}

/// Lower scores sort earlier. `0` = business / important, `1` = neutral,
/// `2` = framework noise. Pure heuristic, only used as a head-of-bucket
/// tiebreaker for truncation; the visible sort still uses
/// `compare_node_order`.
fn compare_business_priority(a: &GraphNode, b: &GraphNode) -> std::cmp::Ordering {
    business_rank(a).cmp(&business_rank(b))
}

fn business_rank(node: &GraphNode) -> u8 {
    // Noise demotion runs first: a method named `dispose` should sink to
    // the tail even when it lives in a business-named file. The noise
    // list is Dart-centric but the same names (`build` / `dispose` /
    // `toString`) recur in Swift / Go heuristics; sharing the rank
    // across languages keeps the graph readable.
    if matches!(
        node.kind.as_str(),
        "dart_method"
            | "dart_function"
            | "swift_method"
            | "swift_function"
            | "go_method"
            | "go_function"
    ) {
        let method_name = node.label.as_str();
        if NOISE_TARGET_METHODS.contains(&method_name) {
            return 2;
        }
    }
    let label_lower = node.label.to_ascii_lowercase();
    let path_lower = node.path.as_deref().unwrap_or("").to_ascii_lowercase();
    let is_business = BUSINESS_KEYWORDS
        .iter()
        .any(|kw| label_lower.contains(kw) || path_lower.contains(kw));
    if is_business {
        return 0;
    }
    1
}

/// Keywords used by [`business_rank`] to pull a node toward the head of
/// the truncation queue. Lower-cased substrings — match label or path.
const BUSINESS_KEYWORDS: &[&str] = &[
    "pay",
    "paywall",
    "purchase",
    "subscription",
    "subscribe",
    "entitlement",
    "iap",
    "billing",
    "checkout",
    "restore",
    "pro_",
    "premium",
];

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
        // Container-shaped declarations sort with `dart_class` so the
        // graph reader keeps "container first, members later" order
        // regardless of language.
        "dart_class" | "swift_class" | "swift_struct" | "swift_enum" | "swift_protocol"
        | "go_struct" | "go_interface" | "python_class" | "python_module" => 4,
        "dart_function" | "swift_function" | "go_function" | "python_function" => 5,
        "dart_method" | "swift_method" | "go_method" | "python_method" => 6,
        "dart_constructor" | "swift_initializer" => 7,
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

    fn fake_node(id: &str, kind: &str, parent: Option<&str>) -> GraphNode {
        GraphNode {
            id: id.into(),
            kind: kind.into(),
            column: GraphColumn::Code,
            layer: GraphLayer::Fact,
            label: id.into(),
            path: None,
            line_range: None,
            status: GraphStatus::Confirmed,
            parent_id: parent.map(|s| s.into()),
            child_count: 0,
            default_visible: false,
            confidence: None,
            source: None,
            badges: Vec::new(),
        }
    }

    fn fake_edge(from: &str, to: &str, kind: &str) -> GraphEdge {
        GraphEdge {
            id: format!("e::{from}::{kind}::{to}"),
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            layer: GraphLayer::Fact,
            status: GraphStatus::Confirmed,
            confidence: None,
            source: None,
            rationale: None,
            source_file: None,
            line_range: None,
            snippet: None,
            resolver: None,
            evidence_quality: None,
        }
    }

    #[test]
    fn focus_subgraph_follows_contains_edges_into_descendants() {
        // class → method via `contains`. Focus on the class must surface
        // the method even though parent_id points at the file, not the
        // class.
        let nodes = vec![
            fake_node("file::lib/a.dart", "file", None),
            fake_node(
                "dart_class::lib/a.dart#C",
                "dart_class",
                Some("file::lib/a.dart"),
            ),
            fake_node(
                "dart_method::lib/a.dart#C.go",
                "dart_method",
                Some("file::lib/a.dart"),
            ),
        ];
        let edges = vec![fake_edge(
            "dart_class::lib/a.dart#C",
            "dart_method::lib/a.dart#C.go",
            "contains",
        )];
        let kept = focus_subgraph(&nodes, &edges, "dart_class::lib/a.dart#C");
        assert!(kept.contains("dart_method::lib/a.dart#C.go"));
    }

    #[test]
    fn focus_subgraph_returns_empty_set_when_target_missing() {
        let nodes = vec![fake_node("only", "module", None)];
        let kept = focus_subgraph(&nodes, &[], "does_not_exist");
        // After `kept.retain(|id| valid.contains(id))`, the inserted bogus
        // target is dropped because it does not exist in `nodes`.
        assert!(kept.is_empty());
    }

    #[test]
    fn focus_subgraph_keeps_semantic_context_from_one_hop_callers() {
        let nodes = vec![
            fake_node(
                "dart_method::lib/paywall.dart#Screen.initState",
                "dart_method",
                None,
            ),
            fake_node(
                "dart_method::lib/paywall.dart#Screen.listenToPurchaseUpdates",
                "dart_method",
                None,
            ),
            fake_node("storage::stream::purchaseStream", "storage", None),
        ];
        let edges = vec![
            fake_edge(
                "dart_method::lib/paywall.dart#Screen.initState",
                "dart_method::lib/paywall.dart#Screen.listenToPurchaseUpdates",
                "references",
            ),
            fake_edge(
                "dart_method::lib/paywall.dart#Screen.initState",
                "storage::stream::purchaseStream",
                "subscribes_stream",
            ),
        ];
        let kept = focus_subgraph(
            &nodes,
            &edges,
            "dart_method::lib/paywall.dart#Screen.listenToPurchaseUpdates",
        );
        assert!(
            kept.contains("storage::stream::purchaseStream"),
            "focusing listener must keep the caller's stream subscription context: {kept:?}"
        );
    }

    #[test]
    fn resolve_focus_matches_by_id_prefix_badge_or_req_form() {
        let mut req_node = fake_node("req::FOO", "requirement", None);
        req_node.badges = vec!["badge-1".into()];
        let nodes = vec![fake_node("module::lib", "module", None), req_node];
        // Direct id match.
        assert_eq!(
            resolve_focus(&nodes, "module::lib"),
            Some("module::lib".into())
        );
        // `req::` synthesised prefix.
        assert_eq!(resolve_focus(&nodes, "FOO"), Some("req::FOO".into()));
        // module-path synthesis (e.g. `lib`).
        assert_eq!(resolve_focus(&nodes, "lib"), Some("module::lib".into()));
        // Badge match falls back last.
        assert_eq!(resolve_focus(&nodes, "badge-1"), Some("req::FOO".into()));
        // Empty input is rejected outright.
        assert_eq!(resolve_focus(&nodes, "  "), None);
        // Total miss.
        assert_eq!(resolve_focus(&nodes, "nope"), None);
    }

    #[test]
    fn apply_view_overview_marks_only_top_level_modules() {
        let mut nodes = vec![
            fake_node("module::lib", "module", None),
            fake_node("module::lib/iap", "module", Some("module::lib")),
            fake_node("file::lib/a.dart", "file", Some("module::lib")),
        ];
        apply_view(&mut nodes, &[], GraphView::Overview, None);
        assert!(nodes[0].default_visible);
        assert!(!nodes[1].default_visible);
        assert!(!nodes[2].default_visible);
    }

    #[test]
    fn apply_view_business_marks_requirement_neighbours() {
        let mut nodes = vec![
            fake_node("req::FOO", "requirement", None),
            fake_node("dart_class::lib/a.dart#C", "dart_class", None),
            fake_node("test_case::lib/a_test.dart#t", "test_case", None),
        ];
        let edges = vec![
            fake_edge(
                "req::FOO",
                "dart_class::lib/a.dart#C",
                "declares_implementation",
            ),
            fake_edge(
                "test_case::lib/a_test.dart#t",
                "req::FOO",
                "declares_verification",
            ),
        ];
        apply_view(&mut nodes, &edges, GraphView::Business, None);
        assert!(nodes.iter().all(|n| n.default_visible));
    }

    #[test]
    fn business_view_no_logic_finding_is_suppressed_when_candidates_exist() {
        let mut candidate = fake_node("business_candidate::a", "business_candidate", None);
        candidate.column = GraphColumn::Business;
        candidate.layer = GraphLayer::Candidate;
        candidate.default_visible = true;
        assert!(
            !should_emit_no_business_logic_finding(&[candidate]),
            "loaded candidates are a business surface; the UI should not ask users to seed candidates"
        );
    }

    #[test]
    fn apply_view_focus_without_id_falls_back_to_overview() {
        let mut nodes = vec![
            fake_node("module::lib", "module", None),
            fake_node("dart_class::lib/a.dart#C", "dart_class", None),
        ];
        apply_view(&mut nodes, &[], GraphView::Focus, None);
        // Without a focus string, focus mode is overview-equivalent.
        assert!(nodes[0].default_visible);
        assert!(!nodes[1].default_visible);
    }

    #[test]
    fn layer_for_edge_promotes_external_manifest_only() {
        use specslice_core::{ArtifactId, EdgeKind};
        let mut e = EdgeAssertion::fact(
            ArtifactId::new("a"),
            ArtifactId::new("b"),
            EdgeKind::Contains,
            EdgeSource::ExternalManifest,
        );
        assert!(matches!(layer_for_edge(&e), GraphLayer::Confirmed));
        e.source = EdgeSource::LanguageAdapter;
        assert!(matches!(layer_for_edge(&e), GraphLayer::Fact));
        e.source = EdgeSource::GitDiff;
        assert!(matches!(layer_for_edge(&e), GraphLayer::Fact));
    }

    #[test]
    fn column_for_handles_every_node_kind() {
        use specslice_core::NodeKind;
        assert_eq!(column_for(NodeKind::Adr), GraphColumn::Documents);
        assert_eq!(
            column_for(NodeKind::AcceptanceCriterion),
            GraphColumn::Documents
        );
        assert_eq!(column_for(NodeKind::Requirement), GraphColumn::Business);
        assert_eq!(column_for(NodeKind::File), GraphColumn::Code);
        assert_eq!(column_for(NodeKind::DartClass), GraphColumn::Code);
        assert_eq!(column_for(NodeKind::DartMethod), GraphColumn::Code);
        assert_eq!(column_for(NodeKind::DartFunction), GraphColumn::Code);
        assert_eq!(column_for(NodeKind::DartConstructor), GraphColumn::Code);
        assert_eq!(column_for(NodeKind::TestCase), GraphColumn::Tests);
        assert_eq!(column_for(NodeKind::TestGroup), GraphColumn::Tests);
    }

    #[test]
    fn kind_rank_falls_back_to_default_bucket() {
        assert!(kind_rank("module") < kind_rank("file"));
        assert!(kind_rank("file") < kind_rank("dart_class"));
        assert!(kind_rank("dart_class") < kind_rank("dart_method"));
        assert!(kind_rank("test_case") > kind_rank("test_group"));
        assert_eq!(kind_rank("alien"), 10);
    }

    fn confirmed(id: &str, kind: &str) -> GraphNode {
        let mut n = fake_node(id, kind, None);
        n.layer = GraphLayer::Confirmed;
        n
    }

    #[test]
    fn priority_order_pulls_focus_then_confirmed_then_neighbours_then_rest() {
        // Focus + confirmed + neighbours + rest, all distinct buckets.
        let req = confirmed("req::A", "requirement");
        let neighbour = fake_node("dart_class::lib/x.dart#X", "dart_class", None);
        let rest = fake_node("module::lib", "module", None);
        let nodes = vec![req.clone(), neighbour.clone(), rest.clone()];
        let edges = vec![fake_edge(
            "req::A",
            "dart_class::lib/x.dart#X",
            "declares_implementation",
        )];
        // No focus → confirmed comes first, neighbour second, rest last.
        let order = priority_order(&nodes, &edges, None);
        assert_eq!(order[0], "req::A");
        assert_eq!(order[1], "dart_class::lib/x.dart#X");
        assert_eq!(order[2], "module::lib");

        // With focus → focused id leads, confirmed/neighbour follow.
        let order2 = priority_order(&nodes, &edges, Some("dart_class::lib/x.dart#X"));
        assert_eq!(order2[0], "dart_class::lib/x.dart#X");
        assert!(order2.contains(&"req::A".to_string()));
        assert!(order2.contains(&"module::lib".to_string()));
    }

    #[test]
    fn compute_stats_counts_each_column_correctly() {
        let mut requirement = fake_node("req::A", "requirement", None);
        requirement.column = GraphColumn::Business;
        requirement.default_visible = true;
        let mut doc = fake_node("doc::A", "doc_section", None);
        doc.column = GraphColumn::Documents;
        let mut file = fake_node("file::a.dart", "file", None);
        file.column = GraphColumn::Code;
        let mut method = fake_node("dart_method::a.dart#C.go", "dart_method", None);
        method.column = GraphColumn::Code;
        let mut test_case = fake_node("test_case::t", "test_case", None);
        test_case.column = GraphColumn::Tests;
        let mut module_node = fake_node("module::lib", "module", None);
        module_node.column = GraphColumn::Code;
        let nodes = vec![requirement, doc, file, method, test_case, module_node];
        let mut confirmed_edge = fake_edge("a", "b", "contains");
        confirmed_edge.layer = GraphLayer::Confirmed;
        let mut candidate_edge = fake_edge("c", "d", "documents");
        candidate_edge.layer = GraphLayer::Candidate;
        let edges = vec![confirmed_edge, candidate_edge];
        let findings = vec![
            GraphFinding {
                code: "warn-1".into(),
                severity: "warning".into(),
                message: "w".into(),
                target_id: None,
            },
            GraphFinding {
                code: "err-1".into(),
                severity: "error".into(),
                message: "e".into(),
                target_id: None,
            },
            GraphFinding {
                code: "info-1".into(),
                severity: "info".into(),
                message: "i".into(),
                target_id: None,
            },
        ];
        let stats = compute_stats(&nodes, &edges, &findings);
        assert_eq!(stats.modules, 1);
        assert_eq!(stats.documents, 1);
        assert_eq!(stats.business_logic, 1);
        assert_eq!(
            stats.code_symbols, 1,
            "file & module excluded from code_symbols"
        );
        assert_eq!(stats.tests, 1);
        assert_eq!(stats.confirmed_edges, 1);
        assert_eq!(stats.candidate_edges, 1);
        assert_eq!(stats.risks, 2, "warning + error counted, info excluded");
        assert_eq!(stats.default_visible, 1);
    }

    #[test]
    fn sort_findings_orders_by_severity_then_code_then_target() {
        let mut findings = vec![
            GraphFinding {
                code: "z".into(),
                severity: "error".into(),
                message: "".into(),
                target_id: Some("t2".into()),
            },
            GraphFinding {
                code: "a".into(),
                severity: "info".into(),
                message: "".into(),
                target_id: None,
            },
            GraphFinding {
                code: "a".into(),
                severity: "warning".into(),
                message: "".into(),
                target_id: Some("t1".into()),
            },
        ];
        sort_findings(&mut findings);
        // severities sort lexicographically: error < info < warning
        assert_eq!(findings[0].severity, "error");
        assert_eq!(findings[1].severity, "info");
        assert_eq!(findings[2].severity, "warning");
    }

    #[test]
    fn load_config_returns_clear_error_when_config_is_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = load_config(tmp.path()).expect_err("missing config must error");
        let msg = format!("{err}");
        assert!(msg.contains("no SpecSlice workspace"), "{msg}");
    }

    #[test]
    fn resolve_storage_path_prefers_absolute_path() {
        let mut cfg = EngineConfig::default();
        cfg.storage.path = "/tmp/abs/graph.db".into();
        let path = resolve_storage_path(Path::new("/repo"), &cfg);
        assert_eq!(path, PathBuf::from("/tmp/abs/graph.db"));
    }

    #[test]
    fn resolve_storage_path_joins_relative_path_against_repo_root() {
        let mut cfg = EngineConfig::default();
        cfg.storage.path = "graph.db".into();
        let path = resolve_storage_path(Path::new("/repo"), &cfg);
        assert_eq!(path, PathBuf::from("/repo/graph.db"));
    }

    // -----------------------------------------------------------------
    // P6.3 unit coverage — evidence, noise, business priority.
    // -----------------------------------------------------------------

    #[test]
    fn p63_target_method_name_extracts_trailing_segment() {
        assert_eq!(target_method_name("dart_method::lib/a.dart#Foo.bar"), "bar");
        assert_eq!(target_method_name("dart_class::lib/a.dart#Foo"), "Foo");
        // Non-symbol id (no `#`) → empty.
        assert_eq!(target_method_name("module::lib/iap"), "");
    }

    #[test]
    fn p63_is_noise_edge_only_filters_calls() {
        // `calls` to `toString` is noise.
        let noisy = GraphEdge {
            id: "e".into(),
            from: "dart_method::lib/x.dart#A.go".into(),
            to: "dart_method::lib/x.dart#B.toString".into(),
            kind: "calls".into(),
            layer: GraphLayer::Fact,
            status: GraphStatus::Confirmed,
            confidence: None,
            source: None,
            rationale: None,
            source_file: None,
            line_range: None,
            snippet: None,
            resolver: None,
            evidence_quality: None,
        };
        assert!(is_noise_edge(&noisy));

        // `references` is never filtered, even to a noise target.
        let mut refs = noisy.clone();
        refs.kind = "references".into();
        assert!(!is_noise_edge(&refs));

        // A `calls` edge to a non-noise method survives.
        let mut real = noisy.clone();
        real.to = "dart_method::lib/x.dart#B.applyPurchase".into();
        assert!(!is_noise_edge(&real));
    }

    #[test]
    fn p63_parse_reference_evidence_extracts_line_and_resolver() {
        let raw = r#"{"line":42,"snippet":"a.b();","resolver":"dart_lightweight"}"#;
        let (lr, sn, res) = parse_reference_evidence(Some(raw));
        assert_eq!(lr, Some((42, 42)));
        assert_eq!(sn.as_deref(), Some("a.b();"));
        assert_eq!(res.as_deref(), Some("dart_lightweight"));
    }

    #[test]
    fn p63_parse_reference_evidence_returns_all_none_on_missing_input() {
        let (lr, sn, res) = parse_reference_evidence(None);
        assert!(lr.is_none() && sn.is_none() && res.is_none());
    }

    #[test]
    fn p63_parse_reference_evidence_rejects_invalid_json_safely() {
        let (lr, sn, res) = parse_reference_evidence(Some("{not json"));
        assert!(lr.is_none() && sn.is_none() && res.is_none());
    }

    #[test]
    fn p63_parse_reference_evidence_rejects_non_object_payload() {
        // JSON array — well-formed but wrong shape. Must not panic.
        let (lr, sn, res) = parse_reference_evidence(Some("[1,2,3]"));
        assert!(lr.is_none() && sn.is_none() && res.is_none());
    }

    #[test]
    fn p63_parse_reference_evidence_drops_zero_or_oversized_line() {
        // Zero is not a real line number.
        let (lr, _, _) = parse_reference_evidence(Some(r#"{"line":0}"#));
        assert!(lr.is_none());
        // u64::MAX cannot fit u32.
        let (lr, _, _) = parse_reference_evidence(Some(r#"{"line":18446744073709551615}"#));
        assert!(lr.is_none());
    }

    #[test]
    fn p63_business_rank_demotes_noise_methods_even_in_business_paths() {
        // `dispose` lives under lib/features/paywall but is still noise.
        let dispose = GraphNode {
            id: "dart_method::lib/features/paywall/paywall_screen.dart#PaywallScreen.dispose"
                .into(),
            kind: "dart_method".into(),
            column: GraphColumn::Code,
            layer: GraphLayer::Fact,
            label: "dispose".into(),
            path: Some("lib/features/paywall/paywall_screen.dart".into()),
            line_range: None,
            status: GraphStatus::Confirmed,
            parent_id: None,
            child_count: 0,
            default_visible: false,
            confidence: None,
            source: None,
            badges: vec![],
        };
        assert_eq!(business_rank(&dispose), 2);

        // `listenToPurchaseUpdates` matches "purchase" — rank 0.
        let listener = GraphNode {
            label: "listenToPurchaseUpdates".into(),
            ..dispose.clone()
        };
        assert_eq!(business_rank(&listener), 0);

        // Plain helper with no keyword and no noise name — neutral rank.
        let neutral = GraphNode {
            label: "encodeFooBar".into(),
            path: Some("lib/utils/codec.dart".into()),
            ..dispose
        };
        assert_eq!(business_rank(&neutral), 1);
    }

    #[test]
    fn p63_focus_descendants_walks_contains_and_parent_id() {
        let nodes = vec![
            fake_node("module::lib", "module", None),
            fake_node("file::lib/a.dart", "file", Some("module::lib")),
            fake_node(
                "dart_class::lib/a.dart#C",
                "dart_class",
                Some("file::lib/a.dart"),
            ),
            fake_node(
                "dart_method::lib/a.dart#C.m",
                "dart_method",
                Some("file::lib/a.dart"),
            ),
            // Outside the focus subtree:
            fake_node("file::other.dart", "file", None),
        ];
        let edges = vec![fake_edge(
            "dart_class::lib/a.dart#C",
            "dart_method::lib/a.dart#C.m",
            "contains",
        )];
        let descendants = focus_descendants(&nodes, &edges, "module::lib");
        assert!(descendants.contains("file::lib/a.dart"));
        assert!(descendants.contains("dart_class::lib/a.dart#C"));
        assert!(descendants.contains("dart_method::lib/a.dart#C.m"));
        assert!(!descendants.contains("module::lib")); // focus itself is excluded
        assert!(!descendants.contains("file::other.dart"));
    }

    #[test]
    fn p63_map_edge_surfaces_evidence_fields_on_graph_edge() {
        use specslice_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource};
        let mut edge = EdgeAssertion::fact(
            ArtifactId::new("dart_method::lib/a.dart#A.x"),
            ArtifactId::new("dart_method::lib/b.dart#B.y"),
            EdgeKind::Calls,
            EdgeSource::LanguageAdapter,
        );
        edge.source_file = Some("lib/a.dart".into());
        edge.evidence_json =
            Some(r#"{"line":11,"snippet":"y();","resolver":"dart_lightweight"}"#.into());

        let g = map_edge(&edge);
        assert_eq!(g.source_file.as_deref(), Some("lib/a.dart"));
        assert_eq!(g.line_range, Some((11, 11)));
        assert_eq!(g.snippet.as_deref(), Some("y();"));
        assert_eq!(g.resolver.as_deref(), Some("dart_lightweight"));
        assert_eq!(g.kind, "calls");
    }
}
