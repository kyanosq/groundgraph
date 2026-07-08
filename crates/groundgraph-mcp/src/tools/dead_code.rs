//! `dead_code` — dead code report with confidence buckets.

use anyhow::{bail, Context, Result};
use groundgraph_engine::{analyze_dead_code, DeadCodeConfidence, DeadCodeOptions};
use serde_json::{json, Value};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

use super::{object_schema, resolve_repo_root};

pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: "dead_code",
        description: "Report symbols that the GroundGraph graph cannot reach from \
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
    let repo_root = resolve_repo_root(server, args)?;
    let opts = DeadCodeOptions {
        repo_root,
        min_confidence,
        include_tests,
    };
    let report = analyze_dead_code(opts).context("analysing dead code")?;
    Ok(serde_json::to_value(&report)?)
}

#[cfg(test)]
mod tests {
    use groundgraph_engine::{DeadCodeReport, DeadCodeStats, DEAD_CODE_SCHEMA_VERSION};

    /// v0.3.0-A Phase 4 — `serde_json::to_value(&report)` is the only
    /// transformation the MCP tool performs, so verifying it carries
    /// `warnings` is enough to lock down the wire shape.
    #[test]
    fn dead_code_response_carries_warnings_when_present() {
        let report = DeadCodeReport {
            schema_version: DEAD_CODE_SCHEMA_VERSION,
            min_confidence: "medium".to_string(),
            stats: DeadCodeStats::default(),
            candidates: Vec::new(),
            warnings: vec!["warn: 节点 X 的入边质量查询失败：sqlite locked".to_string()],
        };
        let value = serde_json::to_value(&report).expect("serialise DeadCodeReport");
        let warnings = value
            .get("warnings")
            .and_then(|v| v.as_array())
            .expect("warnings field must be present and an array when populated");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0]
            .as_str()
            .unwrap_or_default()
            .contains("入边质量查询失败"));
    }

    /// Empty `warnings` must NOT show up in the JSON payload so old
    /// MCP clients keep working unchanged.
    #[test]
    fn dead_code_response_omits_warnings_when_empty() {
        let report = DeadCodeReport {
            schema_version: DEAD_CODE_SCHEMA_VERSION,
            min_confidence: "medium".to_string(),
            stats: DeadCodeStats::default(),
            candidates: Vec::new(),
            warnings: Vec::new(),
        };
        let value = serde_json::to_value(&report).expect("serialise DeadCodeReport");
        assert!(
            value.get("warnings").is_none(),
            "empty warnings must be skipped in JSON, got: {value}",
        );
    }
}
