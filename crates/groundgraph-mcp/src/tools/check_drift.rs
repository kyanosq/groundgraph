//! `check_drift` — consistency checks + content-level doc→code drift.

use anyhow::{Context, Result};
use groundgraph_engine::{run_checks, CheckOptions};
use serde_json::{json, Value};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, resolve_repo_root};

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "check_drift",
        description: "Run GroundGraph consistency checks: broken declared links, \
            requirements missing linked tests, orphan requirements, \
            `doc_stale_code_ref` (a doc body references a path/symbol that no \
            longer exists — stale doc or unimplemented code) and \
            `requirement_implementation_hint` (plausible implementations for \
            orphan requirements found via the graph + fulltext layer). \
            Returns the findings list with severity / code / message / path.",
        input_schema: object_schema(
            json!({
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
    let report = run_checks(CheckOptions {
        repo_root,
        impact: None,
    })
    .context("running checks")?;
    Ok(serde_json::to_value(&report)?)
}
