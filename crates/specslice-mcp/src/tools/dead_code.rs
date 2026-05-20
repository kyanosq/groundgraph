//! `dead_code` — dead code report with confidence buckets.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use specslice_engine::{analyze_dead_code, DeadCodeConfidence, DeadCodeOptions};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, resolve_repo_root};

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "dead_code",
        description: "Report symbols that the SpecSlice graph cannot reach from \
            any configured entry point (main / routes / providers / tests \
            / public_api_roots / Flutter lifecycle). Output bucketed by \
            confidence — High / Medium / Low — with human-readable \
            `reasons`. NEVER recommend deletion; this is a report.",
        input_schema: object_schema(
            json!({
                "min_confidence": {
                    "type": "string",
                    "enum": ["low", "medium", "high"],
                    "default": "medium",
                    "description": "Filter out anything below this bucket."
                },
                "include_tests": {
                    "type": "boolean",
                    "default": false,
                    "description": "Also consider test cases / groups as dead-code candidates."
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
    let min_confidence = match args
        .get("min_confidence")
        .and_then(|v| v.as_str())
        .unwrap_or("medium")
        .to_ascii_lowercase()
        .as_str()
    {
        "high" => DeadCodeConfidence::High,
        "medium" => DeadCodeConfidence::Medium,
        "low" => DeadCodeConfidence::Low,
        other => bail!("`min_confidence` must be high|medium|low; got `{other}`"),
    };
    let include_tests = args
        .get("include_tests")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let repo_root = resolve_repo_root(server, args);
    let opts = DeadCodeOptions {
        repo_root,
        min_confidence,
        include_tests,
    };
    let report = analyze_dead_code(opts).context("analysing dead code")?;
    Ok(serde_json::to_value(&report)?)
}
