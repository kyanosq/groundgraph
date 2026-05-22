//! P19 — `specslice graph-diff`: compare two SpecSlice graph
//! snapshots and report what changed.
//!
//! The MVP works on two `.specslice/graph.db` files (typically
//! `--base-db path/to/old/graph.db` and `--head-db path/to/new/graph.db`).
//! We compare:
//!
//! - Nodes by stable id: added / removed / kind changed.
//! - Edges by stable id: added / removed / status changed
//!   (e.g. a `confirmed` edge becoming `deprecated`).
//! - Business candidates: added / removed / status changed. Reviewers
//!   need a way to see which AI candidates are new in a PR; the
//!   candidates live in `.specslice/candidates/business_logic.yaml`
//!   (never persisted to the SQLite store) so the diff only happens
//!   when both `base_repo_root` and `head_repo_root` are supplied. If
//!   either is missing we keep the three `candidates_*` vectors empty
//!   and the stats at zero — older `--base-db only` invocations stay
//!   bit-for-bit identical.
//!
//! We deliberately don't reindex anything — callers are expected
//! to keep the per-commit graph databases somewhere (CI artefact
//! storage / worktree). For a "compare HEAD~1..HEAD without
//! pre-built graphs" workflow we'd need to drive `index_repository`
//! against a `git worktree`, which is a much bigger change; deferred.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{EdgeAssertion, Node};
use specslice_store::Store;

use crate::business_candidates::{
    candidate_artifact_id, load_business_candidates, BusinessCandidate, ReviewStatus,
};

