//! Evidence-preserving network projection for the offline workspace.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Context;
use groundgraph_core::{EdgeAssertion, Node};
use groundgraph_store::Store;
use serde::Serialize;

use crate::config::{resolve_storage_path, EngineConfig};
use crate::error::EngineResult;

/// A searchable node in the offline workspace. Field names match the JSON it
/// consumes (`webui/index.html`): `id, kind, name, path, line, deg`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetworkNode {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_json: Option<String>,
    pub kind: String,
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Undirected degree over grouped links, independent of assertion count.
    pub deg: usize,
}

/// A directed link `source --kind--> target` between two node ids.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetworkLink {
    pub assertions: Vec<EdgeAssertion>,
    pub source: String,
    pub target: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetworkMeta {
    pub schema_version: u32,
    pub omitted_isolated: usize,
    pub repo: String,
    pub nodes: usize,
    pub links: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetworkGraph {
    pub java_analysis: crate::java_semantics::JavaAnalysis,
    pub dangling_assertions: Vec<EdgeAssertion>,
    pub meta: NetworkMeta,
    pub nodes: Vec<NetworkNode>,
    pub links: Vec<NetworkLink>,
}

#[derive(Debug, Clone)]
pub struct NetworkOptions {
    pub repo_root: PathBuf,
    /// Keep isolated nodes so they remain searchable even without relationships.
    pub keep_isolated: bool,
}

/// Assemble the network from already-loaded nodes/edges. Pure (no I/O) so the
/// topology, assertion grouping and explicit omissions are unit-testable without
/// a database.
pub fn network_from_graph(
    repo: &str,
    nodes: &[Node],
    edges: &[EdgeAssertion],
    keep_isolated: bool,
) -> NetworkGraph {
    let mut out: Vec<NetworkNode> = Vec::with_capacity(nodes.len());
    let mut id_to_idx: HashMap<&str, usize> = HashMap::with_capacity(nodes.len());
    for n in nodes {
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
            // File-level call inventories are already exported in java_analysis.
            metadata_json: (n.kind != groundgraph_core::NodeKind::File)
                .then(|| n.metadata_json.clone())
                .flatten(),
            kind: n.kind.as_str().to_string(),
            name,
            path: n.path.clone().unwrap_or_default(),
            line: n.start_line,
            deg: 0,
        });
    }

    let mut links: Vec<NetworkLink> = Vec::new();
    let mut seen: BTreeMap<(&str, &str, &str), usize> = BTreeMap::new();
    let mut dangling_assertions = Vec::new();
    let mut deg: Vec<usize> = vec![0; out.len()];
    for e in edges {
        let (a, b) = (e.from_id.as_str(), e.to_id.as_str());
        let (Some(&ai), Some(&bi)) = (id_to_idx.get(a), id_to_idx.get(b)) else {
            dangling_assertions.push(e.clone());
            continue;
        };
        let kind = e.kind.as_str();
        if let Some(&idx) = seen.get(&(a, b, kind)) {
            if !links[idx].assertions.iter().any(|old| old.id == e.id) {
                links[idx].assertions.push(e.clone());
            }
            continue;
        }
        seen.insert((a, b, kind), links.len());
        links.push(NetworkLink {
            assertions: vec![e.clone()],
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

    let original_count = out.len();
    if !keep_isolated {
        out.retain(|n| n.deg > 0);
    }

    out.sort_by(|a, b| a.id.cmp(&b.id));
    links.sort_by(|a, b| (&a.source, &a.target, &a.kind).cmp(&(&b.source, &b.target, &b.kind)));
    for link in &mut links {
        link.assertions.sort_by(|a, b| a.id.cmp(&b.id));
    }
    dangling_assertions.sort_by(|a, b| a.id.cmp(&b.id));
    NetworkGraph {
        java_analysis: Default::default(),
        dangling_assertions,
        meta: NetworkMeta {
            schema_version: 2,
            omitted_isolated: original_count - out.len(),
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
    let mut graph = network_from_graph(&repo, &nodes, &edges, options.keep_isolated);
    let files = nodes.iter().filter_map(|n| n.path.clone()).collect();
    graph.java_analysis = crate::java_semantics::analysis_for_files(&store, &files)?;
    Ok(graph)
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
    fn preserves_recursion_and_reports_dangling_assertions() {
        let nodes = vec![
            node("a", NodeKind::GoMethod, "A", "a.go", 1),
            node("b", NodeKind::GoMethod, "B", "b.go", 2),
        ];
        let edges = vec![
            edge("a", "a", EdgeKind::Calls),      // self-loop → retained
            edge("a", "ghost", EdgeKind::Calls),  // dangling target → dropped
            edge("a", "b", EdgeKind::Calls),      // kept
            edge("a", "b", EdgeKind::Calls),      // parallel duplicate → collapsed
            edge("a", "b", EdgeKind::References), // different kind → kept
        ];
        let g = network_from_graph("demo", &nodes, &edges, false);
        assert_eq!(g.meta.links, 3, "recursion + calls + references retained");
        let a = g.nodes.iter().find(|n| n.id == "a").expect("a");
        // a–b counted twice (two distinct-kind links); degree is over links.
        assert_eq!(a.deg, 4);
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(json["dangling_assertions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn parallel_assertions_keep_their_provenance() {
        let nodes = vec![
            node("a", NodeKind::GoMethod, "A", "a.go", 1),
            node("b", NodeKind::GoMethod, "B", "b.go", 2),
        ];
        let first = edge("a", "b", EdgeKind::Calls);
        let mut second = first.clone();
        second.id = ArtifactId::new("another-resolver");
        second.source_file = Some("evidence.json".into());
        let g = network_from_graph("demo", &nodes, &[first, second], true);
        let value = serde_json::to_value(g).unwrap();
        assert_eq!(value["links"][0]["assertions"].as_array().unwrap().len(), 2);
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
