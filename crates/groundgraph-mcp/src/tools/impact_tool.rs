//! `impact` — PR impact analysis from a git diff.

use anyhow::{Context, Result};
use groundgraph_engine::{run_impact, ImpactOptions};
use serde_json::{json, Value};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, resolve_repo_root};

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "impact",
        description: "Compute which requirements, docs, tests, implementations \
            and confirmed business candidates are affected by the diff \
            between two git refs. Equivalent to `groundgraph impact` — \
            ideal for PR/CI use.",
        input_schema: object_schema(
            json!({
                "base": {
                    "type": "string",
                    "default": "origin/main",
                    "description": "Base git ref to diff against."
                },
                "head": {
                    "type": "string",
                    "default": "HEAD",
                    "description": "Head git ref."
                },
                "worktree": {
                    "type": "boolean",
                    "default": false,
                    "description": "Diff `base` against the current working tree instead of a committed head. Use this when an agent is reviewing uncommitted changes."
                },
                "reindex": {
                    "type": "boolean",
                    "default": true,
                    "description": "Re-index changed files before computing impact."
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
    let base = args
        .get("base")
        .and_then(|v| v.as_str())
        .unwrap_or("origin/main")
        .to_string();
    let head = args
        .get("head")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD")
        .to_string();
    let worktree = args
        .get("worktree")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reindex = args
        .get("reindex")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let repo_root = resolve_repo_root(server, args)?;
    let options = ImpactOptions {
        repo_root,
        base_ref: base,
        head_ref: if worktree { String::new() } else { head },
        reindex,
    };
    let report = run_impact(options).context("computing impact")?;
    Ok(serde_json::to_value(&report)?)
}
