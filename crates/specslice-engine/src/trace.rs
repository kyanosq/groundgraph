//! P27 — `接口 → 整张图`：从一个接口/符号出发，沿调用 / 引用 / 持久化等边做
//! **前向传递闭包**，得到 `controller → service → impl → mapper → SQL → table`
//! 的完整下游链路。
//!
//! 背景：`search` 给的是 1 跳并集，`graph --view focus` 给的是「焦点 + 后代 +
//! 1 跳邻居」，两者都是**浅层**视图，回答不了「这个接口背后到底牵动了图里的哪
//! 些东西、最终落到哪几张表」。`trace` 做有界 BFS 前向闭包，是移植 / 影响分析
//! 时把一个端点「整条链路」一次性捞出来的主力命令。
//!
//! 闭包只跟随**语义/数据流**边（calls / references / reads_provider /
//! persists_to / navigates_to / subscribes_stream / derives_from），不跟随结构边
//! （contains / imports），否则一个类的焦点会把整文件兄弟节点全拉进来。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{ArtifactId, NodeKind};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::search::{run_search_with_store, SearchOptions};

/// Edge kinds the forward closure follows. A curated subset of
/// [`crate::search::EXPANSION_EDGE_KINDS`] that excludes the purely structural
/// `contains` / `imports` (which would balloon a method focus into whole files).
pub const TRACE_EDGE_KINDS: &[&str] = &[
    "calls",
    "references",
    "reads_provider",
    "persists_to",
    "navigates_to",
    "subscribes_stream",
    "derives_from",
    // interface → concrete impl (Spring `I<Name>` ↔ `<Name>Impl`), so a call
    // to an interface method descends into the implementation.
    "declares_implementation",
];

/// Framework / language-runtime method names whose `calls` edges are almost
/// always noise. Mirrors `graph::NOISE_TARGET_METHODS`.
const NOISE_TARGET_METHODS: &[&str] = &[
    "toString",
    "hashCode",
    "noSuchMethod",
    "runtimeType",
    "copyWith",
    "dispose",
    "initState",
    "build",
    "equals",
    "getClass",
    "valueOf",
];

#[derive(Debug, Clone)]
pub struct TraceOptions {
    pub repo_root: PathBuf,
    /// Symbol / endpoint to resolve as the seed(s) (same matching as `search`).
    pub query: String,
    /// Max nodes to include before truncating (anchor counts).
    pub max_nodes: usize,
    /// Max BFS depth (hops) from any seed.
    pub max_depth: usize,
    /// How many top search matches to use as seeds.
    pub max_seeds: usize,
    /// Keep framework-noise `calls` edges when `true`.
    pub include_noise: bool,
}

