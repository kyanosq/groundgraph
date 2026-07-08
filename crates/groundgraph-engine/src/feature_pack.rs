//! P24 — one-click feature slice export (gap #7).
//!
//! Hands an agent everything it needs to re-implement one feature in a
//! single JSON document, so it never has to re-read the whole repo:
//!
//! - the **symbols** in scope, each with its behavioural facts + purity,
//! - the **edges** among them (and out to external callees),
//! - the **constants** used inside them,
//! - the **data contract** (tables / JSON keys) of the in-scope files,
//! - the **test suggestions** for the in-scope symbols.
//!
//! Scope is selected either by a **path prefix** (`lib/alarm/`) or by a
//! **requirement id** (reusing [`slice`](crate::slice) to find the files a
//! requirement touches). Everything is filtered to that scope so the pack is
//! self-contained and deterministic.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use groundgraph_core::language_traits::is_code_symbol;
use groundgraph_core::EdgeKind;
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

use crate::config::EngineConfig;
use crate::constants::{analyze_constants_with_store, ConstantEntry, ConstantsOptions};
use crate::data_contract::{
    analyze_data_contract_with_store, DataContractOptions, JsonKey, TableSchema,
};
use crate::slice::slice_from_store;
use crate::source_text::read_node_source;
use crate::symbol_facts::{build_fact, SymbolFact};
use crate::test_suggestions::{
    analyze_test_suggestions_with_store, SymbolSuggestions, TestSuggestionsOptions,
};

