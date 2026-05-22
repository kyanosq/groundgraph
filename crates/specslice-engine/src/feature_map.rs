//! P19 — `specslice features`: cluster code into "functional
//! areas" by walking the call / import graph and grouping
//! tightly-coupled symbols together.
//!
//! Two-pass algorithm, deliberately simple so the output stays
//! explainable:
//!
//! 1. **Seed selection.** Every File / Python module / Dart
//!    module node becomes a candidate cluster seed. We score
//!    seeds by `framework_role` presence (so a file containing
//!    FastAPI routes outranks a generic helpers file) and pick
//!    the top-N as anchors.
//! 2. **Label propagation.** Starting from each seed, do a BFS
//!    over `Contains` (down — pull in children) and `Imports` /
//!    `Calls` / `References` (sideways — pull in tightly-coupled
//!    symbols). Each symbol records the strongest seed it
//!    received a label from; ties are broken by seed score.
//!
//! Results are reported as named clusters. Names are derived
//! heuristically from the seed path (`backend/app/auth/login.py`
//! → "auth · login"); operators can override via
//! `.specslice.yaml` in a later iteration.
//!
//! Limitations (documented so AI agents don't oversell):
//! - Without LSP `Calls` / `References` edges, propagation
//!   reduces to import-only. This is still useful for Python /
//!   Dart / Go but won't surface dynamic dispatch links.
//! - The clustering is deterministic but not optimal — it's a
//!   "good enough" heuristic, not graph community detection.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{ArtifactId, EdgeKind, NodeKind};
use specslice_store::Store;

use crate::python_frameworks::FrameworkRole;

pub const FEATURE_MAP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct FeatureMapOptions {
    pub repo_root: PathBuf,
    /// Maximum number of clusters to report. We pick the highest-
    /// scoring seeds.
    pub max_clusters: usize,
    /// Maximum BFS depth when propagating labels from a seed.
    pub max_propagation_depth: usize,
    /// Lower bound on cluster size (in nodes) to keep the report
    /// from drowning in single-symbol clusters.
    pub min_cluster_size: usize,
}

