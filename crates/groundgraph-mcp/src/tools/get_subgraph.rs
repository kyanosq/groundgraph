//! `get_subgraph` — expand 1..N hops around a known node id, with edge
//! kind filtering. Mirrors what `groundgraph search` does internally but
//! lets the agent jump straight to a node it already knows.

use std::collections::{BTreeSet, HashSet, VecDeque};

use anyhow::{anyhow, bail, Context, Result};
use groundgraph_core::ArtifactId;
use groundgraph_engine::search::EXPANSION_EDGE_KINDS;
use groundgraph_engine::SEARCH_DEFAULT_DEPTH;
use serde_json::{json, Value};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, open_store, parse_edge_kinds, resolve_repo_root};

/// Budgets for the BFS expansion. The whole subgraph is shipped back as a
/// single JSON-RPC line, so an unbounded walk on a dense graph would buffer
/// arbitrarily much memory and produce an unusable reply (issues.md #9).
/// When a budget trips, the response carries `"truncated": true`.
const MAX_SUBGRAPH_DEPTH: usize = 16;
const MAX_SUBGRAPH_NODES: usize = 2_000;
const MAX_SUBGRAPH_EDGES: usize = 8_000;

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "get_subgraph",
        description: "Return the N-hop subgraph around a node id. Useful for \
            programmatic graph drill-down once `search_graph` has found a \
            relevant anchor. Edges are filtered to fact-bearing kinds \
            (calls / references / persists_to / navigates_to / \
            reads_provider / subscribes_stream / derives_from / \
            declares_implementation / declares_verification / contains). \
            Optional `resolvers` lets callers narrow expansion to edges \
            produced by a specific indexer label (e.g. `swift_lsp`, \
            `go_lsp`, `dart_analyzer`) so debugging cross-language \
            provenance is straightforward.",
        input_schema: object_schema(
            json!({
                "node_id": {
                    "type": "string",
                    "description": "ArtifactId, e.g. `dart_method::lib/auth/auth_service.dart#AuthService.signIn`."
                },
                "depth": {
                    "type": "integer",
                    "minimum": 0,
                    "default": SEARCH_DEFAULT_DEPTH,
                    "description": "Number of hops to walk. 0 returns just the anchor node."
                },
                "edge_kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Restrict expansion to these edge kinds. Defaults to all expansion kinds."
                },
                "resolvers": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Restrict expansion to edges whose `indexer` label is in this set (e.g. `swift_lsp`, `go_lsp`, `dart_analyzer`). Empty / omitted means no filter."
                },
                "include_noise": {
                    "type": "boolean",
                    "default": false,
                    "description": "Keep framework-noise `calls` to toString / build / dispose / etc."
                },
                "repo_root": {
                    "type": "string",
                    "description": "Override the default repo root for this call."
                }
            }),
            &["node_id"],
        ),
    }
}