pub const FEATURE_PACK_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Data contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackEdge {
    pub kind: String,
    pub from: String,
    pub to: String,
    /// `true` when both endpoints are in scope; `false` for an external
    /// callee (a dependency the rewrite must also satisfy or stub).
    pub in_scope: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FeaturePackStats {
    pub files: usize,
    pub symbols: usize,
    pub edges_internal: usize,
    pub edges_external: usize,
    pub constants: usize,
    pub tables: usize,
    pub json_keys: usize,
    pub test_suggestions: usize,
    pub pure_symbols: usize,
    pub impure_symbols: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeaturePack {
    pub schema_version: u32,
    /// Echo of the selector, e.g. `path:lib/alarm` or `requirement:REQ-X`.
    pub focus: String,
    pub stats: FeaturePackStats,
    pub files: Vec<String>,
    pub symbols: Vec<SymbolFact>,
    pub edges: Vec<PackEdge>,
    pub constants: Vec<ConstantEntry>,
    pub tables: Vec<TableSchema>,
    pub json_keys: Vec<JsonKey>,
    pub test_suggestions: Vec<SymbolSuggestions>,
}

/// How to choose the feature's scope.
#[derive(Debug, Clone)]
pub enum FeaturePackSelector {
    /// All code symbols whose file path starts with this prefix.
    Path(String),
    /// Files a requirement touches (via the slice engine).
    Requirement(String),
}

#[derive(Debug, Clone)]
pub struct FeaturePackOptions {
    pub repo_root: PathBuf,
    pub selector: FeaturePackSelector,
    pub max_evidence_per_symbol: usize,
}

// A site is "in scope" if its file path is in the resolved scope set. We use
// a generous cap so occurrence counts stay accurate after scope filtering.
const WIDE_CAP: usize = 100_000;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn build_feature_pack(options: FeaturePackOptions) -> Result<FeaturePack> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    build_feature_pack_with_store(&store, &options)
}

pub fn build_feature_pack_with_store(
    store: &Store,
    options: &FeaturePackOptions,
) -> Result<FeaturePack> {
    let (focus, scope_files) = resolve_scope(store, &options.selector)?;

    // In-scope symbols + their facts.
    let mut symbols: Vec<SymbolFact> = Vec::new();
    let mut scope_ids: BTreeSet<String> = BTreeSet::new();
    let mut pure = 0usize;
    let mut impure = 0usize;
    for node in store.list_all_nodes()? {
        if !is_code_symbol(node.kind) {
            continue;
        }
        let Some(path) = &node.path else { continue };
        if !scope_files.contains(path) {
            continue;
        }
        scope_ids.insert(node.id.to_string());
        if let Some(src) = read_node_source(&options.repo_root, &node) {
            let fact = build_fact(&node, &src, options.max_evidence_per_symbol);
            match fact.purity {
                crate::symbol_facts::Purity::Pure => pure += 1,
                crate::symbol_facts::Purity::Impure => impure += 1,
                crate::symbol_facts::Purity::Unknown => {}
            }
            symbols.push(fact);
        }
    }
    symbols.sort_by(|a, b| a.id.cmp(&b.id));

    // Edges touching the scope.
    let mut edges: Vec<PackEdge> = Vec::new();
    let mut edges_internal = 0usize;
    let mut edges_external = 0usize;
    for edge in store.list_all_edges()? {
        let from = edge.from_id.to_string();
        let to = edge.to_id.to_string();
        let from_in = scope_ids.contains(&from);
        let to_in = scope_ids.contains(&to);
        if !from_in && !to_in {
            continue;
        }
        // Only surface meaningful code edges, not doc/manifest links.
        if !matches!(
            edge.kind,
            EdgeKind::Calls
                | EdgeKind::References
                | EdgeKind::Contains
                | EdgeKind::ReadsProvider
                | EdgeKind::PersistsTo
                | EdgeKind::NavigatesTo
        ) {
            continue;
        }
        let in_scope = from_in && to_in;
        if in_scope {
            edges_internal += 1;
        } else {
            edges_external += 1;
        }
        edges.push(PackEdge {
            kind: edge.kind.as_str().to_string(),
            from,
            to,
            in_scope,
        });
    }
    edges.sort_by(|a, b| {
        a.from
            .cmp(&b.from)
            .then(a.to.cmp(&b.to))
            .then(a.kind.cmp(&b.kind))
    });

    // Constants — repo-wide scan filtered to scope.
    let constants = scoped_constants(store, options, &scope_files)?;
    // Data contract — repo-wide scan filtered to scope.
    let (tables, json_keys) = scoped_contract(store, options, &scope_files)?;
    // Test suggestions — filtered to scope ids.
    let test_suggestions = scoped_suggestions(store, options, &scope_ids)?;

    let stats = FeaturePackStats {
        files: scope_files.len(),
        symbols: symbols.len(),
        edges_internal,
        edges_external,
        constants: constants.len(),
        tables: tables.len(),
        json_keys: json_keys.len(),
        test_suggestions: test_suggestions.len(),
        pure_symbols: pure,
        impure_symbols: impure,
    };

    Ok(FeaturePack {
        schema_version: FEATURE_PACK_SCHEMA_VERSION,
        focus,
        stats,
        files: scope_files.into_iter().collect(),
        symbols,
        edges,
        constants,
        tables,
        json_keys,
        test_suggestions,
    })
}

// ---------------------------------------------------------------------------
// Scope resolution
// ---------------------------------------------------------------------------

fn resolve_scope(
    store: &Store,
    selector: &FeaturePackSelector,
) -> Result<(String, BTreeSet<String>)> {
    match selector {
        FeaturePackSelector::Path(prefix) => {
            let norm = prefix.trim_start_matches("./").to_string();
            let mut files = BTreeSet::new();
            for node in store.list_all_nodes()? {
                if !is_code_symbol(node.kind) {
                    continue;
                }
                if let Some(path) = &node.path {
                    if path.starts_with(&norm) {
                        files.insert(path.clone());
                    }
                }
            }
            if files.is_empty() {
                anyhow::bail!("路径前缀 `{prefix}` 下没有可识别的代码符号");
            }
            Ok((format!("path:{prefix}"), files))
        }
        FeaturePackSelector::Requirement(req) => {
            let slice =
                slice_from_store(store, req).with_context(|| format!("解析需求 {req} 的切片"))?;
            let mut files = BTreeSet::new();
            for group in [
                &slice.implementation,
                &slice.linked_tests,
                &slice.code_fanout,
            ] {
                for item in group {
                    if let Some(p) = &item.path {
                        files.insert(p.clone());
                    }
                }
            }
            if files.is_empty() {
                anyhow::bail!("需求 {req} 没有关联到任何实现文件");
            }
            Ok((format!("requirement:{req}"), files))
        }
    }
}

// ---------------------------------------------------------------------------
// Scoped sub-reports
// ---------------------------------------------------------------------------

fn scoped_constants(
    store: &Store,
    options: &FeaturePackOptions,
    scope_files: &BTreeSet<String>,
) -> Result<Vec<ConstantEntry>> {
    let report = analyze_constants_with_store(
        store,
        &ConstantsOptions {
            repo_root: options.repo_root.clone(),
            include_types: false,
            include_trivial: false,
            min_occurrences: 1,
            kind_filter: None,
            max_entries: 0,
            max_sites_per_entry: WIDE_CAP,
        },
    )?;
    let mut out: Vec<ConstantEntry> = report
        .entries
        .into_iter()
        .filter_map(|mut e| {
            e.sites
                .retain(|s| s.path.as_deref().is_some_and(|p| scope_files.contains(p)));
            if e.sites.is_empty() {
                None
            } else {
                e.occurrences = e.sites.len();
                Some(e)
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then(a.kind.cmp(&b.kind))
            .then(a.value.cmp(&b.value))
    });
    Ok(out)
}

fn scoped_contract(
    store: &Store,
    options: &FeaturePackOptions,
    scope_files: &BTreeSet<String>,
) -> Result<(Vec<TableSchema>, Vec<JsonKey>)> {
    let report = analyze_data_contract_with_store(
        store,
        &DataContractOptions {
            repo_root: options.repo_root.clone(),
            tables_only: false,
            keys_only: false,
            max_sites_per_key: WIDE_CAP,
        },
    )?;
    let tables: Vec<TableSchema> = report
        .tables
        .into_iter()
        .filter(|t| scope_files.contains(&t.path))
        .collect();
    let mut json_keys: Vec<JsonKey> = report
        .json_keys
        .into_iter()
        .filter_map(|mut k| {
            k.sites.retain(|s| scope_files.contains(&s.path));
            if k.sites.is_empty() {
                None
            } else {
                k.occurrences = k.sites.len();
                Some(k)
            }
        })
        .collect();
    json_keys.sort_by(|a, b| b.occurrences.cmp(&a.occurrences).then(a.key.cmp(&b.key)));
    Ok((tables, json_keys))
}

fn scoped_suggestions(
    store: &Store,
    options: &FeaturePackOptions,
    scope_ids: &BTreeSet<String>,
) -> Result<Vec<SymbolSuggestions>> {
    let report = analyze_test_suggestions_with_store(
        store,
        &TestSuggestionsOptions {
            repo_root: options.repo_root.clone(),
            include_types: false,
            only_pure: false,
            min_priority: 0,
            max_symbols: 0,
        },
    )?;
    Ok(report
        .items
        .into_iter()
        .filter(|i| scope_ids.contains(&i.id))
        .collect())
}

// ---------------------------------------------------------------------------
// Workspace helpers
// ---------------------------------------------------------------------------

fn load_config(repo_root: &Path) -> Result<EngineConfig> {
    crate::config::load_config(repo_root)
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    crate::config::resolve_storage_path(repo_root, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeSource, Node, NodeKind};

    fn setup() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("lib/alarm")).unwrap();
        std::fs::create_dir_all(dir.path().join("lib/other")).unwrap();
        std::fs::write(
            dir.path().join("lib/alarm/scheduler.dart"),
            "class Scheduler {\n  int pick(int n) {\n    if (n >= 3) { return 3; }\n    return n;\n  }\n  Map toJson() => {'count': pick(0)};\n  factory Scheduler.fromJson(j) => Scheduler(c: j['count'] ?? 0);\n}",
        )
        .unwrap();
        std::fs::write(dir.path().join("lib/other/util.dart"), "int helper() => 7;").unwrap();

        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();

        let mut cls = Node::new(
            ArtifactId::new("dart::lib/alarm/scheduler.dart#Scheduler"),
            NodeKind::DartClass,
        );
        cls.path = Some("lib/alarm/scheduler.dart".to_string());
        cls.name = Some("Scheduler".to_string());
        cls.start_line = Some(1);
        cls.end_line = Some(8);
        store.upsert_node(&cls).unwrap();

        let mut pick = Node::new(
            ArtifactId::new("dart::lib/alarm/scheduler.dart#Scheduler.pick"),
            NodeKind::DartMethod,
        );
        pick.path = Some("lib/alarm/scheduler.dart".to_string());
        pick.name = Some("pick".to_string());
        pick.start_line = Some(2);
        pick.end_line = Some(5);
        store.upsert_node(&pick).unwrap();

        let mut helper = Node::new(
            ArtifactId::new("dart::lib/other/util.dart#helper"),
            NodeKind::DartFunction,
        );
        helper.path = Some("lib/other/util.dart".to_string());
        helper.name = Some("helper".to_string());
        helper.start_line = Some(1);
        helper.end_line = Some(1);
        store.upsert_node(&helper).unwrap();

        // pick -> helper (external callee, out of the alarm scope)
        store
            .upsert_edge(&EdgeAssertion::fact(
                pick.id.clone(),
                helper.id.clone(),
                EdgeKind::Calls,
                EdgeSource::ExternalManifest,
            ))
            .unwrap();
        // Scheduler contains pick (internal)
        store
            .upsert_edge(&EdgeAssertion::fact(
                cls.id.clone(),
                pick.id.clone(),
                EdgeKind::Contains,
                EdgeSource::ExternalManifest,
            ))
            .unwrap();

        (store, dir)
    }

    #[test]
    fn path_pack_is_self_contained_and_scoped() {
        let (store, dir) = setup();
        let pack = build_feature_pack_with_store(
            &store,
            &FeaturePackOptions {
                repo_root: dir.path().to_path_buf(),
                selector: FeaturePackSelector::Path("lib/alarm".to_string()),
                max_evidence_per_symbol: 30,
            },
        )
        .unwrap();

        assert_eq!(pack.focus, "path:lib/alarm");
        assert_eq!(pack.files, vec!["lib/alarm/scheduler.dart".to_string()]);
        // Scheduler + pick are in scope; helper is not.
        let names: Vec<&str> = pack
            .symbols
            .iter()
            .filter_map(|s| s.name.as_deref())
            .collect();
        assert!(names.contains(&"Scheduler"));
        assert!(names.contains(&"pick"));
        assert!(!names.contains(&"helper"));

        // Edge pick->helper is external; Scheduler->pick is internal.
        let external = pack.edges.iter().find(|e| e.kind == "calls").unwrap();
        assert!(!external.in_scope);
        let internal = pack.edges.iter().find(|e| e.kind == "contains").unwrap();
        assert!(internal.in_scope);
        assert_eq!(pack.stats.edges_internal, 1);
        assert_eq!(pack.stats.edges_external, 1);

        // Constant `3` is in scope (×2: the `>= 3` and `return 3`); `7` lives
        // in lib/other and must be excluded.
        assert!(pack.constants.iter().any(|c| c.value == "3"));
        assert!(pack.constants.iter().all(|c| c.value != "7"));

        // JSON key `count` from fromJson is captured.
        assert!(pack.json_keys.iter().any(|k| k.key == "count"));

        // Test suggestions only for in-scope symbols.
        assert!(pack
            .test_suggestions
            .iter()
            .all(|s| s.path.as_deref() == Some("lib/alarm/scheduler.dart")));
        assert!(pack.stats.test_suggestions >= 1);
    }

    #[test]
    fn unknown_path_errors() {
        let (store, dir) = setup();
        let err = build_feature_pack_with_store(
            &store,
            &FeaturePackOptions {
                repo_root: dir.path().to_path_buf(),
                selector: FeaturePackSelector::Path("lib/nope".to_string()),
                max_evidence_per_symbol: 30,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("lib/nope"));
    }
}