impl Default for FeatureMapOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            max_clusters: 20,
            max_propagation_depth: 3,
            min_cluster_size: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureMap {
    pub schema_version: u32,
    pub stats: FeatureMapStats,
    pub clusters: Vec<FeatureCluster>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureMapStats {
    pub seeds_considered: usize,
    pub clusters_reported: usize,
    pub nodes_assigned: usize,
    pub nodes_unassigned: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureCluster {
    /// Stable cluster id derived from the seed path / module.
    pub id: String,
    /// Human-readable name. Derived from seed path; operators
    /// can later override via configuration.
    pub name: String,
    /// Path of the seed node (file / module).
    pub seed_path: String,
    /// Score of the seed (higher = more "central").
    pub seed_score: u32,
    /// Top-N representative symbol ids in this cluster, ordered
    /// by their distance from the seed (closer first). Caller
    /// can use these in `specslice slice` / `graph --focus`.
    pub representative_symbols: Vec<FeatureClusterMember>,
    /// Total node count attached to this cluster.
    pub node_count: usize,
    /// Reason tags drawn from framework metadata on the seed
    /// (`fastapi_route`, `pytest_test`, ...). Empty when no
    /// framework role is detected.
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureClusterMember {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: String,
    pub distance_from_seed: u32,
}

pub fn analyze_feature_map(options: FeatureMapOptions) -> Result<FeatureMap> {
    let db_path = options.repo_root.join(".specslice").join("graph.db");
    let store = Store::open(&db_path)
        .with_context(|| format!("opening graph store at {}", db_path.display()))?;
    analyze_feature_map_with_store(&store, &options)
}

pub fn analyze_feature_map_with_store(
    store: &Store,
    options: &FeatureMapOptions,
) -> Result<FeatureMap> {
    let nodes = store.list_all_nodes().context("listing nodes")?;

    // ---- score every potential seed ---------------------------
    let mut seeds: Vec<(ArtifactId, u32, String, Vec<String>)> = Vec::new();
    for node in &nodes {
        let is_seed = matches!(
            node.kind,
            NodeKind::PythonModule | NodeKind::File | NodeKind::DartClass | NodeKind::SwiftClass
        );
        if !is_seed {
            continue;
        }
        let Some(path) = node.path.clone() else {
            continue;
        };
        // Each file's score = sum of framework roles inside it.
        let descendants = collect_descendants(store, &node.id);
        let mut score: u32 = 1;
        let mut roles: BTreeSet<String> = BTreeSet::new();
        for d in &descendants {
            if let Some(metadata) = lookup_metadata_json(&nodes, d) {
                if let Some(family) = framework_family(&metadata) {
                    score += 5;
                    roles.insert(family);
                }
            }
        }
        // Tests count as a small bump — a file with 10 tests
        // probably represents a coherent feature surface.
        let test_count = descendants
            .iter()
            .filter_map(|id| find_node(&nodes, id))
            .filter(|n| matches!(n.kind, NodeKind::TestCase | NodeKind::TestGroup))
            .count();
        score += u32::try_from(test_count.min(20)).unwrap_or(20);
        seeds.push((node.id.clone(), score, path, roles.into_iter().collect()));
    }
    let total_seeds = seeds.len();

    // Sort by score descending, then path ascending for stable output.
    seeds.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    seeds.truncate(options.max_clusters);

    // ---- propagate labels --------------------------------------
    // Each assignment carries (cluster_idx, seed_score, distance_from_seed).
    // We record the distance during the BFS so the report stage
    // doesn't need a second pass.
    let mut assignments: HashMap<ArtifactId, (usize, u32, u32)> = HashMap::new();
    for (cluster_idx, (seed_id, seed_score, _, _)) in seeds.iter().enumerate() {
        let mut queue: VecDeque<(ArtifactId, u32)> = VecDeque::new();
        // Per-seed visited set guards against re-queuing in the
        // same propagation; a higher-scoring seed CAN still
        // overwrite this node later (handled by the `beats`
        // check below across iterations).
        let mut visited: BTreeSet<ArtifactId> = BTreeSet::new();
        queue.push_back((seed_id.clone(), 0));
        visited.insert(seed_id.clone());
        let max_depth_u32 = u32::try_from(options.max_propagation_depth).unwrap_or(u32::MAX);
        while let Some((cur, depth)) = queue.pop_front() {
            if depth > max_depth_u32 {
                continue;
            }
            // Insert assignment if this seed has a higher score
            // than whoever previously claimed this node, OR if
            // same seed but at a shorter distance.
            let beats = match assignments.get(&cur) {
                Some((prev_idx, prev_score, prev_dist)) => {
                    *seed_score > *prev_score || (*prev_idx == cluster_idx && depth < *prev_dist)
                }
                None => true,
            };
            if beats {
                assignments.insert(cur.clone(), (cluster_idx, *seed_score, depth));
            }
            // Walk outgoing Contains / Imports / Calls / References
            // edges; reverse Contains so the seed pulls in its
            // ancestors' file too.
            let out = store
                .list_edges_from(&cur)
                .with_context(|| format!("listing edges from {cur}"))?;
            for edge in out {
                if !matches!(
                    edge.kind,
                    EdgeKind::Contains | EdgeKind::Imports | EdgeKind::Calls | EdgeKind::References
                ) {
                    continue;
                }
                if visited.insert(edge.to_id.clone()) {
                    queue.push_back((edge.to_id, depth + 1));
                }
            }
            let inc = store
                .list_edges_to(&cur)
                .with_context(|| format!("listing edges to {cur}"))?;
            for edge in inc {
                if !matches!(edge.kind, EdgeKind::Contains) {
                    continue;
                }
                if visited.insert(edge.from_id.clone()) {
                    queue.push_back((edge.from_id, depth + 1));
                }
            }
        }
    }

    // ---- build cluster reports ---------------------------------
    let mut buckets: BTreeMap<usize, Vec<(ArtifactId, u32)>> = BTreeMap::new();
    for (id, (cluster_idx, _, distance)) in &assignments {
        buckets
            .entry(*cluster_idx)
            .or_default()
            .push((id.clone(), *distance));
    }

    let mut clusters: Vec<FeatureCluster> = Vec::new();
    let nodes_assigned = assignments.len();
    let mut total_unassigned: usize = 0;
    for (cluster_idx, mut members) in buckets {
        let (seed_id, seed_score, seed_path, roles) = &seeds[cluster_idx];
        if members.len() < options.min_cluster_size {
            continue;
        }
        members.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.to_string().cmp(&b.0.to_string())));
        let mut representative_symbols: Vec<FeatureClusterMember> = Vec::new();
        for (id, distance) in members.iter().take(12) {
            let Some(node) = find_node(&nodes, id) else {
                continue;
            };
            // Skip the seed itself in the representative list —
            // it's already named at the cluster level.
            if id == seed_id {
                continue;
            }
            representative_symbols.push(FeatureClusterMember {
                id: id.to_string(),
                kind: node.kind.as_str().into(),
                label: node
                    .name
                    .clone()
                    .unwrap_or_else(|| node.stable_key.clone().unwrap_or_default()),
                path: node.path.clone().unwrap_or_default(),
                distance_from_seed: *distance,
            });
        }
        let name = derive_cluster_name(seed_path);
        let cluster_id = format!("feature::{}", seed_id.as_str());
        clusters.push(FeatureCluster {
            id: cluster_id,
            name,
            seed_path: seed_path.clone(),
            seed_score: *seed_score,
            representative_symbols,
            node_count: members.len(),
            roles: roles.clone(),
        });
    }
    clusters.sort_by(|a, b| {
        b.seed_score
            .cmp(&a.seed_score)
            .then(b.node_count.cmp(&a.node_count))
            .then(a.name.cmp(&b.name))
    });

    // Unassigned counter — route through the cross-language trait
    // layer so new languages (TypeScript / Java / ...) automatically
    // contribute to the "nodes_unassigned" stat without a hand-edit
    // here. Module-level nodes are deliberately excluded — they sit
    // closer to the seed and would inflate the metric.
    let assigned_ids: BTreeSet<&ArtifactId> = assignments.keys().collect();
    for n in &nodes {
        if !assigned_ids.contains(&n.id)
            && (specslice_core::language_traits::is_callable(n.kind)
                || specslice_core::language_traits::is_type(n.kind))
        {
            total_unassigned += 1;
        }
    }
    let clusters_reported = clusters.len();
    Ok(FeatureMap {
        schema_version: FEATURE_MAP_SCHEMA_VERSION,
        stats: FeatureMapStats {
            seeds_considered: total_seeds,
            clusters_reported,
            nodes_assigned,
            nodes_unassigned: total_unassigned,
        },
        clusters,
    })
}

fn collect_descendants(store: &Store, root: &ArtifactId) -> Vec<ArtifactId> {
    let mut out = Vec::new();
    let mut queue: VecDeque<ArtifactId> = VecDeque::new();
    queue.push_back(root.clone());
    let mut seen: BTreeSet<ArtifactId> = BTreeSet::new();
    seen.insert(root.clone());
    while let Some(cur) = queue.pop_front() {
        let edges = match store.list_edges_from(&cur) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for edge in edges {
            if !matches!(edge.kind, EdgeKind::Contains) {
                continue;
            }
            if seen.insert(edge.to_id.clone()) {
                out.push(edge.to_id.clone());
                queue.push_back(edge.to_id);
            }
        }
    }
    out
}

fn lookup_metadata_json(nodes: &[specslice_core::Node], id: &ArtifactId) -> Option<String> {
    nodes
        .iter()
        .find(|n| &n.id == id)
        .and_then(|n| n.metadata_json.clone())
}

fn find_node<'a>(
    nodes: &'a [specslice_core::Node],
    id: &ArtifactId,
) -> Option<&'a specslice_core::Node> {
    nodes.iter().find(|n| &n.id == id)
}

