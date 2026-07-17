//! MCP tool catalogue and dispatcher.
//!
//! Seven tools — see module-level descriptors below. Each handler is
//! intentionally small: parse JSON args → call into `groundgraph-engine`
//! → return the engine's response as a JSON value. The server module
//! wraps that value as an MCP `tools/call` content block.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use groundgraph_core::NodeKind;
use groundgraph_engine::config::{load_config, resolve_storage_path, EngineConfig};
use groundgraph_engine::default_search_kinds;
use groundgraph_engine::search::EXPANSION_EDGE_KINDS;
use groundgraph_store::Store;
use serde_json::{json, Value};

use crate::protocol::ToolDescriptor;
use crate::server::Server;

mod check_drift;
mod context_pack;
mod dead_code;
mod explain_symbol;
mod get_subgraph;
mod impact_tool;
mod search_graph;
mod validate;

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

/// Validate `args` against the named tool's advertised `inputSchema` (#89).
/// Returns the first contract violation as a message the dispatcher turns into
/// a `-32602 Invalid params` error. An unknown tool is a no-op here — the
/// dispatcher already rejects it before reaching this point.
pub(crate) fn validate_call_arguments(name: &str, args: &Value) -> Result<(), String> {
    let Some(descriptor) = descriptors().into_iter().find(|d| d.name == name) else {
        return Ok(());
    };
    validate::validate_arguments(&descriptor.input_schema, args)
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

pub(crate) fn resolve_repo_root(server: &Server, args: &Value) -> Result<PathBuf> {
    // Missing / null / non-string `repo_root` keeps the lenient default — no
    // change for the common case. Only an *explicit string* is validated.
    let Some(s) = args.get("repo_root").and_then(|v| v.as_str()) else {
        return Ok(server.default_repo_root.clone());
    };
    // `""` previously folded to `default_repo_root.join("")` == the default by
    // accident; make the intent explicit instead of silently guessing (#108).
    if s.is_empty() {
        bail!("`repo_root` must not be an empty string; omit it to use the server default");
    }
    let p = Path::new(s);
    if p.is_absolute() {
        // An absolute path is the client explicitly naming the repo to analyse.
        return Ok(p.to_path_buf());
    }
    // Confine a *relative* root to the server default: a `..` escape would let
    // a request walk out of the intended workspace into e.g. the user's home,
    // where a stray `.groundgraph.yaml` could be mistaken for the workspace
    // (#108). Callers that genuinely want another repo pass an absolute path.
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!(
            "`repo_root` ({s}) must not contain `..`; pass an absolute path to \
             analyse a repo outside the default root"
        );
    }
    Ok(server.default_repo_root.join(p))
}

pub(crate) fn open_store(repo_root: &Path) -> Result<Store> {
    let config = load_engine_config(repo_root)?;
    // Storage-path resolution — including the #242 `..` confinement — lives
    // in the single shared engine resolver (#145/#263); there is no separate
    // MCP-side copy to drift.
    let db_path = resolve_storage_path(repo_root, &config)?;
    Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))
}

pub(crate) fn load_engine_config(repo_root: &Path) -> Result<EngineConfig> {
    load_config(repo_root).with_context(|| {
        format!(
            "loading GroundGraph config for MCP workspace {}",
            repo_root.display()
        )
    })
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
    // `groundgraph-core` (`dart_class`, `typescript_interface`, `cpp_method`,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::Server;

    fn server() -> Server {
        Server::new(PathBuf::from("/work/repo"))
    }

    #[test]
    fn resolve_repo_root_defaults_and_joins_relative() {
        let s = server();
        // Missing → server default.
        assert_eq!(
            resolve_repo_root(&s, &json!({})).unwrap(),
            PathBuf::from("/work/repo")
        );
        // A plain relative path joins under the default.
        assert_eq!(
            resolve_repo_root(&s, &json!({ "repo_root": "sub/dir" })).unwrap(),
            PathBuf::from("/work/repo/sub/dir")
        );
        // An absolute path is honoured verbatim (client names its own repo).
        assert_eq!(
            resolve_repo_root(&s, &json!({ "repo_root": "/other/repo" })).unwrap(),
            PathBuf::from("/other/repo")
        );
        // Non-string keeps the lenient default rather than erroring.
        assert_eq!(
            resolve_repo_root(&s, &json!({ "repo_root": 123 })).unwrap(),
            PathBuf::from("/work/repo")
        );
    }

    #[test]
    fn resolve_repo_root_rejects_empty_and_parent_dir_escape() {
        let s = server();
        // Empty string is no longer silently folded to the default (#108).
        assert!(resolve_repo_root(&s, &json!({ "repo_root": "" })).is_err());
        // `..` escapes out of the confined workspace → refused (#108).
        assert!(resolve_repo_root(&s, &json!({ "repo_root": "../../etc" })).is_err());
        assert!(resolve_repo_root(&s, &json!({ "repo_root": "sub/../../escape" })).is_err());
    }

    #[test]
    fn open_store_rejects_a_storage_path_escaping_the_repo() {
        // #242: a poisoned `.groundgraph.yaml` must not relocate the SQLite db
        // outside the analysed repo via a `..`-escaping relative `storage.path`.
        // The guard now lives in the shared engine resolver (#145/#263); this
        // pins it through the MCP entry point.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::write(
            repo.join(".groundgraph.yaml"),
            "storage:\n  path: ../../evil.db\n",
        )
        .unwrap();
        let err = open_store(repo)
            .err()
            .expect("a `..` storage.path must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("storage.path"), "{msg}");
        assert!(msg.contains(".."), "{msg}");
    }

    #[test]
    fn open_store_honours_an_absolute_storage_path() {
        // Absolute remains allowed — an explicit operator override, consistent
        // with the engine resolver and #108 repo_root.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let db = tmp.path().join("elsewhere/graph.db");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(
            repo.join(".groundgraph.yaml"),
            format!("storage:\n  path: {}\n", db.display()),
        )
        .unwrap();
        open_store(&repo).unwrap();
        assert!(db.exists());
    }
}
