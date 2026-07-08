//! `context_pack` — assemble an agent-ready context pack for a
//! requirement, business candidate or code symbol.
//!
//! Mirrors `groundgraph context` for requirement_id, then extends to
//! candidate / symbol modes that the CLI does not currently expose.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use groundgraph_core::ArtifactId;
use groundgraph_engine::{build_context, ContextOptions};
use serde_json::{json, Value};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, open_store, resolve_repo_root};

/// Skip inlining a snippet from a file larger than this. Reading a 50 MB
/// generated bundle / minified file to surface a handful of lines is not worth
/// the allocation and risks OOM on a corrupt `end_line` (#88; mirrors
/// `search.rs` `SNIPPET_MAX_FILE_BYTES` / issues2.md #51).
const SNIPPET_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Cap the neighbours materialised for a symbol pack. A hub symbol can have a
/// 5-digit edge count; one `context_pack` call must not buffer them all into a
/// single response (#88; sibling of explain_symbol #87 / get_subgraph #9).
const MAX_PACK_NEIGHBORS: usize = 300;

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "context_pack",
        description: "Build an agent-ready context pack. Supply exactly one of \
            `requirement_id`, `candidate_id` or `symbol_id`. For requirements \
            this mirrors `groundgraph context` (slice + docs + tests + \
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
    let repo_root = resolve_repo_root(server, args)?;
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
    // `provided != 1` above guarantees `symbol_id` is the remaining option, but
    // guard explicitly so a future change to the validation can't turn this into
    // a panic on a malformed MCP request.
    let Some(sym) = symbol_id else {
        bail!("supply exactly one of `requirement_id`, `candidate_id`, `symbol_id`");
    };
    build_symbol_pack(&repo_root, sym, include_snippets)
}