fn framework_family(metadata_json: &str) -> Option<String> {
    let role: FrameworkRole = serde_json::from_str(metadata_json).ok()?;
    Some(role.family().to_string())
}

/// Turn a repo-relative path like `backend/app/auth/login.py`
/// into a short feature name like `auth · login`. We drop the
/// outermost directory (usually `backend`, `src`, `lib`), strip
/// `.py` / `.dart` extensions, and join the remaining segments
/// with `·` for visual punch.
fn derive_cluster_name(path: &str) -> String {
    let mut parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != "lib" && *p != "src" && *p != "backend")
        .collect();
    if let Some(last) = parts.last_mut() {
        // Drop common code extensions to keep the label clean.
        for ext in [".py", ".dart", ".swift", ".go", ".ts", ".js"] {
            if let Some(stripped) = last.strip_suffix(ext) {
                *last = stripped;
                break;
            }
        }
    }
    if parts.is_empty() {
        path.to_string()
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_cluster_name_drops_outer_dirs_and_extensions() {
        assert_eq!(
            derive_cluster_name("backend/app/auth/login.py"),
            "app · auth · login"
        );
        assert_eq!(
            derive_cluster_name("lib/ui/widgets/login_form.dart"),
            "ui · widgets · login_form"
        );
        assert_eq!(derive_cluster_name("README.md"), "README.md");
    }

    #[test]
    fn unknown_metadata_json_does_not_panic() {
        assert!(framework_family("{}").is_none());
        assert!(framework_family("not json").is_none());
        // FrameworkRole uses #[serde(tag = "framework", rename_all = "snake_case")]
        // so the on-disk form starts with `{"framework": "..."}`.
        let json =
            r#"{"framework":"fastapi_route","verb":"get","path":"/x","decorator":"app.get"}"#;
        assert_eq!(framework_family(json), Some("fastapi_route".to_string()));
    }
}
