//! `context_pack` — assemble an agent-ready context pack for a
//! requirement, business candidate or code symbol.
//!
//! Mirrors `specslice context` for requirement_id, then extends to
//! candidate / symbol modes that the CLI does not currently expose.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use specslice_core::ArtifactId;
use specslice_engine::{build_context, ContextOptions};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, open_store, resolve_repo_root};

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "context_pack",
        description: "Build an agent-ready context pack. Supply exactly one of \
            `requirement_id`, `candidate_id` or `symbol_id`. For requirements \
            this mirrors `specslice context` (slice + docs + tests + \
            implementation snippets). For candidates and symbols we \
            synthesize the equivalent: anchor metadata, neighbours, \
            tests, and optionally inline source snippets.",
        input_schema: object_schema(
            json!({
                "requirement_id": {
                    "type": "string",
                    "description": "Requirement id (e.g. `REQ-WATERMARK-001`)."
                },
                "candidate_id": {
                    "type": "string",
                    "description": "Business candidate id from business_logic.yaml."
                },
                "symbol_id": {
                    "type": "string",
                    "description": "Code symbol ArtifactId."
                },
                "include_snippets": {
                    "type": "boolean",
                    "default": true,
                    "description": "Inline relevant doc/code/test source snippets."
                },
                "repo_root": {
                    "type": "string",
                    "description": "Override the default repo root for this call."
                }
            }),
            &[],
        ),
    }
}

pub fn call(server: &Server, args: &Value) -> Result<Value> {
    let repo_root = resolve_repo_root(server, args);
    let include_snippets = args
        .get("include_snippets")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let requirement_id = args.get("requirement_id").and_then(|v| v.as_str());
    let candidate_id = args.get("candidate_id").and_then(|v| v.as_str());
    let symbol_id = args.get("symbol_id").and_then(|v| v.as_str());

    let provided = [
        requirement_id.is_some(),
        candidate_id.is_some(),
        symbol_id.is_some(),
    ]
    .iter()
    .filter(|x| **x)
    .count();
    if provided != 1 {
        bail!("supply exactly one of `requirement_id`, `candidate_id`, `symbol_id`");
    }

    if let Some(req) = requirement_id {
        let pack = build_context(ContextOptions {
            repo_root: repo_root.clone(),
            requirement: req.to_string(),
            include_snippets,
        })
        .context("building requirement context pack")?;
        let value = serde_json::to_value(&pack)?;
        return Ok(json!({
            "mode": "requirement",
            "pack": value,
        }));
    }
    if let Some(cand) = candidate_id {
        return build_candidate_pack(&repo_root, cand, include_snippets);
    }
    let sym = symbol_id.expect("checked above");
    build_symbol_pack(&repo_root, sym, include_snippets)
}

fn build_candidate_pack(
    repo_root: &Path,
    candidate_id: &str,
    include_snippets: bool,
) -> Result<Value> {
    let doc = specslice_engine::load_business_candidates(repo_root)
        .context("loading business candidates")?;
    let candidate = doc
        .document
        .candidates
        .into_iter()
        .find(|c| c.id == candidate_id)
        .ok_or_else(|| {
            anyhow::anyhow!("candidate `{candidate_id}` not found in business_logic.yaml")
        })?;
    let store = open_store(repo_root)?;

    let mut evidence: Vec<Value> = Vec::new();
    let mut files_to_read: HashSet<String> = HashSet::new();
    for ev in &candidate.evidence {
        let aid = ArtifactId::new(ev.clone());
        let node = store.find_node(&aid).context("loading evidence node")?;
        let snippet = if include_snippets {
            node.as_ref()
                .and_then(|n| read_snippet(repo_root, n).ok().flatten())
        } else {
            None
        };
        if let Some(n) = &node {
            if let Some(p) = &n.path {
                files_to_read.insert(p.clone());
            }
        }
        evidence.push(json!({
            "id": ev,
            "node": node.as_ref().map(|n| json!({
                "id": n.id.to_string(),
                "kind": n.kind.as_str(),
                "name": n.name,
                "path": n.path,
                "line_range": match (n.start_line, n.end_line) {
                    (Some(s), Some(e)) => Some(json!([s, e])),
                    _ => None,
                },
            })),
            "snippet": snippet,
        }));
    }
    let mut files_to_read: Vec<String> = files_to_read.into_iter().collect();
    files_to_read.sort();

    Ok(json!({
        "mode": "candidate",
        "pack": {
            "candidate_id": candidate.id,
            "name": candidate.name,
            "description": candidate.description,
            "confidence": candidate.confidence,
            "status": candidate.review_status().map(|s| match s {
                specslice_engine::ReviewStatus::Accepted => "accepted",
                specslice_engine::ReviewStatus::Rejected => "rejected",
                specslice_engine::ReviewStatus::NeedsChanges => "needs_changes",
                specslice_engine::ReviewStatus::Pending => "pending",
            }).unwrap_or("unreviewed"),
            "risks": candidate.risks,
            "recommendation": candidate.recommendation,
            "open_questions": candidate.open_questions,
            "evidence": evidence,
            "files_to_read": files_to_read,
        }
    }))
}

