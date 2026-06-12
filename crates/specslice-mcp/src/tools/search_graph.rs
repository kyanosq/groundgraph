//! `search_graph` — code-graph search (grep replacement).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use specslice_engine::run_search_with_store;
use specslice_engine::search::{SearchOptions, SearchQuery};
use specslice_engine::{SEARCH_DEFAULT_DEPTH, SEARCH_DEFAULT_LIMIT};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, open_store, parse_node_kinds, resolve_repo_root};

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "search_graph",
        description: "Search the SpecSlice code graph for matches plus a 1-hop subgraph. \
            Provide exactly one of `query` (free text), `code` (snippet) or \
            (`file` + `line`). Returns matches with `match_reasons` and an \
            optional expanded subgraph for Agent reasoning. Equivalent to \
            `specslice search` but emits structured JSON only.",
        input_schema: object_schema(
            json!({
                "query": {
                    "type": "string",
                    "description": "Free-form keywords. Whitespace and punctuation split into tokens; AI expansion happens on the caller, not here."
                },
                "code": {
                    "type": "string",
                    "description": "Code snippet. We deterministically extract identifiers, string literals and path-like tokens."
                },
                "file": {
                    "type": "string",
                    "description": "Repo-relative file path for position search. Requires `line`."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line number inside `file`."
                },
                "depth": {
                    "type": "integer",
                    "minimum": 0,
                    "default": SEARCH_DEFAULT_DEPTH,
                    "description": "Hops to expand from each match. 0 = direct matches only."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "default": SEARCH_DEFAULT_LIMIT,
                    "description": "Cap on the number of direct matches (neighbours not counted)."
                },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Restrict direct matches to these node kinds (e.g. `dart_method`, `test_case`, `business_candidate`)."
                },
                "include_noise": {
                    "type": "boolean",
                    "default": false,
                    "description": "Keep framework-noise calls (toString / build / dispose / ...)."
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
    let query = build_query(args)?;
    let depth = args
        .get("depth")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(SEARCH_DEFAULT_DEPTH);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(SEARCH_DEFAULT_LIMIT)
        .max(1);
    let kinds = parse_node_kinds(args.get("kinds").unwrap_or(&Value::Null))?;
    let include_noise = args
        .get("include_noise")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let repo_root = resolve_repo_root(server, args);
    let store = open_store(&repo_root)?;
    let options = SearchOptions {
        repo_root: repo_root.clone(),
        query,
        depth,
        kinds,
        limit,
        include_noise,
    };
    let result = run_search_with_store(&store, options).context("running search")?;
    Ok(serde_json::to_value(&result)?)
}

fn build_query(args: &Value) -> Result<SearchQuery> {
    let query = args.get("query").and_then(|v| v.as_str());
    let code = args.get("code").and_then(|v| v.as_str());
    let file = args.get("file").and_then(|v| v.as_str());
    let line = args.get("line").and_then(|v| v.as_u64());

    let provided = [query.is_some(), code.is_some(), file.is_some()]
        .iter()
        .filter(|x| **x)
        .count();
    if provided == 0 {
        bail!("supply one of `query`, `code` or (`file` + `line`)");
    }
    if provided > 1 {
        bail!("`query`, `code` and `file` are mutually exclusive");
    }

    if let Some(q) = query {
        return Ok(SearchQuery::Keywords(q.to_string()));
    }
    if let Some(c) = code {
        return Ok(SearchQuery::Code(c.to_string()));
    }
    let path = file.expect("file present").to_string();
    let line = line.ok_or_else(|| anyhow::anyhow!("`file` requires `line`"))?;
    let line_u32 = u32::try_from(line).map_err(|_| anyhow::anyhow!("`line` must fit in u32"))?;
    if line_u32 == 0 {
        bail!("`line` must be >= 1");
    }
    Ok(SearchQuery::Position {
        path,
        line: line_u32,
    })
}

#[cfg(test)]
mod tests {
    use specslice_engine::search::SearchSubgraph;
    use specslice_engine::SearchResult;

    /// v0.3.0-A Phase 4 — the MCP tool delegates everything to
    /// `serde_json::to_value(&result)` so the wire shape is whatever
    /// `SearchResult` serialises to. Lock down that the new
    /// `warnings` field round-trips into the JSON-RPC payload when
    /// non-empty.
    #[test]
    fn search_graph_response_carries_warnings_when_present() {
        let result = SearchResult {
            query: "login".to_string(),
            tokens: vec!["login".to_string()],
            matches: Vec::new(),
            subgraph: SearchSubgraph {
                nodes: Vec::new(),
                edges: Vec::new(),
                truncated: false,
            },
            graph_commands: Vec::new(),
            warnings: vec!["warn: 节点 abc 的出边质量查询失败：disk i/o error".to_string()],
        };
        let value = serde_json::to_value(&result).expect("serialise SearchResult");
        let warnings = value
            .get("warnings")
            .and_then(|v| v.as_array())
            .expect("warnings field must be present and an array when populated");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0]
            .as_str()
            .unwrap_or_default()
            .contains("出边质量查询失败"));
    }

    /// Empty `warnings` must NOT show up in the JSON payload so older
    /// MCP clients (and JSON consumers of `specslice search --json`)
    /// remain fully backward compatible.
    #[test]
    fn search_graph_response_omits_warnings_when_empty() {
        let result = SearchResult {
            query: "login".to_string(),
            tokens: vec!["login".to_string()],
            matches: Vec::new(),
            subgraph: SearchSubgraph {
                nodes: Vec::new(),
                edges: Vec::new(),
                truncated: false,
            },
            graph_commands: Vec::new(),
            warnings: Vec::new(),
        };
        let value = serde_json::to_value(&result).expect("serialise SearchResult");
        assert!(
            value.get("warnings").is_none(),
            "empty warnings must be skipped in JSON, got: {value}",
        );
    }
}