impl TraceOptions {
    pub fn new(repo_root: impl Into<PathBuf>, query: impl Into<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            query: query.into(),
            max_nodes: 400,
            max_depth: 12,
            max_seeds: 6,
            include_noise: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
    /// Shortest hop distance from any seed (seed = 0).
    pub depth: usize,
    /// Coarse architectural layer (controller / service / mapper / sql / table …).
    pub layer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceResult {
    pub query: String,
    pub seeds: Vec<String>,
    pub nodes: Vec<TraceNode>,
    pub edges: Vec<TraceEdge>,
    /// Distinct `DbTable` names reached (the data the endpoint ultimately touches).
    pub tables: Vec<String>,
    /// Node count per layer.
    pub layer_counts: BTreeMap<String, usize>,
    /// `true` when the closure hit `max_nodes` and stopped early.
    pub truncated: bool,
}

/// Open the repo's graph.db and trace the forward closure of `query`.
pub fn run_trace(options: TraceOptions) -> Result<TraceResult> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    run_trace_with_store(&store, options)
}

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
    let cfg: EngineConfig = serde_yml::from_str(&contents)
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

pub fn run_trace_with_store(store: &Store, options: TraceOptions) -> Result<TraceResult> {
    // 1. Resolve seeds via the same matching as `search`.
    let mut search_opts = SearchOptions::keywords(&options.repo_root, &options.query);
    search_opts.limit = options.max_seeds.max(1);
    let search = run_search_with_store(store, search_opts)
        .with_context(|| format!("resolving trace seeds for `{}`", options.query))?;
    let seeds: Vec<ArtifactId> = search
        .matches
        .iter()
        .take(options.max_seeds.max(1))
        .map(|m| ArtifactId::new(m.id.clone()))
        .collect();

    trace_forward(store, &options, seeds)
}

/// The pure BFS — separated so tests can drive it against a hand-built store.
fn trace_forward(
    store: &Store,
    options: &TraceOptions,
    seeds: Vec<ArtifactId>,
) -> Result<TraceResult> {
    let mut depth_of: BTreeMap<String, usize> = BTreeMap::new();
    let mut edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut queue: VecDeque<(ArtifactId, usize)> = VecDeque::new();
    let mut truncated = false;

    for s in &seeds {
        if depth_of.insert(s.as_str().to_string(), 0).is_none() {
            queue.push_back((s.clone(), 0));
        }
    }

    while let Some((node, depth)) = queue.pop_front() {
        if depth >= options.max_depth {
            continue;
        }
        for edge in store.list_edges_from(&node)? {
            let kind = edge.kind.as_str();
            if !TRACE_EDGE_KINDS.contains(&kind) {
                continue;
            }
            if !options.include_noise && kind == "calls" && is_noise_target(edge.to_id.as_str()) {
                continue;
            }
            let to = edge.to_id.as_str().to_string();
            edges.insert((node.as_str().to_string(), to.clone(), kind.to_string()));
            if !depth_of.contains_key(&to) {
                if depth_of.len() >= options.max_nodes {
                    truncated = true;
                    continue;
                }
                depth_of.insert(to.clone(), depth + 1);
                queue.push_back((edge.to_id.clone(), depth + 1));
            }
        }
    }

    // Hydrate node metadata + classify layers.
    let mut nodes: Vec<TraceNode> = Vec::with_capacity(depth_of.len());
    let mut tables: BTreeSet<String> = BTreeSet::new();
    let mut layer_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (id, depth) in &depth_of {
        let aid = ArtifactId::new(id.clone());
        let (kind, label, path) = match store.find_node(&aid)? {
            Some(n) => {
                let kind = n.kind.as_str().to_string();
                let label = n.name.clone().unwrap_or_else(|| display_label(id));
                if n.kind == NodeKind::DbTable {
                    tables.insert(label.clone());
                }
                (kind, label, n.path.clone())
            }
            None => ("unknown".to_string(), display_label(id), None),
        };
        let layer = classify_layer(&kind, path.as_deref(), &label);
        *layer_counts.entry(layer.clone()).or_default() += 1;
        nodes.push(TraceNode {
            id: id.clone(),
            kind,
            label,
            path,
            depth: *depth,
            layer,
        });
    }
    nodes.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.id.cmp(&b.id)));

    let edges: Vec<TraceEdge> = edges
        .into_iter()
        .map(|(from, to, kind)| TraceEdge { from, to, kind })
        .collect();

    Ok(TraceResult {
        query: options.query.clone(),
        seeds: seeds.iter().map(|s| s.as_str().to_string()).collect(),
        nodes,
        edges,
        tables: tables.into_iter().collect(),
        layer_counts,
        truncated,
    })
}

/// Strip an artifact id down to its trailing symbol name (handles both the
/// Java `::Class.method` and the Dart `#Class.method` id shapes).
fn display_label(id: &str) -> String {
    let tail = id.rsplit("::").next().unwrap_or(id);
    let tail = tail.rsplit('#').next().unwrap_or(tail);
    tail.to_string()
}

fn is_noise_target(id: &str) -> bool {
    let name = display_label(id);
    let method = name.rsplit('.').next().unwrap_or(&name);
    NOISE_TARGET_METHODS.contains(&method)
}

