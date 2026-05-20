//! `get_subgraph` — expand 1..N hops around a known node id, with edge
//! kind filtering. Mirrors what `specslice search` does internally but
//! lets the agent jump straight to a node it already knows.

use std::collections::{BTreeSet, HashSet, VecDeque};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use specslice_core::ArtifactId;
use specslice_engine::search::EXPANSION_EDGE_KINDS;
use specslice_engine::SEARCH_DEFAULT_DEPTH;

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, open_store, parse_edge_kinds, resolve_repo_root};

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "get_subgraph",
        description: "Return the N-hop subgraph around a node id. Useful for \
            programmatic graph drill-down once `search_graph` has found a \
            relevant anchor. Edges are filtered to fact-bearing kinds \
            (calls / references / persists_to / navigates_to / \
            reads_provider / subscribes_stream / derives_from / \
            declares_implementation / declares_verification / contains).",
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
        .unwrap_or(SEARCH_DEFAULT_DEPTH);
    let edge_kinds_filter = parse_edge_kinds(args.get("edge_kinds").unwrap_or(&Value::Null))?;
    let include_noise = args
        .get("include_noise")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let repo_root = resolve_repo_root(server, args);
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

    while let Some((id, hop)) = queue.pop_front() {
        if hop >= depth {
            continue;
        }
        let aid = ArtifactId::new(id.clone());
        for edge in store.list_edges_from(&aid)? {
            let kind_str = edge.kind.as_str();
            if !allow_edge_kind.contains(kind_str) {
                continue;
            }
            if !include_noise && is_noise_calls(kind_str, edge.to_id.as_str()) {
                continue;
            }
            let edge_id = edge.id.to_string();
            if seen_edges.insert(edge_id.clone()) {
                edges_out.push(edge_to_json(&edge));
            }
            let other = edge.to_id.to_string();
            if visited.insert(other.clone()) {
                if let Some(n) = store.find_node(&edge.to_id)? {
                    nodes_out.push(node_to_json(&n));
                }
                queue.push_back((other, hop + 1));
            }
        }
        for edge in store.list_edges_to(&aid)? {
            let kind_str = edge.kind.as_str();
            if !allow_edge_kind.contains(kind_str) {
                continue;
            }
            if !include_noise && is_noise_calls(kind_str, edge.to_id.as_str()) {
                continue;
            }
            let edge_id = edge.id.to_string();
            if seen_edges.insert(edge_id.clone()) {
                edges_out.push(edge_to_json(&edge));
            }
            let other = edge.from_id.to_string();
            if visited.insert(other.clone()) {
                if let Some(n) = store.find_node(&edge.from_id)? {
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
        "nodes": nodes_out,
        "edges": edges_out,
    }))
}

fn node_to_json(node: &specslice_core::Node) -> Value {
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

fn edge_to_json(edge: &specslice_core::EdgeAssertion) -> Value {
    json!({
        "id": edge.id.to_string(),
        "from": edge.from_id.to_string(),
        "to": edge.to_id.to_string(),
        "kind": edge.kind.as_str(),
        "source_file": edge.source_file.clone(),
        "evidence_json": edge.evidence_json.clone(),
    })
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