pub fn call(server: &Server, args: &Value) -> Result<Value> {
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("`node_id` is required"))?
        .to_string();
    let depth = args
        .get("depth")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(SEARCH_DEFAULT_DEPTH)
        .min(MAX_SUBGRAPH_DEPTH);
    let edge_kinds_filter = parse_edge_kinds(args.get("edge_kinds").unwrap_or(&Value::Null))?;
    let resolvers_filter = parse_resolvers(args.get("resolvers").unwrap_or(&Value::Null))?;
    let include_noise = args
        .get("include_noise")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let repo_root = resolve_repo_root(server, args)?;
    let store = open_store(&repo_root)?;

    let anchor = store
        .find_node(&ArtifactId::new(node_id.clone()))
        .with_context(|| format!("loading anchor node `{node_id}`"))?
        .ok_or_else(|| anyhow!("node `{node_id}` not found in graph store"))?;

    let allow_edge_kind: HashSet<String> = if edge_kinds_filter.is_empty() {
        EXPANSION_EDGE_KINDS.iter().map(|s| s.to_string()).collect()
    } else {
        edge_kinds_filter.into_iter().collect()
    };

    let mut visited: BTreeSet<String> = BTreeSet::new();
    visited.insert(anchor.id.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((anchor.id.to_string(), 0));

    let mut nodes_out: Vec<Value> = vec![node_to_json(&anchor)];
    let mut edges_out: Vec<Value> = Vec::new();
    let mut seen_edges: HashSet<String> = HashSet::new();
    let mut truncated = false;

    'walk: while let Some((id, hop)) = queue.pop_front() {
        if hop >= depth {
            continue;
        }
        let aid = ArtifactId::new(id.clone());
        let outgoing = store.list_edges_from(&aid)?.into_iter().map(|e| {
            let other = e.to_id.clone();
            (e, other)
        });
        let incoming = store.list_edges_to(&aid)?.into_iter().map(|e| {
            let other = e.from_id.clone();
            (e, other)
        });
        for (edge, other_id) in outgoing.chain(incoming) {
            if nodes_out.len() >= MAX_SUBGRAPH_NODES || edges_out.len() >= MAX_SUBGRAPH_EDGES {
                truncated = true;
                break 'walk;
            }
            let kind_str = edge.kind.as_str();
            if !allow_edge_kind.contains(kind_str) {
                continue;
            }
            if !resolver_allowed(&edge, &resolvers_filter) {
                continue;
            }
            if !include_noise && is_noise_calls(kind_str, edge.to_id.as_str()) {
                continue;
            }
            let edge_id = edge.id.to_string();
            if seen_edges.insert(edge_id.clone()) {
                edges_out.push(edge_to_json(&edge));
            }
            let other = other_id.to_string();
            if visited.insert(other.clone()) {
                if let Some(n) = store.find_node(&other_id)? {
                    nodes_out.push(node_to_json(&n));
                }
                queue.push_back((other, hop + 1));
            }
        }
    }

    nodes_out.sort_by(|a, b| {
        a.get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .cmp(b.get("id").and_then(|v| v.as_str()).unwrap_or_default())
    });
    edges_out.sort_by(|a, b| {
        a.get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .cmp(b.get("id").and_then(|v| v.as_str()).unwrap_or_default())
    });

    if depth > 0 && nodes_out.len() == 1 && edges_out.is_empty() {
        bail!(
            "node `{node_id}` has no edges in the configured kinds set. \
             Try widening `edge_kinds` or check the index ran with the sidecar."
        );
    }

    Ok(json!({
        "anchor_id": anchor.id.to_string(),
        "depth": depth,
        "truncated": truncated,
        "nodes": nodes_out,
        "edges": edges_out,
    }))
}

fn node_to_json(node: &groundgraph_core::Node) -> Value {
    json!({
        "id": node.id.to_string(),
        "kind": node.kind.as_str(),
        "name": node.name.clone(),
        "path": node.path.clone(),
        "line_range": match (node.start_line, node.end_line) {
            (Some(s), Some(e)) => Some(json!([s, e])),
            _ => None,
        },
    })
}

fn edge_to_json(edge: &groundgraph_core::EdgeAssertion) -> Value {
    json!({
        "id": edge.id.to_string(),
        "from": edge.from_id.to_string(),
        "to": edge.to_id.to_string(),
        "kind": edge.kind.as_str(),
        "source_file": edge.source_file.clone(),
        "evidence_json": edge.evidence_json.clone(),
    })
}

/// Parse the optional `resolvers` argument from `tools/call` params.
///
/// Accepts an array of strings (e.g. `["swift_lsp", "go_lsp"]`); empty
/// / missing / `null` disables filtering. We don't validate against a
/// closed set here because adapter labels evolve as we add languages;
/// `crates/groundgraph-engine/src/{swift,go,dart}_indexer.rs` are the
/// source of truth.
fn parse_resolvers(values: &Value) -> Result<HashSet<String>> {
    if values.is_null() {
        return Ok(HashSet::new());
    }
    let Some(arr) = values.as_array() else {
        bail!("`resolvers` must be an array of strings");
    };
    let mut out = HashSet::with_capacity(arr.len());
    for v in arr {
        let s = v
            .as_str()
            .ok_or_else(|| anyhow!("`resolvers` entries must be strings"))?;
        out.insert(s.trim().to_string());
    }
    out.remove("");
    Ok(out)
}

/// Empty `resolvers` = no filter. Otherwise the edge's `indexer` label
/// must be present in the set. Edges without an `indexer` (older data,
/// manifest-driven edges, etc.) are excluded once the filter is active
/// — callers asking "show me Swift LSP edges" don't want manifest
/// edges leaking in.
fn resolver_allowed(edge: &groundgraph_core::EdgeAssertion, allow: &HashSet<String>) -> bool {
    if allow.is_empty() {
        return true;
    }
    match edge.indexer.as_deref() {
        Some(name) => allow.contains(name),
        None => false,
    }
}

