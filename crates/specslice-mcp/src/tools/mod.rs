//! MCP tool catalogue and dispatcher.
//!
//! Seven tools — see module-level descriptors below. Each handler is
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

mod check_drift;
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
        check_drift::descriptor(),
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
            | "check_drift"
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
        "check_drift" => check_drift::call(server, args),
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
    // Canonical snake_case names resolve via the single source of truth in
    // `specslice-core` (`dart_class`, `typescript_interface`, `cpp_method`,
    // …) — no need to re-list all ~58 here. Below we only keep the *extra*
    // short / legacy aliases the canonical scheme does not cover.
    if let Some(kind) = NodeKind::from_str(&lower) {
        return Ok(kind);
    }
    Ok(match lower.as_str() {
        // Bare aliases bound to Dart for backward compatibility with
        // existing skills (`class`/`method` mean Dart unless prefixed).
        "doc" => NodeKind::DocSection,
        "class" => NodeKind::DartClass,
        "method" => NodeKind::DartMethod,
        "function" => NodeKind::DartFunction,
        "constructor" => NodeKind::DartConstructor,
        "test" => NodeKind::TestCase,
        "group" => NodeKind::TestGroup,
        "provider" => NodeKind::DartProvider,
        "candidate" => NodeKind::BusinessCandidate,
        // Swift / Go short aliases.
        "swift_init" => NodeKind::SwiftInitializer,
        "gostruct" => NodeKind::GoStruct,
        "gointerface" => NodeKind::GoInterface,
        "gofunc" => NodeKind::GoFunction,
        // Python `py_` aliases.
        "py_module" => NodeKind::PythonModule,
        "py_class" => NodeKind::PythonClass,
        "py_function" | "pyfunc" => NodeKind::PythonFunction,
        "py_method" => NodeKind::PythonMethod,
        // TypeScript `ts_` aliases.
        "ts_module" => NodeKind::TypescriptModule,
        "ts_class" => NodeKind::TypescriptClass,
        "ts_interface" => NodeKind::TypescriptInterface,
        "ts_enum" => NodeKind::TypescriptEnum,
        "ts_function" | "tsfunc" => NodeKind::TypescriptFunction,
        "ts_method" => NodeKind::TypescriptMethod,
        // Java / Rust / C / C++ short + `cxx_` aliases.
        "java_init" => NodeKind::JavaConstructor,
        "rs_module" | "rs_mod" => NodeKind::RustModule,
        "rs_struct" => NodeKind::RustStruct,
        "rs_enum" => NodeKind::RustEnum,
        "rs_trait" => NodeKind::RustTrait,
        "rs_function" | "rs_fn" => NodeKind::RustFunction,
        "rs_method" => NodeKind::RustMethod,
        "cfn" => NodeKind::CFunction,
        "cxx_namespace" | "cpp_ns" => NodeKind::CppNamespace,
        "cxx_class" => NodeKind::CppClass,
        "cxx_struct" => NodeKind::CppStruct,
        "cxx_enum" => NodeKind::CppEnum,
        "cxx_function" | "cpp_fn" => NodeKind::CppFunction,
        "cxx_method" => NodeKind::CppMethod,
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
