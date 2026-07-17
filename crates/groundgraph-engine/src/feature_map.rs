//! P19 — `groundgraph features`: cluster code into "functional
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
//! `.groundgraph.yaml` in a later iteration.
//!
//! Limitations (documented so AI agents don't oversell):
//! - Without LSP `Calls` / `References` edges, propagation
//!   reduces to import-only. This is still useful for Python /
//!   Dart / Go but won't surface dynamic dispatch links.
//! - The clustering is deterministic but not optimal — it's a
//!   "good enough" heuristic, not graph community detection.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;

use anyhow::Context;
use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeKind, Node, NodeKind};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

use crate::error::EngineResult;
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
            // 0 = auto: scale with repo size (~1 cluster / 250 code
            // symbols, clamped to [20, 80]).
            max_clusters: 0,
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
    /// can use these in `groundgraph slice` / `graph --focus`.
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

pub fn analyze_feature_map(options: FeatureMapOptions) -> EngineResult<FeatureMap> {
    let db_path = crate::config::storage_path_for_repo(&options.repo_root)?;
    let store = Store::open(&db_path)?;
    analyze_feature_map_with_store(&store, &options)
}

pub fn analyze_feature_map_with_store(
    store: &Store,
    options: &FeatureMapOptions,
) -> EngineResult<FeatureMap> {
    let nodes = store.list_all_nodes().context("listing nodes")?;
    let edges = store.list_all_edges().context("listing edges")?;

    // Load the whole graph into memory once. Previously the seed scoring and
    // BFS issued a SQLite query per node *per seed* (O(seeds × nodes) round
    // trips), which made `features` take >60s on a ~2k-symbol repo. The maps
    // below turn every neighbour lookup into an O(1) hashmap hit; the algorithm
    // and its (order-independent) output are unchanged.
    let nodes_by_id: HashMap<&ArtifactId, &Node> = nodes.iter().map(|n| (&n.id, n)).collect();
    let mut out_edges: HashMap<&ArtifactId, Vec<&EdgeAssertion>> = HashMap::new();
    let mut in_edges: HashMap<&ArtifactId, Vec<&EdgeAssertion>> = HashMap::new();
    for edge in &edges {
        out_edges.entry(&edge.from_id).or_default().push(edge);
        in_edges.entry(&edge.to_id).or_default().push(edge);
    }

    let code_node_count = nodes
        .iter()
        .filter(|n| groundgraph_core::is_code_symbol(n.kind))
        .count();

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
        // Test files consume features, they are not features: a test file
        // dense with TestCases used to outscore every business file and
        // cluster #1 became `test/...`. Tests still join clusters as
        // members through the BFS; they just never anchor one.
        if crate::path_class::is_test_path(&path) {
            continue;
        }
        // Each file's score = sum of framework roles inside it, plus how
        // referenced the file (or anything in it) is from *other* files —
        // graph centrality, not directory shape, marks a feature core.
        let descendants = collect_descendants(&out_edges, &node.id);
        let mut inside: BTreeSet<&ArtifactId> = descendants.iter().copied().collect();
        inside.insert(&node.id);
        let mut score: u32 = 1;
        let mut roles: BTreeSet<String> = BTreeSet::new();
        let mut external_refs: usize = 0;
        for &d in inside.iter() {
            if let Some(desc) = nodes_by_id.get(d).copied() {
                if let Some(metadata) = &desc.metadata_json {
                    if let Some(family) = framework_family(metadata) {
                        score += 5;
                        roles.insert(family);
                    }
                }
            }
            if let Some(ins) = in_edges.get(d) {
                for e in ins {
                    if matches!(
                        e.kind,
                        EdgeKind::Calls | EdgeKind::References | EdgeKind::Imports
                    ) && !inside.contains(&e.from_id)
                    {
                        external_refs += 1;
                    }
                }
            }
        }
        // `external_refs.min(20)` is in [0, 20]. `try_from` keeps clippy happy
        // (no lossy `as`); the `unwrap_or(20)` can't actually fire, and 20 is
        // exactly the clamp ceiling, so the fallback is consistent rather than
        // an arbitrary sentinel. (#260)
        score += u32::try_from(external_refs.min(20)).unwrap_or(20);
        seeds.push((node.id.clone(), score, path, roles.into_iter().collect()));
    }
    let total_seeds = seeds.len();

    // Sort by score descending, then path ascending for stable output.
    seeds.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    // One seed per file: a file node and a class inside it would otherwise
    // both anchor near-identical clusters under the same name.
    {
        let mut seen_paths: BTreeSet<String> = BTreeSet::new();
        seeds.retain(|(_, _, path, _)| seen_paths.insert(path.clone()));
    }
    // `max_clusters == 0` means auto: scale with repo size so a 200k-line
    // codebase is not squeezed into the same 20 clusters as a toy app.
    // ~1 cluster per 250 code symbols, clamped to [20, 80].
    let max_clusters = if options.max_clusters == 0 {
        (code_node_count / 250).clamp(20, 80)
    } else {
        options.max_clusters
    };
    seeds.truncate(max_clusters);

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
            // A node already claimed by another seed is re-claimed only by a
            // strictly higher-scoring seed. The old "same seed, shorter
            // distance" sub-condition was unreachable: each seed runs exactly
            // one BFS whose per-seed `visited` set pops every node at most once,
            // so a seed never revisits a node it already assigned. (#260)
            let beats = match assignments.get(&cur) {
                Some((_, prev_score, _)) => *seed_score > *prev_score,
                None => true,
            };
            if beats {
                assignments.insert(cur.clone(), (cluster_idx, *seed_score, depth));
            }
            // Walk outgoing Contains / Imports / Calls / References
            // edges; reverse Contains so the seed pulls in its
            // ancestors' file too.
            if let Some(out) = out_edges.get(&cur) {
                for edge in out {
                    if !matches!(
                        edge.kind,
                        EdgeKind::Contains
                            | EdgeKind::Imports
                            | EdgeKind::Calls
                            | EdgeKind::References
                    ) {
                        continue;
                    }
                    if visited.insert(edge.to_id.clone()) {
                        queue.push_back((edge.to_id.clone(), depth + 1));
                    }
                }
            }
            if let Some(inc) = in_edges.get(&cur) {
                for edge in inc {
                    if edge.kind != EdgeKind::Contains {
                        continue;
                    }
                    if visited.insert(edge.from_id.clone()) {
                        queue.push_back((edge.from_id.clone(), depth + 1));
                    }
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
            let Some(node) = nodes_by_id.get(id).copied() else {
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
            && (groundgraph_core::language_traits::is_callable(n.kind)
                || groundgraph_core::language_traits::is_type(n.kind))
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

/// In-memory Contains-only descendant walk over the prebuilt adjacency map.
fn collect_descendants<'a>(
    out_edges: &HashMap<&'a ArtifactId, Vec<&'a EdgeAssertion>>,
    root: &'a ArtifactId,
) -> Vec<&'a ArtifactId> {
    let mut out = Vec::new();
    let mut queue: VecDeque<&ArtifactId> = VecDeque::new();
    queue.push_back(root);
    let mut seen: BTreeSet<&ArtifactId> = BTreeSet::new();
    seen.insert(root);
    while let Some(cur) = queue.pop_front() {
        let Some(edges) = out_edges.get(cur) else {
            continue;
        };
        for edge in edges {
            if edge.kind != EdgeKind::Contains {
                continue;
            }
            if seen.insert(&edge.to_id) {
                out.push(&edge.to_id);
                queue.push_back(&edge.to_id);
            }
        }
    }
    out
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
    use groundgraph_core::EdgeSource;
    use tempfile::TempDir;

    fn store_with(
        nodes: &[(&str, NodeKind, &str)],
        edges: &[(&str, &str, EdgeKind)],
    ) -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        for (id, kind, path) in nodes {
            let mut n = Node::new(ArtifactId::new(*id), *kind);
            n.path = Some((*path).to_string());
            n.name = Some(id.rsplit("::").next().unwrap_or(id).to_string());
            store.upsert_node(&n).unwrap();
        }
        for (from, to, kind) in edges {
            let e = EdgeAssertion::fact(
                ArtifactId::new(*from),
                ArtifactId::new(*to),
                *kind,
                EdgeSource::LanguageAdapter,
            );
            store.upsert_edge(&e).unwrap();
        }
        (store, dir)
    }

    #[test]
    fn max_clusters_zero_scales_with_repo_size() {
        // 0 = auto. A large repo (Redis: ~7k symbols) must not be squeezed
        // into 20 clusters; a small repo keeps the floor of 20.
        let mut nodes: Vec<(String, NodeKind, String)> = Vec::new();
        let mut edges: Vec<(String, String, EdgeKind)> = Vec::new();
        // 30 files × 250 symbols each = 7500 code symbols → cap 30 clusters.
        for f in 0..30 {
            let file = format!("file::src/m{f}.c");
            nodes.push((file.clone(), NodeKind::File, format!("src/m{f}.c")));
            for s in 0..250 {
                let id = format!("c::src/m{f}.c::fn{s}");
                nodes.push((id.clone(), NodeKind::CFunction, format!("src/m{f}.c")));
                edges.push((file.clone(), id, EdgeKind::Contains));
            }
        }
        let node_refs: Vec<(&str, NodeKind, &str)> = nodes
            .iter()
            .map(|(id, k, p)| (id.as_str(), *k, p.as_str()))
            .collect();
        let edge_refs: Vec<(&str, &str, EdgeKind)> = edges
            .iter()
            .map(|(f, t, k)| (f.as_str(), t.as_str(), *k))
            .collect();
        let (store, _dir) = store_with(&node_refs, &edge_refs);
        let map = analyze_feature_map_with_store(
            &store,
            &FeatureMapOptions {
                max_clusters: 0,
                ..FeatureMapOptions::default()
            },
        )
        .unwrap();
        assert!(
            map.stats.clusters_reported > 20,
            "7500-symbol repo must report more than the legacy 20 clusters, got {}",
            map.stats.clusters_reported
        );
    }

    #[test]
    fn test_files_never_seed_clusters() {
        // A test file stuffed with TestCases used to outscore every business
        // file (its own cases counted as "coverage"), so cluster #1 became
        // `test/...`. Test files are consumers of features, not features.
        let mut nodes: Vec<(String, NodeKind, String)> = vec![
            (
                "file::test/a_test.dart".into(),
                NodeKind::File,
                "test/a_test.dart".into(),
            ),
            (
                "file::lib/core.dart".into(),
                NodeKind::File,
                "lib/core.dart".into(),
            ),
            (
                "dart::lib/core.dart::Core".into(),
                NodeKind::DartClass,
                "lib/core.dart".into(),
            ),
            (
                "file::lib/user.dart".into(),
                NodeKind::File,
                "lib/user.dart".into(),
            ),
            (
                "dart::lib/user.dart::Usage".into(),
                NodeKind::DartFunction,
                "lib/user.dart".into(),
            ),
        ];
        let mut edges: Vec<(String, String, EdgeKind)> = vec![
            (
                "file::lib/core.dart".into(),
                "dart::lib/core.dart::Core".into(),
                EdgeKind::Contains,
            ),
            (
                "file::lib/user.dart".into(),
                "dart::lib/user.dart::Usage".into(),
                EdgeKind::Contains,
            ),
            // external usage makes lib/core.dart the natural top seed
            (
                "dart::lib/user.dart::Usage".into(),
                "dart::lib/core.dart::Core".into(),
                EdgeKind::Calls,
            ),
        ];
        for i in 0..10 {
            let id = format!("test::test/a_test.dart::case{i}");
            nodes.push((id.clone(), NodeKind::TestCase, "test/a_test.dart".into()));
            edges.push(("file::test/a_test.dart".into(), id, EdgeKind::Contains));
        }
        let node_refs: Vec<(&str, NodeKind, &str)> = nodes
            .iter()
            .map(|(a, k, p)| (a.as_str(), *k, p.as_str()))
            .collect();
        let edge_refs: Vec<(&str, &str, EdgeKind)> = edges
            .iter()
            .map(|(f, t, k)| (f.as_str(), t.as_str(), *k))
            .collect();
        let (store, _dir) = store_with(&node_refs, &edge_refs);

        let map = analyze_feature_map_with_store(
            &store,
            &FeatureMapOptions {
                min_cluster_size: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            map.clusters
                .iter()
                .all(|c| !crate::path_class::is_test_path(&c.seed_path)),
            "seeds={:?}",
            map.clusters
                .iter()
                .map(|c| c.seed_path.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            map.clusters.first().map(|c| c.seed_path.as_str()),
            Some("lib/core.dart"),
            "referenced business file should rank first"
        );
    }

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