fn is_noise_calls(kind: &str, to_id: &str) -> bool {
    if kind != "calls" {
        return false;
    }
    const NOISE: &[&str] = &[
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
    let target = match to_id.split_once('#') {
        Some((_, tail)) => tail.rsplit_once('.').map(|(_, m)| m).unwrap_or(tail),
        None => "",
    };
    NOISE.contains(&target)
}

#[cfg(test)]
mod tests {
    use super::{call, parse_resolvers, resolver_allowed, MAX_SUBGRAPH_NODES};
    use crate::server::Server;
    use groundgraph_core::{
        ArtifactId, Confidence, EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus,
        Node, NodeKind,
    };
    use serde_json::json;

    fn make_edge(indexer: Option<&str>) -> EdgeAssertion {
        EdgeAssertion {
            id: ArtifactId::new("e".to_string()),
            from_id: ArtifactId::new("a".to_string()),
            to_id: ArtifactId::new("b".to_string()),
            kind: EdgeKind::Calls,
            source: EdgeSource::LanguageAdapter,
            certainty: EdgeCertainty::Fact,
            status: EdgeStatus::Confirmed,
            confidence: Confidence::FULL,
            evidence_json: None,
            source_file: None,
            indexer: indexer.map(|s| s.to_string()),
            metadata_json: None,
        }
    }

    /// A dense graph must not be buffered wholesale into one JSON-RPC
    /// line: the walk stops at the node budget and says so via
    /// `truncated` (issues.md #9).
    #[test]
    fn bfs_stops_at_the_node_budget_and_reports_truncation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".groundgraph")).unwrap();
        std::fs::write(dir.path().join(".groundgraph.yaml"), "{}\n").unwrap();
        let mut store =
            groundgraph_store::Store::open(dir.path().join(".groundgraph/graph.db")).unwrap();
        store.migrate().unwrap();

        let anchor = ArtifactId::new("dart_fn::lib/hub.dart#hub".to_string());
        store
            .upsert_node(&Node::new(anchor.clone(), NodeKind::DartFunction))
            .unwrap();
        let fan_out = MAX_SUBGRAPH_NODES + 500;
        for i in 0..fan_out {
            let to = ArtifactId::new(format!("dart_fn::lib/n{i}.dart#leaf{i}"));
            store
                .upsert_node(&Node::new(to.clone(), NodeKind::DartFunction))
                .unwrap();
            let mut edge = EdgeAssertion::fact(
                anchor.clone(),
                to,
                EdgeKind::Calls,
                EdgeSource::LanguageAdapter,
            );
            edge.id = ArtifactId::new(format!("edge::{i}"));
            store.upsert_edge(&edge).unwrap();
        }

        let server = Server::new(dir.path().to_path_buf());
        let out = call(
            &server,
            &json!({ "node_id": anchor.to_string(), "depth": 3 }),
        )
        .unwrap();
        let nodes = out["nodes"].as_array().unwrap();
        assert!(
            nodes.len() <= MAX_SUBGRAPH_NODES,
            "node budget must hold: got {}",
            nodes.len()
        );
        assert_eq!(
            out["truncated"],
            json!(true),
            "caller must learn the subgraph was cut short"
        );
    }

    #[test]
    fn parse_resolvers_accepts_string_array_and_rejects_other_shapes() {
        let parsed = parse_resolvers(&json!(["swift_lsp", "go_lsp"])).unwrap();
        assert!(parsed.contains("swift_lsp"));
        assert!(parsed.contains("go_lsp"));
        assert_eq!(parsed.len(), 2);

        // null and missing → empty filter (allow all).
        assert!(parse_resolvers(&serde_json::Value::Null)
            .unwrap()
            .is_empty());

        // wrong shape must surface an error so callers learn fast.
        assert!(parse_resolvers(&json!("swift_lsp")).is_err());
        assert!(parse_resolvers(&json!([1, 2, 3])).is_err());

        // empty strings get dropped so accidental whitespace doesn't filter to nothing.
        let parsed = parse_resolvers(&json!(["", "  ", "swift_lsp"])).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed.contains("swift_lsp"));
    }

    #[test]
    fn resolver_allowed_passes_through_when_filter_empty_and_filters_when_set() {
        let edge_swift = make_edge(Some("swift_lsp"));
        let edge_dart = make_edge(Some("dart_analyzer"));
        let edge_unknown = make_edge(None);

        // No filter → all edges flow through.
        let empty = parse_resolvers(&serde_json::Value::Null).unwrap();
        assert!(resolver_allowed(&edge_swift, &empty));
        assert!(resolver_allowed(&edge_dart, &empty));
        assert!(resolver_allowed(&edge_unknown, &empty));

        // Filter set → only matching indexer survives; missing indexer
        // is excluded so callers asking for `swift_lsp` don't see
        // manifest-driven edges leaking in.
        let only_swift = parse_resolvers(&json!(["swift_lsp"])).unwrap();
        assert!(resolver_allowed(&edge_swift, &only_swift));
        assert!(!resolver_allowed(&edge_dart, &only_swift));
        assert!(!resolver_allowed(&edge_unknown, &only_swift));
    }
}