pub const GRAPH_DIFF_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default)]
pub struct GraphDiffOptions {
    pub base_db: PathBuf,
    pub head_db: PathBuf,
    /// Optional repo root for the *base* snapshot. When supplied (and
    /// the matching `head_repo_root` is too), `graph-diff` loads each
    /// repo's `.specslice/candidates/business_logic.yaml` and reports
    /// candidate-level changes. When omitted the candidate vectors are
    /// left empty for backward compatibility with `--base-db only`
    /// callers.
    #[allow(clippy::struct_field_names)]
    pub base_repo_root: Option<PathBuf>,
    pub head_repo_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphDiff {
    pub schema_version: u32,
    pub stats: GraphDiffStats,
    pub nodes_added: Vec<DiffNode>,
    pub nodes_removed: Vec<DiffNode>,
    pub nodes_kind_changed: Vec<DiffNodeKindChange>,
    pub edges_added: Vec<DiffEdge>,
    pub edges_removed: Vec<DiffEdge>,
    pub edges_status_changed: Vec<DiffEdgeStatusChange>,
    /// Candidate-level changes — only populated when both
    /// `base_repo_root` and `head_repo_root` were supplied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates_added: Vec<DiffCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates_removed: Vec<DiffCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates_status_changed: Vec<DiffCandidateStatusChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphDiffStats {
    pub base_nodes: usize,
    pub head_nodes: usize,
    pub base_edges: usize,
    pub head_edges: usize,
    pub nodes_added: usize,
    pub nodes_removed: usize,
    pub nodes_kind_changed: usize,
    pub edges_added: usize,
    pub edges_removed: usize,
    pub edges_status_changed: usize,
    /// 0 unless `base_repo_root` + `head_repo_root` were supplied.
    #[serde(default)]
    pub base_candidates: usize,
    #[serde(default)]
    pub head_candidates: usize,
    #[serde(default)]
    pub candidates_added: usize,
    #[serde(default)]
    pub candidates_removed: usize,
    #[serde(default)]
    pub candidates_status_changed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffNodeKindChange {
    pub id: String,
    pub from_kind: String,
    pub to_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub source: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffEdgeStatusChange {
    pub id: String,
    pub kind: String,
    pub from_status: String,
    pub to_status: String,
}

/// Summary of a business candidate as seen by graph-diff. We surface
/// just enough for a PR reviewer or AI agent to decide whether the new /
/// removed candidate matters — name, status, the YAML path the
/// candidate lives in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffCandidate {
    /// `business_candidate::<slug>` — the same id `graph.rs` / questions
    /// use, so reviewers can paste it into `specslice candidate review`.
    pub id: String,
    pub name: String,
    /// Effective review status (`accepted` / `rejected` / `needs_changes`
    /// / `pending` / `proposed`). Mirrors `BusinessCandidate::status`
    /// or `review.status` when set.
    pub status: String,
    /// Repo-relative YAML path so an agent can open it directly.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffCandidateStatusChange {
    pub id: String,
    pub name: String,
    pub from_status: String,
    pub to_status: String,
}

pub fn diff_graphs(options: GraphDiffOptions) -> Result<GraphDiff> {
    let base = Store::open(&options.base_db)
        .with_context(|| format!("opening base graph at {}", options.base_db.display()))?;
    let head = Store::open(&options.head_db)
        .with_context(|| format!("opening head graph at {}", options.head_db.display()))?;
    let mut diff = diff_graphs_with_stores(&base, &head)?;
    if let (Some(base_root), Some(head_root)) = (
        options.base_repo_root.as_ref(),
        options.head_repo_root.as_ref(),
    ) {
        diff_candidates_into(&mut diff, base_root, head_root)?;
    }
    Ok(diff)
}

pub fn diff_graphs_with_stores(base: &Store, head: &Store) -> Result<GraphDiff> {
    let base_nodes = base.list_all_nodes().context("listing base nodes")?;
    let head_nodes = head.list_all_nodes().context("listing head nodes")?;
    let base_edges = base.list_all_edges().context("listing base edges")?;
    let head_edges = head.list_all_edges().context("listing head edges")?;

    let base_node_map: BTreeMap<String, &Node> =
        base_nodes.iter().map(|n| (n.id.to_string(), n)).collect();
    let head_node_map: BTreeMap<String, &Node> =
        head_nodes.iter().map(|n| (n.id.to_string(), n)).collect();

    let mut nodes_added: Vec<DiffNode> = Vec::new();
    let mut nodes_removed: Vec<DiffNode> = Vec::new();
    let mut nodes_kind_changed: Vec<DiffNodeKindChange> = Vec::new();

    for (id, node) in &head_node_map {
        match base_node_map.get(id) {
            Some(prev) => {
                if prev.kind != node.kind {
                    nodes_kind_changed.push(DiffNodeKindChange {
                        id: id.clone(),
                        from_kind: prev.kind.as_str().into(),
                        to_kind: node.kind.as_str().into(),
                    });
                }
            }
            None => nodes_added.push(diff_node(node)),
        }
    }
    for (id, node) in &base_node_map {
        if !head_node_map.contains_key(id) {
            nodes_removed.push(diff_node(node));
        }
    }

    let base_edge_map: BTreeMap<String, &EdgeAssertion> =
        base_edges.iter().map(|e| (e.id.to_string(), e)).collect();
    let head_edge_map: BTreeMap<String, &EdgeAssertion> =
        head_edges.iter().map(|e| (e.id.to_string(), e)).collect();

    let mut edges_added: Vec<DiffEdge> = Vec::new();
    let mut edges_removed: Vec<DiffEdge> = Vec::new();
    let mut edges_status_changed: Vec<DiffEdgeStatusChange> = Vec::new();

    for (id, edge) in &head_edge_map {
        match base_edge_map.get(id) {
            Some(prev) => {
                if prev.status != edge.status {
                    edges_status_changed.push(DiffEdgeStatusChange {
                        id: id.clone(),
                        kind: edge.kind.as_str().into(),
                        from_status: prev.status.as_str().into(),
                        to_status: edge.status.as_str().into(),
                    });
                }
            }
            None => edges_added.push(diff_edge(edge)),
        }
    }
    for (id, edge) in &base_edge_map {
        if !head_edge_map.contains_key(id) {
            edges_removed.push(diff_edge(edge));
        }
    }

    // Stable order for snapshot-style diffing.
    nodes_added.sort_by(|a, b| a.id.cmp(&b.id));
    nodes_removed.sort_by(|a, b| a.id.cmp(&b.id));
    nodes_kind_changed.sort_by(|a, b| a.id.cmp(&b.id));
    edges_added.sort_by(|a, b| a.id.cmp(&b.id));
    edges_removed.sort_by(|a, b| a.id.cmp(&b.id));
    edges_status_changed.sort_by(|a, b| a.id.cmp(&b.id));

    let stats = GraphDiffStats {
        base_nodes: base_nodes.len(),
        head_nodes: head_nodes.len(),
        base_edges: base_edges.len(),
        head_edges: head_edges.len(),
        nodes_added: nodes_added.len(),
        nodes_removed: nodes_removed.len(),
        nodes_kind_changed: nodes_kind_changed.len(),
        edges_added: edges_added.len(),
        edges_removed: edges_removed.len(),
        edges_status_changed: edges_status_changed.len(),
        ..GraphDiffStats::default()
    };

    Ok(GraphDiff {
        schema_version: GRAPH_DIFF_SCHEMA_VERSION,
        stats,
        nodes_added,
        nodes_removed,
        nodes_kind_changed,
        edges_added,
        edges_removed,
        edges_status_changed,
        candidates_added: Vec::new(),
        candidates_removed: Vec::new(),
        candidates_status_changed: Vec::new(),
    })
}

/// Fold candidate-YAML diffs into an existing [`GraphDiff`] in place.
/// Pulled out as a free function so the test fixtures can drive it
/// without an actual `Store`.
pub fn diff_candidates_into(
    diff: &mut GraphDiff,
    base_repo_root: &std::path::Path,
    head_repo_root: &std::path::Path,
) -> Result<()> {
    let base = load_business_candidates(base_repo_root)
        .with_context(|| format!("loading base candidates from {}", base_repo_root.display()))?;
    let head = load_business_candidates(head_repo_root)
        .with_context(|| format!("loading head candidates from {}", head_repo_root.display()))?;
    let base_path = base.path.to_string_lossy().to_string();
    let head_path = head.path.to_string_lossy().to_string();

    let base_by_id: BTreeMap<String, &BusinessCandidate> = base
        .document
        .candidates
        .iter()
        .map(|c| (c.id.clone(), c))
        .collect();
    let head_by_id: BTreeMap<String, &BusinessCandidate> = head
        .document
        .candidates
        .iter()
        .map(|c| (c.id.clone(), c))
        .collect();

    for (id, c) in &head_by_id {
        match base_by_id.get(id) {
            None => diff.candidates_added.push(diff_candidate(c, &head_path)),
            Some(prev) => {
                let prev_status = effective_status(prev);
                let next_status = effective_status(c);
                if prev_status != next_status {
                    diff.candidates_status_changed
                        .push(DiffCandidateStatusChange {
                            id: candidate_artifact_id(&c.id),
                            name: c.name.clone(),
                            from_status: prev_status,
                            to_status: next_status,
                        });
                }
            }
        }
    }
    for (id, c) in &base_by_id {
        if !head_by_id.contains_key(id) {
            diff.candidates_removed.push(diff_candidate(c, &base_path));
        }
    }

    diff.candidates_added.sort_by(|a, b| a.id.cmp(&b.id));
    diff.candidates_removed.sort_by(|a, b| a.id.cmp(&b.id));
    diff.candidates_status_changed
        .sort_by(|a, b| a.id.cmp(&b.id));

    diff.stats.base_candidates = base.document.candidates.len();
    diff.stats.head_candidates = head.document.candidates.len();
    diff.stats.candidates_added = diff.candidates_added.len();
    diff.stats.candidates_removed = diff.candidates_removed.len();
    diff.stats.candidates_status_changed = diff.candidates_status_changed.len();
    Ok(())
}

fn diff_candidate(c: &BusinessCandidate, yaml_path: &str) -> DiffCandidate {
    DiffCandidate {
        id: candidate_artifact_id(&c.id),
        name: c.name.clone(),
        status: effective_status(c),
        path: yaml_path.to_string(),
    }
}

/// Collapse a candidate's `status` + `review.status` into a single
/// string. Mirrors [`BusinessCandidate::review_status`] but keeps the
/// raw "proposed" wording when no human review exists.
fn effective_status(c: &BusinessCandidate) -> String {
    if let Some(s) = c.review_status() {
        return match s {
            ReviewStatus::Accepted => "accepted".into(),
            ReviewStatus::Rejected => "rejected".into(),
            ReviewStatus::NeedsChanges => "needs_changes".into(),
            ReviewStatus::Pending => "pending".into(),
        };
    }
    // Default lifecycle position when no human verdict exists yet.
    let raw = c.status.trim().to_ascii_lowercase();
    if raw.is_empty() {
        "proposed".into()
    } else {
        raw
    }
}

fn diff_node(node: &Node) -> DiffNode {
    DiffNode {
        id: node.id.to_string(),
        kind: node.kind.as_str().into(),
        label: node
            .name
            .clone()
            .unwrap_or_else(|| node.stable_key.clone().unwrap_or_default()),
        path: node.path.clone(),
    }
}

fn diff_edge(edge: &EdgeAssertion) -> DiffEdge {
    DiffEdge {
        id: edge.id.to_string(),
        from: edge.from_id.to_string(),
        to: edge.to_id.to_string(),
        kind: edge.kind.as_str().into(),
        source: edge.source.as_str().into(),
        status: edge.status.as_str().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{ArtifactId, EdgeKind, EdgeSource, EdgeStatus, NodeKind};
    use tempfile::TempDir;

    fn empty_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    fn upsert(store: &mut Store, id: &str, kind: NodeKind, path: &str) {
        let mut n = Node::new(ArtifactId::new(id), kind);
        n.path = Some(path.into());
        n.name = Some(id.rsplit("::").next().unwrap().into());
        store.upsert_node(&n).unwrap();
    }

    fn edge(store: &mut Store, from: &str, to: &str, kind: EdgeKind, status: EdgeStatus) {
        let mut e = EdgeAssertion::fact(
            ArtifactId::new(from),
            ArtifactId::new(to),
            kind,
            EdgeSource::LanguageAdapter,
        );
        e.status = status;
        store.upsert_edge(&e).unwrap();
    }

    #[test]
    fn diff_reports_added_and_removed_nodes() {
        let (mut base, _b) = empty_store();
        let (mut head, _h) = empty_store();
        upsert(&mut base, "n::1", NodeKind::PythonFunction, "a.py");
        upsert(&mut base, "n::2", NodeKind::PythonFunction, "b.py");
        upsert(&mut head, "n::1", NodeKind::PythonFunction, "a.py");
        upsert(&mut head, "n::3", NodeKind::PythonFunction, "c.py");
        let d = diff_graphs_with_stores(&base, &head).unwrap();
        assert_eq!(d.stats.nodes_added, 1);
        assert_eq!(d.stats.nodes_removed, 1);
        assert_eq!(d.nodes_added[0].id, "n::3");
        assert_eq!(d.nodes_removed[0].id, "n::2");
    }

    #[test]
    fn diff_detects_kind_change_in_existing_node() {
        let (mut base, _b) = empty_store();
        let (mut head, _h) = empty_store();
        upsert(&mut base, "n::1", NodeKind::PythonFunction, "a.py");
        upsert(&mut head, "n::1", NodeKind::PythonMethod, "a.py");
        let d = diff_graphs_with_stores(&base, &head).unwrap();
        assert_eq!(d.stats.nodes_added, 0);
        assert_eq!(d.stats.nodes_removed, 0);
        assert_eq!(d.stats.nodes_kind_changed, 1);
        assert_eq!(d.nodes_kind_changed[0].from_kind, "python_function");
        assert_eq!(d.nodes_kind_changed[0].to_kind, "python_method");
    }

    fn write_candidates(dir: &TempDir, contents: &str) {
        let candidates_dir = dir.path().join(".specslice").join("candidates");
        std::fs::create_dir_all(&candidates_dir).unwrap();
        std::fs::write(candidates_dir.join("business_logic.yaml"), contents).unwrap();
    }

    #[test]
    fn diff_candidates_reports_added_removed_and_status_change() {
        let base = TempDir::new().unwrap();
        let head = TempDir::new().unwrap();
        write_candidates(
            &base,
            r#"
schema_version: 1
candidates:
  - id: pay_flow
    name: Pay
    status: proposed
  - id: signup
    name: SignUp
    status: proposed
"#,
        );
        write_candidates(
            &head,
            r#"
schema_version: 1
candidates:
  - id: pay_flow
    name: Pay
    status: accepted
    review:
      status: accepted
  - id: refund
    name: Refund
    status: proposed
"#,
        );

        let mut diff = GraphDiff {
            schema_version: GRAPH_DIFF_SCHEMA_VERSION,
            stats: GraphDiffStats::default(),
            nodes_added: Vec::new(),
            nodes_removed: Vec::new(),
            nodes_kind_changed: Vec::new(),
            edges_added: Vec::new(),
            edges_removed: Vec::new(),
            edges_status_changed: Vec::new(),
            candidates_added: Vec::new(),
            candidates_removed: Vec::new(),
            candidates_status_changed: Vec::new(),
        };
        diff_candidates_into(&mut diff, base.path(), head.path()).unwrap();

        // refund is new; signup is removed; pay_flow flips status.
        assert_eq!(diff.stats.base_candidates, 2);
        assert_eq!(diff.stats.head_candidates, 2);
        assert_eq!(diff.stats.candidates_added, 1);
        assert_eq!(diff.stats.candidates_removed, 1);
        assert_eq!(diff.stats.candidates_status_changed, 1);
        assert_eq!(diff.candidates_added[0].id, "business_candidate::refund");
        assert_eq!(diff.candidates_removed[0].id, "business_candidate::signup");
        let sc = &diff.candidates_status_changed[0];
        assert_eq!(sc.id, "business_candidate::pay_flow");
        assert_eq!(sc.from_status, "proposed");
        assert_eq!(sc.to_status, "accepted");
    }

    #[test]
    fn diff_graphs_keeps_candidate_vectors_empty_without_repo_roots() {
        let (mut base, _b) = empty_store();
        let (mut head, _h) = empty_store();
        upsert(&mut base, "n::1", NodeKind::PythonFunction, "a.py");
        upsert(&mut head, "n::1", NodeKind::PythonFunction, "a.py");
        let d = diff_graphs_with_stores(&base, &head).unwrap();
        assert!(d.candidates_added.is_empty());
        assert!(d.candidates_removed.is_empty());
        assert!(d.candidates_status_changed.is_empty());
        assert_eq!(d.stats.base_candidates, 0);
        assert_eq!(d.stats.head_candidates, 0);
    }

    #[test]
    fn diff_reports_edges_and_status_changes() {
        let (mut base, _b) = empty_store();
        let (mut head, _h) = empty_store();
        upsert(&mut base, "n::1", NodeKind::PythonFunction, "a.py");
        upsert(&mut base, "n::2", NodeKind::PythonFunction, "b.py");
        upsert(&mut head, "n::1", NodeKind::PythonFunction, "a.py");
        upsert(&mut head, "n::2", NodeKind::PythonFunction, "b.py");
        // Both: same edge id, but status changed.
        edge(
            &mut base,
            "n::1",
            "n::2",
            EdgeKind::Calls,
            EdgeStatus::Confirmed,
        );
        edge(
            &mut head,
            "n::1",
            "n::2",
            EdgeKind::Calls,
            EdgeStatus::Deprecated,
        );
        let d = diff_graphs_with_stores(&base, &head).unwrap();
        assert_eq!(d.stats.edges_status_changed, 1);
        assert_eq!(d.edges_status_changed[0].from_status, "confirmed");
        assert_eq!(d.edges_status_changed[0].to_status, "deprecated");
    }
}