fn build_symbol_pack(repo_root: &Path, symbol_id: &str, include_snippets: bool) -> Result<Value> {
    let store = open_store(repo_root)?;
    let aid = ArtifactId::new(symbol_id.to_string());
    let node = store
        .find_node(&aid)
        .with_context(|| format!("loading symbol `{symbol_id}`"))?
        .ok_or_else(|| anyhow::anyhow!("symbol `{symbol_id}` not found"))?;

    let mut neighbours: Vec<Value> = Vec::new();
    let mut tests: Vec<Value> = Vec::new();
    let mut files_to_read: HashSet<String> = HashSet::new();
    if let Some(p) = &node.path {
        files_to_read.insert(p.clone());
    }

    let snippet_self = if include_snippets {
        read_snippet(repo_root, &node).ok().flatten()
    } else {
        None
    };

    for edge in store.list_edges_from(&aid)? {
        if let Some(n) = store.find_node(&edge.to_id)? {
            if let Some(p) = &n.path {
                files_to_read.insert(p.clone());
            }
            neighbours.push(json!({
                "direction": "downstream",
                "edge_kind": edge.kind.as_str(),
                "neighbor_id": n.id.to_string(),
                "neighbor_kind": n.kind.as_str(),
                "neighbor_label": n.name.clone(),
                "neighbor_path": n.path.clone(),
                "line_range": match (n.start_line, n.end_line) {
                    (Some(s), Some(e)) => Some(json!([s, e])),
                    _ => None,
                },
            }));
        }
    }
    for edge in store.list_edges_to(&aid)? {
        if let Some(n) = store.find_node(&edge.from_id)? {
            if let Some(p) = &n.path {
                files_to_read.insert(p.clone());
            }
            if edge.kind.as_str() == "declares_verification"
                && matches!(
                    n.kind,
                    specslice_core::NodeKind::TestCase | specslice_core::NodeKind::TestGroup
                )
            {
                tests.push(json!({
                    "id": n.id.to_string(),
                    "label": n.name.clone(),
                    "path": n.path.clone(),
                    "line_range": match (n.start_line, n.end_line) {
                        (Some(s), Some(e)) => Some(json!([s, e])),
                        _ => None,
                    },
                }));
            }
            neighbours.push(json!({
                "direction": "upstream",
                "edge_kind": edge.kind.as_str(),
                "neighbor_id": n.id.to_string(),
                "neighbor_kind": n.kind.as_str(),
                "neighbor_label": n.name.clone(),
                "neighbor_path": n.path.clone(),
                "line_range": match (n.start_line, n.end_line) {
                    (Some(s), Some(e)) => Some(json!([s, e])),
                    _ => None,
                },
            }));
        }
    }

    let mut files_to_read: Vec<String> = files_to_read.into_iter().collect();
    files_to_read.sort();

    Ok(json!({
        "mode": "symbol",
        "pack": {
            "node": {
                "id": node.id.to_string(),
                "kind": node.kind.as_str(),
                "name": node.name,
                "path": node.path,
                "line_range": match (node.start_line, node.end_line) {
                    (Some(s), Some(e)) => Some(json!([s, e])),
                    _ => None,
                },
            },
            "snippet": snippet_self,
            "neighbours": neighbours,
            "tests": tests,
            "files_to_read": files_to_read,
        }
    }))
}

fn read_snippet(repo_root: &Path, node: &specslice_core::Node) -> Result<Option<String>> {
    let Some(rel_path) = &node.path else {
        return Ok(None);
    };
    let Some(start) = node.start_line else {
        return Ok(None);
    };
    let end = node.end_line.unwrap_or(start);
    if end < start {
        return Ok(None);
    }
    let abs = repo_root.join(rel_path);
    if !abs.exists() {
        return Ok(None);
    }
    let body =
        std::fs::read_to_string(&abs).with_context(|| format!("reading {}", abs.display()))?;
    let start = start.saturating_sub(1) as usize;
    let end = (end as usize).min(body.lines().count());
    let slice: String = body
        .lines()
        .skip(start)
        .take(end.saturating_sub(start).max(1))
        .collect::<Vec<&str>>()
        .join("\n");
    Ok(Some(slice))
}
