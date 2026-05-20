//! `explain_symbol` — bundle node, neighbours, tests and business
//! candidates that reference one symbol id. Designed so an agent can
//! answer "what is this symbol?" in one tool call.

use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use specslice_core::ArtifactId;

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, open_store, resolve_repo_root};

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "explain_symbol",
        description: "Produce a structured explanation of a single symbol: node \
            metadata, 1-hop upstream/downstream edges grouped by kind, \
            attached tests, and any business candidate whose evidence \
            cites this symbol. Intended as the canonical 'what is this?' \
            tool for agents that received a symbol id from search results \
            or impact reports.",
        input_schema: object_schema(
            json!({
                "symbol_id": {
                    "type": "string",
                    "description": "ArtifactId of the symbol to explain."
                },
                "repo_root": {
                    "type": "string",
                    "description": "Override the default repo root for this call."
                }
            }),
            &["symbol_id"],
        ),
    }
}

pub fn call(server: &Server, args: &Value) -> Result<Value> {
    let symbol_id = args
        .get("symbol_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("`symbol_id` is required"))?
        .to_string();
    let repo_root = resolve_repo_root(server, args);
    let store = open_store(&repo_root)?;

    let aid = ArtifactId::new(symbol_id.clone());
    let node = store
        .find_node(&aid)
        .with_context(|| format!("loading node `{symbol_id}`"))?
        .ok_or_else(|| anyhow!("node `{symbol_id}` not found in graph store"))?;

    let mut upstream: Vec<Value> = Vec::new();
    let mut downstream: Vec<Value> = Vec::new();
    let mut tests: Vec<Value> = Vec::new();
    let mut neighbour_ids: HashSet<String> = HashSet::new();

    for edge in store.list_edges_from(&aid)? {
        neighbour_ids.insert(edge.to_id.to_string());
        let neighbor = store.find_node(&edge.to_id)?;
        downstream.push(json!({
            "edge_id": edge.id.to_string(),
            "edge_kind": edge.kind.as_str(),
            "neighbor_id": edge.to_id.to_string(),
            "neighbor_kind": neighbor.as_ref().map(|n| n.kind.as_str()),
            "neighbor_label": neighbor.as_ref().and_then(|n| n.name.clone()).unwrap_or_else(|| edge.to_id.to_string()),
            "neighbor_path": neighbor.as_ref().and_then(|n| n.path.clone()),
            "source_file": edge.source_file.clone(),
        }));
    }
    for edge in store.list_edges_to(&aid)? {
        neighbour_ids.insert(edge.from_id.to_string());
        let neighbor = store.find_node(&edge.from_id)?;
        if edge.kind.as_str() == "declares_verification" {
            if let Some(n) = &neighbor {
                if matches!(
                    n.kind,
                    specslice_core::NodeKind::TestCase | specslice_core::NodeKind::TestGroup
                ) {
                    tests.push(json!({
                        "id": n.id.to_string(),
                        "label": n.name.clone().unwrap_or_else(|| n.id.to_string()),
                        "path": n.path.clone(),
                        "line_range": match (n.start_line, n.end_line) {
                            (Some(s), Some(e)) => Some(json!([s, e])),
                            _ => None,
                        },
                    }));
                }
            }
        }
        upstream.push(json!({
            "edge_id": edge.id.to_string(),
            "edge_kind": edge.kind.as_str(),
            "neighbor_id": edge.from_id.to_string(),
            "neighbor_kind": neighbor.as_ref().map(|n| n.kind.as_str()),
            "neighbor_label": neighbor.as_ref().and_then(|n| n.name.clone()).unwrap_or_else(|| edge.from_id.to_string()),
            "neighbor_path": neighbor.as_ref().and_then(|n| n.path.clone()),
            "source_file": edge.source_file.clone(),
        }));
    }

    // Group edges by kind for the inspector view.
    let mut grouped_up: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut grouped_down: serde_json::Map<String, Value> = serde_json::Map::new();
    for row in &upstream {
        if let Some(k) = row.get("edge_kind").and_then(|v| v.as_str()) {
            grouped_up
                .entry(k.to_string())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .expect("array")
                .push(row.clone());
        }
    }
    for row in &downstream {
        if let Some(k) = row.get("edge_kind").and_then(|v| v.as_str()) {
            grouped_down
                .entry(k.to_string())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .expect("array")
                .push(row.clone());
        }
    }

    // Look for business candidates whose evidence cites this symbol.
    let mut candidate_refs: Vec<Value> = Vec::new();
    if let Ok(doc) = specslice_engine::load_business_candidates(&repo_root) {
        for c in doc.document.candidates {
            if c.evidence.iter().any(|e| e == &symbol_id) {
                candidate_refs.push(json!({
                    "id": specslice_engine::candidate_artifact_id(&c.id),
                    "candidate_id": c.id,
                    "name": c.name,
                    "status": c.review_status().map(|s| match s {
                        specslice_engine::ReviewStatus::Accepted => "accepted",
                        specslice_engine::ReviewStatus::Rejected => "rejected",
                        specslice_engine::ReviewStatus::NeedsChanges => "needs_changes",
                        specslice_engine::ReviewStatus::Pending => "pending",
                    }).unwrap_or("unreviewed"),
                    "confidence": c.confidence,
                    "description": c.description,
                }));
            }
        }
    }

    Ok(json!({
        "node": {
            "id": node.id.to_string(),
            "kind": node.kind.as_str(),
            "name": node.name,
            "path": node.path,
            "line_range": match (node.start_line, node.end_line) {
                (Some(s), Some(e)) => Some(json!([s, e])),
                _ => None,
            },
            "source": node.indexer,
        },
        "upstream": upstream,
        "downstream": downstream,
        "upstream_by_kind": grouped_up,
        "downstream_by_kind": grouped_down,
        "tests": tests,
        "candidates_referencing": candidate_refs,
        "stats": {
            "upstream_count": upstream.len(),
            "downstream_count": downstream.len(),
            "neighbour_count": neighbour_ids.len(),
            "test_count": tests.len(),
            "candidates_count": candidate_refs.len(),
        }
    }))
}
