//! MCP tool catalogue and dispatcher.
//!
//! Six tools — see module-level descriptors below. Each handler is
//! intentionally small: parse JSON args → call into `specslice-engine`
//! → return the engine's response as a JSON value. The server module
//! wraps that value as an MCP `tools/call` content block.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use specslice_core::NodeKind;
use specslice_engine::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use specslice_engine::default_search_kinds;
use specslice_engine::search::EXPANSION_EDGE_KINDS;
use specslice_store::Store;

use crate::protocol::ToolDescriptor;
use crate::server::Server;

mod context_pack;
mod dead_code;
mod explain_symbol;
mod get_subgraph;
mod impact_tool;
mod search_graph;

/// Static catalogue of every tool the server exposes. Order is the
/// order returned to `tools/list`, which becomes the order shown to
/// agents — `search_graph` first because it is the entry point for
/// every other tool.
pub fn descriptors() -> Vec<ToolDescriptor> {
    vec![
        search_graph::descriptor(),
        get_subgraph::descriptor(),
        explain_symbol::descriptor(),
        impact_tool::descriptor(),
        dead_code::descriptor(),
        context_pack::descriptor(),
    ]
}

pub fn is_known(name: &str) -> bool {
    matches!(
        name,
        "search_graph"
            | "get_subgraph"
            | "explain_symbol"
            | "impact"
            | "dead_code"
            | "context_pack"
    )
}

pub fn call(server: &Server, name: &str, args: &Value) -> Result<Value> {
    match name {
        "search_graph" => search_graph::call(server, args),
        "get_subgraph" => get_subgraph::call(server, args),
        "explain_symbol" => explain_symbol::call(server, args),
        "impact" => impact_tool::call(server, args),
        "dead_code" => dead_code::call(server, args),
        "context_pack" => context_pack::call(server, args),
        other => Err(anyhow!("unknown tool `{other}`")),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers used by multiple tool handlers
// ---------------------------------------------------------------------------

pub(crate) fn resolve_repo_root(server: &Server, args: &Value) -> PathBuf {
    if let Some(s) = args.get("repo_root").and_then(|v| v.as_str()) {
        let p = PathBuf::from(s);
        if p.is_absolute() {
            return p;
        }
        return server.default_repo_root.join(p);
    }
    server.default_repo_root.clone()
}

pub(crate) fn open_store(repo_root: &Path) -> Result<Store> {
    let config = load_engine_config(repo_root)?;
    let db_path = resolve_db_path(repo_root, &config);
    Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))
}

pub(crate) fn load_engine_config(repo_root: &Path) -> Result<EngineConfig> {
    let path = repo_root.join(DEFAULT_CONFIG_FILE_NAME);
    if !path.exists() {
        bail!(
            "no SpecSlice workspace at {}: run `specslice init` first.",
            repo_root.display()
        );
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    serde_yaml::from_str::<EngineConfig>(&raw)
        .with_context(|| format!("parsing config {}", path.display()))
}

pub(crate) fn resolve_db_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = config.storage.path.clone();
    if raw.is_empty() {
        return repo_root.join(".specslice/graph.db");
    }
    let candidate = PathBuf::from(&raw);
    if candidate.is_absolute() {
        candidate
    } else {
        repo_root.join(candidate)
    }
}

pub(crate) fn parse_node_kinds(values: &Value) -> Result<Vec<NodeKind>> {
    let Some(arr) = values.as_array() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let s = v
            .as_str()
            .ok_or_else(|| anyhow!("`kinds` entries must be strings"))?;
        out.push(parse_node_kind(s)?);
    }
    Ok(out)
}

pub(crate) fn parse_node_kind(raw: &str) -> Result<NodeKind> {
    let lower = raw.to_ascii_lowercase();
    Ok(match lower.as_str() {
        "file" => NodeKind::File,
        "doc" | "doc_section" => NodeKind::DocSection,
        "class" | "dart_class" => NodeKind::DartClass,
        "method" | "dart_method" => NodeKind::DartMethod,
        "function" | "dart_function" => NodeKind::DartFunction,
        "constructor" | "dart_constructor" => NodeKind::DartConstructor,
        "test" | "test_case" => NodeKind::TestCase,
        "group" | "test_group" => NodeKind::TestGroup,
        "provider" | "dart_provider" => NodeKind::DartProvider,
        "route" => NodeKind::Route,
        "storage" => NodeKind::Storage,
        "candidate" | "business_candidate" => NodeKind::BusinessCandidate,
        "requirement" => NodeKind::Requirement,
        // P11 — Swift / Go kinds (full names + short aliases). Short
        // aliases let agents say `swift_method` *or* `swift.method`;
        // the bare `class` / `method` aliases above remain bound to
        // Dart for backward compatibility with existing skills.
        "swift_class" => NodeKind::SwiftClass,
        "swift_struct" => NodeKind::SwiftStruct,
        "swift_enum" => NodeKind::SwiftEnum,
        "swift_protocol" => NodeKind::SwiftProtocol,
        "swift_method" => NodeKind::SwiftMethod,
        "swift_function" => NodeKind::SwiftFunction,
        "swift_initializer" | "swift_init" => NodeKind::SwiftInitializer,
        "go_struct" | "gostruct" => NodeKind::GoStruct,
        "go_interface" | "gointerface" => NodeKind::GoInterface,
        "go_method" => NodeKind::GoMethod,
        "go_function" | "gofunc" => NodeKind::GoFunction,
        // P16 — Python kinds. Modules surface here too so agents can
        // search `kinds: ["python_module"]` for higher-level filtering.
        "python_module" | "py_module" => NodeKind::PythonModule,
        "python_class" | "py_class" => NodeKind::PythonClass,
        "python_function" | "py_function" | "pyfunc" => NodeKind::PythonFunction,
        "python_method" | "py_method" => NodeKind::PythonMethod,
        other => bail!(
            "unknown node kind `{other}`. valid: {}",
            default_search_kinds()
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

pub(crate) fn parse_edge_kinds(values: &Value) -> Result<Vec<String>> {
    let Some(arr) = values.as_array() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let s = v
            .as_str()
            .ok_or_else(|| anyhow!("`edge_kinds` entries must be strings"))?;
        if !EXPANSION_EDGE_KINDS.contains(&s) {
            bail!(
                "unknown edge kind `{s}`. valid: {}",
                EXPANSION_EDGE_KINDS.join(", ")
            );
        }
        out.push(s.to_string());
    }
    Ok(out)
}

/// Convenience for tools that just want the basic JSON Schema object
/// shape `{ "type": "object", "properties": {...}, "required": [...] }`.
pub(crate) fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}
