//! `explain_symbol` — bundle node, neighbours, tests and business
//! candidates that reference one symbol id. Designed so an agent can
//! answer "what is this symbol?" in one tool call.

use std::collections::{BTreeMap, HashSet};

use anyhow::{anyhow, Context, Result};
use groundgraph_core::ArtifactId;
use serde::Serialize;
use serde_json::{json, Value};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, open_store, resolve_repo_root};

/// A high-fan-out hub (Java `Object.equals`, Spring `ApplicationContext`) can
/// have a 5-digit edge count; serialising them all into one `tools/call` text
/// block tears the MCP frame on clients with a per-content-block size cap.
/// Bound the materialised edges (upstream first so attached tests survive) and
/// flag `truncated` so the agent drills down with `get_subgraph` — the same
/// defence `get_subgraph` got in issues.md #9, here for `explain_symbol` (#87).
const MAX_EXPLAIN_EDGES: usize = 500;

/// One row of the upstream/downstream edge listing — a typed mirror of the
/// `json!({...})` value `explain_symbol` used to build per edge (issues.md
/// #162). Field order is alphabetical to match the prior `serde_json::Map`
/// output (built without `preserve_order` ⇒ BTreeMap key order), so the
/// serialised shape — and thus the MCP contract — is byte-identical.
#[derive(Serialize, Clone)]
struct ExplainEdgeRow {
    edge_id: String,
    edge_kind: &'static str,
    neighbor_id: String,
    neighbor_kind: Option<&'static str>,
    neighbor_label: String,
    neighbor_path: Option<String>,
    source_file: Option<String>,
}

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
    let repo_root = resolve_repo_root(server, args)?;
    let store = open_store(&repo_root)?;

    let aid = ArtifactId::new(symbol_id.clone());
    let node = store
        .find_node(&aid)
        .with_context(|| format!("loading node `{symbol_id}`"))?
        .ok_or_else(|| anyhow!("node `{symbol_id}` not found in graph store"))?;

    let mut upstream: Vec<ExplainEdgeRow> = Vec::new();
    let mut downstream: Vec<ExplainEdgeRow> = Vec::new();
    let mut tests: Vec<Value> = Vec::new();
    let mut neighbour_ids: HashSet<String> = HashSet::new();
    // Bucket edges by kind while iterating (#162): previously every edge was
    // built as a `serde_json::Value` (a `Map<String, Value>` + its strings),
    // pushed into `upstream`/`downstream`, then a second pass `.clone()`-d
    // each `Value` into the grouped map. A typed `ExplainEdgeRow` serialised
    // straight to the MCP shape, plus a single iteration that fills both the
    // flat list and the buckets, drops the per-edge `Map` allocation and the
    // clone loop. `BTreeMap` keeps the prior alphabetised key order (serde_json
    // is built without `preserve_order`).
    let mut grouped_up: BTreeMap<&'static str, Vec<ExplainEdgeRow>> = BTreeMap::new();
    let mut grouped_down: BTreeMap<&'static str, Vec<ExplainEdgeRow>> = BTreeMap::new();

    let from_edges = store.list_edges_from(&aid)?;
    let to_edges = store.list_edges_to(&aid)?;
    let total_edges = from_edges.len() + to_edges.len();
    let truncated = total_edges > MAX_EXPLAIN_EDGES;

    // Upstream (in-edges) first so the `declares_verification` tests survive
    // truncation even when it's the downstream fan-out that blew the budget.
    let up_taken = to_edges.len().min(MAX_EXPLAIN_EDGES);
    for edge in to_edges.iter().take(MAX_EXPLAIN_EDGES) {
        neighbour_ids.insert(edge.from_id.to_string());
        let neighbor = store.find_node(&edge.from_id)?;
        if edge.kind.as_str() == "declares_verification" {
            if let Some(n) = &neighbor {
                if matches!(
                    n.kind,
                    groundgraph_core::NodeKind::TestCase | groundgraph_core::NodeKind::TestGroup
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
        let row = ExplainEdgeRow {
            edge_id: edge.id.to_string(),
            edge_kind: edge.kind.as_str(),
            neighbor_id: edge.from_id.to_string(),
            neighbor_kind: neighbor.as_ref().map(|n| n.kind.as_str()),
            neighbor_label: neighbor
                .as_ref()
                .and_then(|n| n.name.clone())
                .unwrap_or_else(|| edge.from_id.to_string()),
            neighbor_path: neighbor.as_ref().and_then(|n| n.path.clone()),
            source_file: edge.source_file.clone(),
        };
        grouped_up
            .entry(edge.kind.as_str())
            .or_default()
            .push(row.clone());
        upstream.push(row);
    }
    let down_budget = MAX_EXPLAIN_EDGES.saturating_sub(up_taken);
    for edge in from_edges.iter().take(down_budget) {
        neighbour_ids.insert(edge.to_id.to_string());
        let neighbor = store.find_node(&edge.to_id)?;
        let row = ExplainEdgeRow {
            edge_id: edge.id.to_string(),
            edge_kind: edge.kind.as_str(),
            neighbor_id: edge.to_id.to_string(),
            neighbor_kind: neighbor.as_ref().map(|n| n.kind.as_str()),
            neighbor_label: neighbor
                .as_ref()
                .and_then(|n| n.name.clone())
                .unwrap_or_else(|| edge.to_id.to_string()),
            neighbor_path: neighbor.as_ref().and_then(|n| n.path.clone()),
            source_file: edge.source_file.clone(),
        };
        grouped_down
            .entry(edge.kind.as_str())
            .or_default()
            .push(row.clone());
        downstream.push(row);
    }

    // Look for business candidates whose evidence cites this symbol.
    let mut candidate_refs: Vec<Value> = Vec::new();
    if let Ok(doc) = groundgraph_engine::load_business_candidates(&repo_root) {
        for c in doc.document.candidates {
            if c.evidence.iter().any(|e| e == &symbol_id) {
                candidate_refs.push(json!({
                    "id": groundgraph_engine::candidate_artifact_id(&c.id),
                    "candidate_id": c.id,
                    "name": c.name,
                    "status": c.review_status().map(|s| match s {
                        groundgraph_engine::ReviewStatus::Accepted => "accepted",
                        groundgraph_engine::ReviewStatus::Rejected => "rejected",
                        groundgraph_engine::ReviewStatus::NeedsChanges => "needs_changes",
                        groundgraph_engine::ReviewStatus::Pending => "pending",
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
        "truncated": truncated,
        "truncation_hint": if truncated {
            Some(format!(
                "symbol has {total_edges} edges; only the first {MAX_EXPLAIN_EDGES} were \
                 materialised. Use get_subgraph for the full neighbourhood."
            ))
        } else {
            None
        },
        "stats": {
            "upstream_count": upstream.len(),
            "downstream_count": downstream.len(),
            "neighbour_count": neighbour_ids.len(),
            "test_count": tests.len(),
            "candidates_count": candidate_refs.len(),
            "total_edges": total_edges,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{call, MAX_EXPLAIN_EDGES};
    use crate::server::Server;
    use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
    use serde_json::json;

    /// A hub with a downstream fan-out far past the budget, plus exactly one
    /// upstream `declares_verification` test edge.
    fn hub_store() -> (tempfile::TempDir, ArtifactId) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".groundgraph")).unwrap();
        std::fs::write(dir.path().join(".groundgraph.yaml"), "{}\n").unwrap();
        let mut store =
            groundgraph_store::Store::open(dir.path().join(".groundgraph/graph.db")).unwrap();
        store.migrate().unwrap();

        let hub = ArtifactId::new("java::Hub.java#Hub.equals".to_string());
        store
            .upsert_node(&Node::new(hub.clone(), NodeKind::JavaMethod))
            .unwrap();

        // Upstream test (declares_verification: test -> hub). Materialised
        // first, so it must survive truncation of the downstream fan-out.
        let test_id = ArtifactId::new("test::HubTest.java#HubTest.testEquals".to_string());
        let mut tn = Node::new(test_id.clone(), NodeKind::TestCase);
        tn.name = Some("testEquals".to_string());
        store.upsert_node(&tn).unwrap();
        let mut te = EdgeAssertion::fact(
            test_id,
            hub.clone(),
            EdgeKind::DeclaresVerification,
            EdgeSource::LanguageAdapter,
        );
        te.id = ArtifactId::new("edge::test".to_string());
        store.upsert_edge(&te).unwrap();

        for i in 0..(MAX_EXPLAIN_EDGES + 200) {
            let to = ArtifactId::new(format!("java::Leaf{i}.java#Leaf{i}.m"));
            store
                .upsert_node(&Node::new(to.clone(), NodeKind::JavaMethod))
                .unwrap();
            let mut e = EdgeAssertion::fact(
                hub.clone(),
                to,
                EdgeKind::Calls,
                EdgeSource::LanguageAdapter,
            );
            e.id = ArtifactId::new(format!("edge::down::{i}"));
            store.upsert_edge(&e).unwrap();
        }
        (dir, hub)
    }

    /// #87: a high-fan-out hub must not ship every edge in one frame — the
    /// response is capped, flags `truncated`, and still keeps attached tests.
    #[test]
    fn explain_caps_edges_keeps_tests_and_flags_truncation() {
        let (dir, hub) = hub_store();
        let server = Server::new(dir.path().to_path_buf());
        let out = call(&server, &json!({ "symbol_id": hub.to_string() })).unwrap();

        assert_eq!(out["truncated"], json!(true), "hub must report truncation");
        let up = out["upstream"].as_array().unwrap().len();
        let down = out["downstream"].as_array().unwrap().len();
        assert!(
            up + down <= MAX_EXPLAIN_EDGES,
            "materialised edges must respect the budget: {up}+{down}"
        );
        assert_eq!(
            out["tests"].as_array().unwrap().len(),
            1,
            "the declares_verification test must survive truncation (upstream first)"
        );
    }

    /// A hub with one upstream edge of each evidence kind (plus a downstream
    /// Calls + References), used to pin the full `explain_symbol` JSON shape.
    fn diverse_store() -> (tempfile::TempDir, ArtifactId) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".groundgraph")).unwrap();
        std::fs::write(dir.path().join(".groundgraph.yaml"), "{}\n").unwrap();
        let mut store =
            groundgraph_store::Store::open(dir.path().join(".groundgraph/graph.db")).unwrap();
        store.migrate().unwrap();

        let hub = ArtifactId::new("java::Hub.java#Hub.work".to_string());
        let mut hn = Node::new(hub.clone(), NodeKind::JavaMethod);
        hn.name = Some("work".into());
        hn.path = Some("Hub.java".into());
        store.upsert_node(&hn).unwrap();

        // mk_node borrows `store` mutably; scope it so that borrow ends before
        // `edge` (which also needs &mut store) is defined.
        let (doc, imp, tst, callee, reft) = {
            let mut mk_node = |id: &str, kind: NodeKind, name: &str, path: &str| -> ArtifactId {
                let aid = ArtifactId::new(id.to_string());
                let mut n = Node::new(aid.clone(), kind);
                n.name = Some(name.into());
                n.path = Some(path.into());
                store.upsert_node(&n).unwrap();
                aid
            };
            (
                mk_node("doc::a", NodeKind::DocSection, "Spec", "spec.md"),
                mk_node("py::a", NodeKind::PythonFunction, "do", "a.py"),
                mk_node("test::a", NodeKind::TestCase, "test_work", "a_test.py"),
                mk_node("java::b", NodeKind::JavaMethod, "helper", "B.java"),
                mk_node("java::c", NodeKind::JavaClass, "field", "C.java"),
            )
        };

        let mut edge = |id: &str, from: ArtifactId, to: ArtifactId, kind: EdgeKind| {
            let mut e = EdgeAssertion::fact(from, to, kind, EdgeSource::LanguageAdapter);
            e.id = ArtifactId::new(id.to_string());
            store.upsert_edge(&e).unwrap();
        };
        edge("e::doc", doc, hub.clone(), EdgeKind::Documents);
        edge(
            "e::impl",
            imp,
            hub.clone(),
            EdgeKind::DeclaresImplementation,
        );
        edge("e::verif", tst, hub.clone(), EdgeKind::DeclaresVerification);
        edge("e::call", hub.clone(), callee, EdgeKind::Calls);
        edge("e::ref", hub.clone(), reft, EdgeKind::References);
        (dir, hub)
    }

    /// issues.md #162: the `explain_symbol` JSON is an MCP output contract.
    /// Pin its key set, key order, row shape and counts so the typed-struct +
    /// bucket-while-iterating refactor cannot drift what clients see. Keys are
    /// alphabetised because `serde_json` is built without `preserve_order`
    /// (Map == BTreeMap semantics).
    #[test]
    fn explain_symbol_json_shape_pins_the_mcp_contract() {
        let (dir, hub) = diverse_store();
        let server = Server::new(dir.path().to_path_buf());
        let out = call(&server, &json!({ "symbol_id": hub.to_string() })).unwrap();
        let obj = out.as_object().expect("top-level is an object");

        // Top-level key set + (alphabetised) order.
        let top: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            top,
            vec![
                "candidates_referencing",
                "downstream",
                "downstream_by_kind",
                "node",
                "stats",
                "tests",
                "truncated",
                "truncation_hint",
                "upstream",
                "upstream_by_kind",
            ]
        );

        // grouped-by-kind keys are alphabetised.
        let up: Vec<&str> = out["upstream_by_kind"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            up,
            vec![
                "declares_implementation",
                "declares_verification",
                "documents"
            ]
        );
        let down: Vec<&str> = out["downstream_by_kind"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(down, vec!["calls", "references"]);

        // Each edge row exposes exactly this key set, in this order.
        let row: Vec<&str> = out["upstream"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            row,
            vec![
                "edge_id",
                "edge_kind",
                "neighbor_id",
                "neighbor_kind",
                "neighbor_label",
                "neighbor_path",
                "source_file",
            ]
        );

        // declares_verification + TestCase neighbour → captured into `tests`.
        assert_eq!(out["tests"].as_array().unwrap().len(), 1);
        assert_eq!(out["stats"]["upstream_count"], json!(3));
        assert_eq!(out["stats"]["downstream_count"], json!(2));
        assert_eq!(out["stats"]["test_count"], json!(1));
        assert_eq!(out["truncated"], json!(false));
        assert_eq!(out["truncation_hint"], json!(null));
    }
}
