//! `groundgraph graph --format web` data backend.
//!
//! Builds a *full* force-directed network view of the graph store — every
//! stored node plus the de-duplicated edge set — for the WebGL constellation
//! viewer (`webui/index.html`). Unlike [`crate::graph::build_graph_view`], which
//! produces a curated, capped *business* view, this is the raw topology: the
//! viewer itself does the adaptive degradation (degree capping, kind hiding) at
//! render time. This is the Rust port of the bootstrap `webui/export_graph.py`
//! script so the export is a first-class CLI feature with no Python dependency.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Context;
use groundgraph_core::{EdgeAssertion, Node};
use groundgraph_store::Store;
use serde::Serialize;

use crate::config::{resolve_storage_path, EngineConfig};
use crate::error::EngineResult;

/// A node in the force-directed network. Field names match the JSON the viewer
/// consumes (`webui/index.html`): `id, kind, name, path, line, deg`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkNode {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Undirected degree over the de-duplicated link set; drives node size and
    /// the viewer's top-N backbone cap on large graphs.
    pub deg: usize,
}

/// A directed link `source --kind--> target` between two node ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkLink {
    pub source: String,
    pub target: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkMeta {
    pub repo: String,
    pub nodes: usize,
    pub links: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkGraph {
    pub meta: NetworkMeta,
    pub nodes: Vec<NetworkNode>,
    pub links: Vec<NetworkLink>,
}

#[derive(Debug, Clone)]
pub struct NetworkOptions {
    pub repo_root: PathBuf,
    /// Keep nodes with degree 0. Off by default: a force layout of thousands of
    /// disconnected points is noise, so the export drops them like the viewer.
    pub keep_isolated: bool,
}

/// Assemble the network from already-loaded nodes/edges. Pure (no I/O) so the
/// topology rules — self-loop drop, dangling-edge drop, `(from,to,kind)`
/// de-duplication, degree, isolated-node filtering — are unit-testable without
/// a database.
pub fn network_from_graph(
    repo: &str,
    nodes: &[Node],
    edges: &[EdgeAssertion],
    keep_isolated: bool,
) -> NetworkGraph {
    let mut out: Vec<NetworkNode> = Vec::with_capacity(nodes.len());
    let mut id_to_idx: HashMap<&str, usize> = HashMap::with_capacity(nodes.len());
    for (idx, n) in nodes.iter().enumerate() {
        let id = n.id.as_str();
        // First id wins; duplicate ids in the store would otherwise inflate
        // degree counts on a phantom second copy.
        if id_to_idx.contains_key(id) {
            continue;
        }
        id_to_idx.insert(id, out.len());
        let name = n
            .name
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| id.rsplit("::").next().unwrap_or(id).to_string());
        out.push(NetworkNode {
            id: id.to_string(),
            kind: n.kind.as_str().to_string(),
            name,
            path: n.path.clone().unwrap_or_default(),
            line: n.start_line,
            deg: 0,
        });
        let _ = idx;
    }

    let mut links: Vec<NetworkLink> = Vec::new();
    let mut seen: BTreeSet<(usize, usize, &str)> = BTreeSet::new();
    let mut deg: Vec<usize> = vec![0; out.len()];
    for e in edges {
        let (a, b) = (e.from_id.as_str(), e.to_id.as_str());
        if a == b {
            continue; // self-loop: no visual signal in a force layout
        }
        let (Some(&ai), Some(&bi)) = (id_to_idx.get(a), id_to_idx.get(b)) else {
            continue; // edge to a node not in the store: skip the dangling link
        };
        let kind = e.kind.as_str();
        if !seen.insert((ai, bi, kind)) {
            continue; // collapse parallel edges of the same kind
        }
        links.push(NetworkLink {
            source: a.to_string(),
            target: b.to_string(),
            kind: kind.to_string(),
        });
        deg[ai] += 1;
        deg[bi] += 1;
    }
    for (i, n) in out.iter_mut().enumerate() {
        n.deg = deg[i];
    }

    if !keep_isolated {
        out.retain(|n| n.deg > 0);
    }

    NetworkGraph {
        meta: NetworkMeta {
            repo: repo.to_string(),
            nodes: out.len(),
            links: links.len(),
        },
        nodes: out,
        links,
    }
}