/// Coarse architectural layer for grouping/printing. Path-based heuristics
/// match the conventional `Controller` / `ServiceImpl` / `Service` / `Mapper`
/// Java naming; schema nodes are classified by kind.
fn classify_layer(kind: &str, path: Option<&str>, _label: &str) -> String {
    match kind {
        "db_table" => return "table".to_string(),
        "sql_mapper_stmt" => return "sql".to_string(),
        // The HTTP route is the entry point — the URL a client calls, sitting
        // just above its controller handler in the data-flow order.
        "http_route" => return "route".to_string(),
        _ => {}
    }
    let p = path.unwrap_or("");
    if p.contains("Controller") || p.contains("/controller/") {
        "controller".to_string()
    } else if p.contains("ServiceImpl") || p.contains("/impl/") {
        "service_impl".to_string()
    } else if p.contains("Service") || p.contains("/service/") {
        "service".to_string()
    } else if p.contains("Mapper") || p.contains("/mapper/") || p.contains("/dao/") {
        "mapper".to_string()
    } else {
        "other".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{EdgeAssertion, EdgeKind, EdgeSource, Node};

    fn method(id: &str, path: &str) -> Node {
        let mut n = Node::new(ArtifactId::new(id), NodeKind::JavaMethod);
        n.path = Some(path.to_string());
        n
    }

    #[test]
    fn forward_closure_reaches_table_through_mapper_and_sql() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();

        // controller -> service -> impl -> mapper(method) -> sql -> table
        let ctrl = method(
            "java::a/CraftController.java::CraftController.selectCraftTree",
            "a/controller/CraftController.java",
        );
        let svc = method(
            "java::a/ICraftService.java::ICraftService.selectCraftTree",
            "a/service/ICraftService.java",
        );
        let imp = method(
            "java::a/CraftServiceImpl.java::CraftServiceImpl.selectCraftTree",
            "a/service/impl/CraftServiceImpl.java",
        );
        let mapper = method(
            "java::a/CraftMapper.java::CraftMapper.selectTree",
            "a/mapper/CraftMapper.java",
        );
        for n in [&ctrl, &svc, &imp, &mapper] {
            store.upsert_node(n).unwrap();
        }
        let stmt = {
            let mut n = Node::new(
                ArtifactId::new("sql_mapper::CraftMapper.xml::selectTree"),
                NodeKind::SqlMapperStmt,
            );
            n.name = Some("selectTree".to_string());
            n
        };
        let table = {
            let mut n = Node::new(
                ArtifactId::new("db_table::schema.sql::craft"),
                NodeKind::DbTable,
            );
            n.name = Some("craft".to_string());
            n
        };
        store.upsert_node(&stmt).unwrap();
        store.upsert_node(&table).unwrap();

        let mk = |from: &str, to: &str, kind: EdgeKind| {
            EdgeAssertion::fact(
                ArtifactId::new(from),
                ArtifactId::new(to),
                kind,
                EdgeSource::LanguageAdapter,
            )
        };
        store
            .upsert_edge(&mk(ctrl.id.as_str(), svc.id.as_str(), EdgeKind::Calls))
            .unwrap();
        store
            .upsert_edge(&mk(svc.id.as_str(), imp.id.as_str(), EdgeKind::Calls))
            .unwrap();
        store
            .upsert_edge(&mk(imp.id.as_str(), mapper.id.as_str(), EdgeKind::Calls))
            .unwrap();
        store
            .upsert_edge(&mk(
                mapper.id.as_str(),
                stmt.id.as_str(),
                EdgeKind::References,
            ))
            .unwrap();
        store
            .upsert_edge(&mk(
                stmt.id.as_str(),
                table.id.as_str(),
                EdgeKind::PersistsTo,
            ))
            .unwrap();
        // a framework-noise call that must be filtered out by default.
        let noise = method("java::a/X.java::Object.toString", "a/X.java");
        store.upsert_node(&noise).unwrap();
        store
            .upsert_edge(&mk(imp.id.as_str(), noise.id.as_str(), EdgeKind::Calls))
            .unwrap();

        let opts = TraceOptions::new(dir.path(), "selectCraftTree");
        let res = trace_forward(&store, &opts, vec![ctrl.id.clone()]).unwrap();

        assert_eq!(
            res.tables,
            vec!["craft".to_string()],
            "endpoint must reach the table"
        );
        assert!(
            res.nodes.iter().any(|n| n.layer == "sql"),
            "SQL layer present"
        );
        assert!(
            res.nodes.iter().any(|n| n.layer == "mapper"),
            "mapper layer present"
        );
        assert!(
            !res.nodes.iter().any(|n| n.label == "toString"),
            "framework-noise call must be filtered: {:?}",
            res.nodes
        );
        // depth: ctrl=0, svc=1, impl=2, mapper=3, sql=4, table=5
        let table_node = res.nodes.iter().find(|n| n.layer == "table").unwrap();
        assert_eq!(table_node.depth, 5);
    }

    #[test]
    fn forward_closure_starts_from_http_route_when_path_differs_from_method() {
        // The motivating case: tailorx calls `/style-info/getMeasuresInfo`, but
        // the Java handler is named `measuresInfo`. Starting the trace from the
        // HttpRoute (resolved by the URL segment) must descend route -> handler
        // -> ... -> table, with the route grouped in the top `route` layer.
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();

        let route = {
            let mut n = Node::new(
                ArtifactId::new(
                    "http_route::a/StyleInfoController.java::GET /style-info/getMeasuresInfo",
                ),
                NodeKind::HttpRoute,
            );
            n.name = Some("getMeasuresInfo".to_string());
            n.path = Some("/style-info/getMeasuresInfo".to_string());
            n
        };
        let handler = method(
            "java::a/StyleInfoController.java::StyleInfoController.measuresInfo",
            "a/controller/StyleInfoController.java",
        );
        let table = {
            let mut n = Node::new(
                ArtifactId::new("db_table::schema.sql::size_value"),
                NodeKind::DbTable,
            );
            n.name = Some("size_value".to_string());
            n
        };
        for n in [&route, &handler, &table] {
            store.upsert_node(n).unwrap();
        }
        let mk = |from: &str, to: &str, kind: EdgeKind| {
            EdgeAssertion::fact(
                ArtifactId::new(from),
                ArtifactId::new(to),
                kind,
                EdgeSource::LanguageAdapter,
            )
        };
        // route --references--> handler (schema indexer), handler --persists_to--> table.
        store
            .upsert_edge(&mk(
                route.id.as_str(),
                handler.id.as_str(),
                EdgeKind::References,
            ))
            .unwrap();
        store
            .upsert_edge(&mk(
                handler.id.as_str(),
                table.id.as_str(),
                EdgeKind::PersistsTo,
            ))
            .unwrap();

        let opts = TraceOptions::new(dir.path(), "getMeasuresInfo");
        let res = trace_forward(&store, &opts, vec![route.id.clone()]).unwrap();

        assert_eq!(
            res.tables,
            vec!["size_value".to_string()],
            "route trace must reach the table the handler persists to"
        );
        assert!(
            res.nodes.iter().any(|n| n.layer == "route" && n.depth == 0),
            "the HTTP route is the seed and belongs to the `route` layer: {:?}",
            res.nodes
        );
        assert!(
            res.nodes
                .iter()
                .any(|n| n.label.ends_with("measuresInfo") && n.depth == 1),
            "handler reached at depth 1 even though path segment != method name: {:?}",
            res.nodes
        );
    }

    #[test]
    fn max_depth_stops_before_table() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        let a = method("java::a::A.m", "a/controller/A.java");
        let b = method("java::a::B.m", "a/service/B.java");
        store.upsert_node(&a).unwrap();
        store.upsert_node(&b).unwrap();
        store
            .upsert_edge(&EdgeAssertion::fact(
                a.id.clone(),
                b.id.clone(),
                EdgeKind::Calls,
                EdgeSource::LanguageAdapter,
            ))
            .unwrap();
        let mut opts = TraceOptions::new(dir.path(), "A");
        opts.max_depth = 0; // no expansion at all
        let res = trace_forward(&store, &opts, vec![a.id.clone()]).unwrap();
        assert_eq!(res.nodes.len(), 1, "depth 0 keeps only the seed");
    }
}