fn build_candidate_pack(
    repo_root: &Path,
    candidate_id: &str,
    include_snippets: bool,
) -> Result<Value> {
    let doc = groundgraph_engine::load_business_candidates(repo_root)
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
                groundgraph_engine::ReviewStatus::Accepted => "accepted",
                groundgraph_engine::ReviewStatus::Rejected => "rejected",
                groundgraph_engine::ReviewStatus::NeedsChanges => "needs_changes",
                groundgraph_engine::ReviewStatus::Pending => "pending",
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

    let from_edges = store.list_edges_from(&aid)?;
    let to_edges = store.list_edges_to(&aid)?;
    let truncated = from_edges.len() + to_edges.len() > MAX_PACK_NEIGHBORS;

    // Upstream (incl. `declares_verification` tests) first so tests survive
    // truncation on a hub whose downstream fan-out blew the neighbour budget.
    let up_taken = to_edges.len().min(MAX_PACK_NEIGHBORS);
    for edge in to_edges.iter().take(MAX_PACK_NEIGHBORS) {
        if let Some(n) = store.find_node(&edge.from_id)? {
            if let Some(p) = &n.path {
                files_to_read.insert(p.clone());
            }
            if edge.kind.as_str() == "declares_verification"
                && matches!(
                    n.kind,
                    groundgraph_core::NodeKind::TestCase | groundgraph_core::NodeKind::TestGroup
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
    let down_budget = MAX_PACK_NEIGHBORS.saturating_sub(up_taken);
    for edge in from_edges.iter().take(down_budget) {
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

    let mut files_to_read: Vec<String> = files_to_read.into_iter().collect();
    files_to_read.sort();

    Ok(json!({
        "mode": "symbol",
        "truncated": truncated,
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

fn read_snippet(repo_root: &Path, node: &groundgraph_core::Node) -> Result<Option<String>> {
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
    // Confine to the repo: a malicious / corrupt graph whose `node.path` is
    // absolute or contains `..` must not let us read (and return to the client)
    // files outside `repo_root` (#243). Snippets are best-effort, so a refused
    // path simply yields no snippet.
    let rel = Path::new(rel_path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Ok(None);
    }
    let abs = repo_root.join(rel);
    if !abs.exists() {
        return Ok(None);
    }
    // Bound memory: a symbol whose `path` points at a generated / minified /
    // vendored multi-MB file must not be slurped whole just to extract a few
    // lines (#88). A corrupt `end_line` would otherwise also force an O(file)
    // `lines()` walk over that whole blob.
    if let Ok(meta) = std::fs::metadata(&abs) {
        if meta.len() > SNIPPET_MAX_FILE_BYTES {
            return Ok(None);
        }
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

#[cfg(test)]
mod tests {
    use super::{build_symbol_pack, read_snippet, MAX_PACK_NEIGHBORS, SNIPPET_MAX_FILE_BYTES};
    use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};

    /// #88: a snippet must not slurp a multi-MB file into memory. An oversized
    /// file yields no snippet; a small one with the same range still does.
    #[test]
    fn read_snippet_skips_oversized_files_but_reads_small_ones() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("gen")).unwrap();
        let big_rel = "gen/huge.dart";
        std::fs::write(
            dir.path().join(big_rel),
            vec![b'x'; usize::try_from(SNIPPET_MAX_FILE_BYTES + 1).unwrap()],
        )
        .unwrap();
        let mut node = Node::new(
            ArtifactId::new("dart::gen/huge.dart#x"),
            NodeKind::DartFunction,
        );
        node.path = Some(big_rel.to_string());
        node.start_line = Some(1);
        node.end_line = Some(2);
        assert_eq!(
            read_snippet(dir.path(), &node).unwrap(),
            None,
            "oversized file must be skipped, not slurped"
        );

        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let small_rel = "src/a.dart";
        std::fs::write(dir.path().join(small_rel), "line1\nline2\nline3\n").unwrap();
        node.path = Some(small_rel.to_string());
        assert!(
            read_snippet(dir.path(), &node).unwrap().is_some(),
            "a small file must still produce a snippet"
        );
    }

    /// #243: a malicious / corrupt graph whose `node.path` escapes the repo
    /// (via `..` or an absolute path) must NOT let `context_pack` read files
    /// outside `repo_root` and leak them to the MCP client.
    #[test]
    fn read_snippet_refuses_path_traversal() {
        let outer = tempfile::tempdir().unwrap();
        std::fs::write(outer.path().join("secret.txt"), "TOP SECRET\nx\n").unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let mut node = Node::new(
            ArtifactId::new("x::../secret.txt#s"),
            NodeKind::DartFunction,
        );
        node.start_line = Some(1);
        node.end_line = Some(1);

        // `..` escape → no snippet.
        node.path = Some("../secret.txt".to_string());
        assert_eq!(
            read_snippet(&repo, &node).unwrap(),
            None,
            "`..` traversal must not read files outside repo_root"
        );

        // Absolute path → no snippet.
        node.path = Some(
            outer
                .path()
                .join("secret.txt")
                .to_string_lossy()
                .into_owned(),
        );
        assert_eq!(
            read_snippet(&repo, &node).unwrap(),
            None,
            "absolute node.path must not read files outside repo_root"
        );
    }

    /// #88: a hub symbol's pack caps the neighbour list and flags truncation
    /// instead of buffering a 5-digit fan-out into one response.
    #[test]
    fn symbol_pack_caps_neighbours_and_flags_truncation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".groundgraph")).unwrap();
        std::fs::write(dir.path().join(".groundgraph.yaml"), "{}\n").unwrap();
        let mut store =
            groundgraph_store::Store::open(dir.path().join(".groundgraph/graph.db")).unwrap();
        store.migrate().unwrap();

        let hub = ArtifactId::new("java::Hub.java#Hub.m".to_string());
        store
            .upsert_node(&Node::new(hub.clone(), NodeKind::JavaMethod))
            .unwrap();
        for i in 0..(MAX_PACK_NEIGHBORS + 150) {
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
            e.id = ArtifactId::new(format!("edge::{i}"));
            store.upsert_edge(&e).unwrap();
        }

        let out = build_symbol_pack(dir.path(), &hub.to_string(), false).unwrap();
        assert_eq!(out["truncated"], serde_json::json!(true));
        let neighbours = out["pack"]["neighbours"].as_array().unwrap();
        assert!(
            neighbours.len() <= MAX_PACK_NEIGHBORS,
            "neighbour budget must hold: {}",
            neighbours.len()
        );
    }
}