/// Load the graph store at `repo_root` and build the full network view.
pub fn build_network_graph(options: NetworkOptions) -> EngineResult<NetworkGraph> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config)?;
    let mut store = Store::open(&db_path)?;
    store.migrate()?;
    let nodes = store.list_all_nodes().context("listing nodes")?;
    let edges = store.list_all_edges().context("listing edges")?;
    let repo = repo_name(&options.repo_root);
    Ok(network_from_graph(
        &repo,
        &nodes,
        &edges,
        options.keep_isolated,
    ))
}

/// Human-friendly repo label: the canonical directory name, falling back to the
/// raw path's last component.
fn repo_name(repo_root: &Path) -> String {
    repo_root
        .canonicalize()
        .ok()
        .as_deref()
        .and_then(|p| p.file_name())
        .or_else(|| repo_root.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string())
}

fn load_config(repo_root: &Path) -> crate::error::EngineResult<EngineConfig> {
    crate::config::load_config(repo_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, NodeKind};

    fn node(id: &str, kind: NodeKind, name: &str, path: &str, line: u32) -> Node {
        let mut n = Node::new(ArtifactId::new(id.to_string()), kind);
        n.name = Some(name.to_string());
        n.path = Some(path.to_string());
        n.start_line = Some(line);
        n
    }

    fn edge(from: &str, to: &str, kind: EdgeKind) -> EdgeAssertion {
        EdgeAssertion::fact(
            ArtifactId::new(from.to_string()),
            ArtifactId::new(to.to_string()),
            kind,
            EdgeSource::LanguageAdapter,
        )
    }

    #[test]
    fn builds_nodes_links_and_degree_dropping_isolated() {
        let nodes = vec![
            node("a", NodeKind::GoMethod, "A", "a.go", 1),
            node("b", NodeKind::GoMethod, "B", "b.go", 2),
            node("c", NodeKind::GoMethod, "C", "c.go", 3), // isolated
        ];
        let edges = vec![edge("a", "b", EdgeKind::Calls)];

        let g = network_from_graph("demo", &nodes, &edges, false);

        assert_eq!(g.meta.repo, "demo");
        assert_eq!(g.meta.nodes, 2, "isolated node c dropped");
        assert_eq!(g.meta.links, 1);
        let a = g.nodes.iter().find(|n| n.id == "a").expect("a");
        assert_eq!(a.deg, 1);
        assert_eq!(a.kind, "go_method");
        assert!(g.nodes.iter().all(|n| n.id != "c"), "c is isolated");
        let l = &g.links[0];
        assert_eq!(
            (l.source.as_str(), l.target.as_str(), l.kind.as_str()),
            ("a", "b", "calls")
        );
    }

    #[test]
    fn keep_isolated_retains_degree_zero_nodes() {
        let nodes = vec![node("solo", NodeKind::GoMethod, "Solo", "s.go", 1)];
        let g = network_from_graph("demo", &nodes, &[], true);
        assert_eq!(g.meta.nodes, 1);
        assert_eq!(g.nodes[0].deg, 0);
    }

    #[test]
    fn drops_self_loops_dangling_edges_and_dedupes_parallel() {
        let nodes = vec![
            node("a", NodeKind::GoMethod, "A", "a.go", 1),
            node("b", NodeKind::GoMethod, "B", "b.go", 2),
        ];
        let edges = vec![
            edge("a", "a", EdgeKind::Calls),      // self-loop → dropped
            edge("a", "ghost", EdgeKind::Calls),  // dangling target → dropped
            edge("a", "b", EdgeKind::Calls),      // kept
            edge("a", "b", EdgeKind::Calls),      // parallel duplicate → collapsed
            edge("a", "b", EdgeKind::References), // different kind → kept
        ];
        let g = network_from_graph("demo", &nodes, &edges, false);
        assert_eq!(
            g.meta.links, 2,
            "one calls + one references, dups/self/dangling removed"
        );
        let a = g.nodes.iter().find(|n| n.id == "a").expect("a");
        // a–b counted twice (two distinct-kind links); degree is over links.
        assert_eq!(a.deg, 2);
    }

    #[test]
    fn name_falls_back_to_last_id_segment_when_missing() {
        let mut n = Node::new(
            ArtifactId::new("http_route::f.go::GET /x".to_string()),
            NodeKind::HttpRoute,
        );
        n.name = None;
        n.path = None;
        let g = network_from_graph("demo", &[n], &[], true);
        assert_eq!(g.nodes[0].name, "GET /x");
        assert_eq!(g.nodes[0].path, "");
    }
}
