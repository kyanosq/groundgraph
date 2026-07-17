//! 业务图等价 (graph-equiv) — structural + name equivalence between the same
//! business slice in two graphs (e.g. Java source ↔ Go rewrite).
//!
//! `port-coverage` answers "which symbol names exist on both sides" globally.
//! This module answers a sharper, *quantified* question for one business
//! subgraph: scoped to a path glob on each side, how do the two graphs compare
//! in **node counts** (by kind / by family), **edge counts** (by kind), and
//! **name coverage** — and emit AI-traversable JSON so an agent can walk the
//! subgraph and audit each divergence.
//!
//! The numbers are the point: they make "the Go port faithfully replaces the
//! Java service" a measurable claim, not an assertion.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use groundgraph_core::language_traits::{is_callable, is_type};
use groundgraph_core::{Node, NodeKind};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

use crate::error::EngineResult;
use crate::path_class::{is_generated_path, is_test_path};
use crate::schema_indexer::{normalize_column, DbTableMeta};

pub const GRAPH_EQUIV_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SideNodeCounts {
    pub total: usize,
    /// Count per concrete node kind (e.g. `java_method`, `go_function`).
    pub by_kind: BTreeMap<String, usize>,
    /// Count per language-neutral family: `callable` / `type` / `other`.
    pub by_family: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeComparison {
    pub source: SideNodeCounts,
    pub target: SideNodeCounts,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SideEdgeCounts {
    /// Edges whose *both* endpoints are in scope (the subgraph's internal wiring).
    pub total_internal: usize,
    pub by_kind: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EdgeComparison {
    pub source: SideEdgeCounts,
    pub target: SideEdgeCounts,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NameComparison {
    pub source_distinct: usize,
    pub target_distinct: usize,
    pub ported: usize,
    pub missing: usize,
    pub extra: usize,
    /// `ported / source_distinct` (0.0 when no source names).
    pub coverage: f32,
    pub missing_names: Vec<String>,
    pub extra_names: Vec<String>,
}

/// Per-table column parity (the data-contract evidence).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableColumnsDiff {
    pub table: String,
    pub source_columns: usize,
    pub target_columns: usize,
    pub matched_columns: usize,
    /// Columns present in source but missing in target.
    pub missing_columns: Vec<String>,
    /// Columns present in target but absent in source.
    pub extra_columns: Vec<String>,
    pub coverage: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableComparison {
    pub source_tables: usize,
    pub target_tables: usize,
    pub matched_tables: Vec<String>,
    /// Tables in source but missing in target.
    pub missing_tables: Vec<String>,
    /// Tables in target but absent in source.
    pub extra_tables: Vec<String>,
    /// Matched columns / source columns across all matched tables.
    pub column_coverage: f32,
    pub per_table: Vec<TableColumnsDiff>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EquivMetrics {
    pub name_coverage: f32,
    pub callable_source: usize,
    pub callable_target: usize,
    pub type_source: usize,
    pub type_target: usize,
    pub edge_internal_source: usize,
    pub edge_internal_target: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphEquivReport {
    pub schema_version: u32,
    pub source_scope: Vec<String>,
    pub target_scope: Vec<String>,
    pub nodes: NodeComparison,
    pub edges: EdgeComparison,
    pub names: NameComparison,
    /// Data-contract evidence: DB table + column parity.
    pub tables: TableComparison,
    pub metrics: EquivMetrics,
}

#[derive(Debug, Clone)]
pub struct GraphEquivOptions {
    pub source_db: PathBuf,
    pub target_db: PathBuf,
    /// Path globs scoping the source business slice (empty = whole graph).
    pub source_scope: Vec<String>,
    pub target_scope: Vec<String>,
    pub include_types: bool,
    /// Include `DbTable` nodes (data-contract evidence). Default true.
    pub include_tables: bool,
    pub ignore_case: bool,
    pub normalize_names: bool,
    pub skip_generated: bool,
    pub skip_tests: bool,
    /// Cap `missing` / `extra` name list lengths (0 = unlimited).
    pub max_items: usize,
}

impl Default for GraphEquivOptions {
    fn default() -> Self {
        Self {
            source_db: PathBuf::new(),
            target_db: PathBuf::new(),
            source_scope: Vec::new(),
            target_scope: Vec::new(),
            include_types: true,
            include_tables: true,
            ignore_case: false,
            normalize_names: false,
            skip_generated: true,
            skip_tests: true,
            max_items: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn analyze_graph_equiv(options: GraphEquivOptions) -> EngineResult<GraphEquivReport> {
    let source = Store::open(&options.source_db)?;
    let target = Store::open(&options.target_db)?;
    Ok(analyze_graph_equiv_with_stores(&source, &target, &options)?)
}

pub fn analyze_graph_equiv_with_stores(
    source: &Store,
    target: &Store,
    options: &GraphEquivOptions,
) -> Result<GraphEquivReport> {
    let src = collect_side(source, &options.source_scope, options)?;
    let tgt = collect_side(target, &options.target_scope, options)?;

    let tgt_keys: BTreeSet<&String> = tgt.keyset.keys().collect();
    let src_keys: BTreeSet<&String> = src.keyset.keys().collect();

    let mut ported = 0usize;
    let mut missing_names: Vec<String> = Vec::new();
    for (k, raw) in &src.keyset {
        if tgt_keys.contains(k) {
            ported += 1;
        } else {
            missing_names.push(raw.clone());
        }
    }
    let mut extra_names: Vec<String> = tgt
        .keyset
        .iter()
        .filter(|(k, _)| !src_keys.contains(k))
        .map(|(_, raw)| raw.clone())
        .collect();
    missing_names.sort();
    extra_names.sort();

    let source_distinct = src.keyset.len();
    let target_distinct = tgt.keyset.len();
    let missing = missing_names.len();
    let extra = extra_names.len();
    let coverage = if source_distinct == 0 {
        0.0
    } else {
        ported as f32 / source_distinct as f32
    };
    if options.max_items > 0 {
        missing_names.truncate(options.max_items);
        extra_names.truncate(options.max_items);
    }

    let tables = compare_tables(&src.tables, &tgt.tables, options.max_items);

    let callable_source = src.counts.by_family.get("callable").copied().unwrap_or(0);
    let callable_target = tgt.counts.by_family.get("callable").copied().unwrap_or(0);
    let type_source = src.counts.by_family.get("type").copied().unwrap_or(0);
    let type_target = tgt.counts.by_family.get("type").copied().unwrap_or(0);

    Ok(GraphEquivReport {
        schema_version: GRAPH_EQUIV_SCHEMA_VERSION,
        source_scope: options.source_scope.clone(),
        target_scope: options.target_scope.clone(),
        metrics: EquivMetrics {
            name_coverage: coverage,
            callable_source,
            callable_target,
            type_source,
            type_target,
            edge_internal_source: src.edges.total_internal,
            edge_internal_target: tgt.edges.total_internal,
        },
        nodes: NodeComparison {
            source: src.counts,
            target: tgt.counts,
        },
        edges: EdgeComparison {
            source: src.edges,
            target: tgt.edges,
        },
        tables,
        names: NameComparison {
            source_distinct,
            target_distinct,
            ported,
            missing,
            extra,
            coverage,
            missing_names,
            extra_names,
        },
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

struct SideData {
    counts: SideNodeCounts,
    edges: SideEdgeCounts,
    /// Normalised name key → one representative raw name (for display).
    keyset: BTreeMap<String, String>,
    /// Table name key → (raw table name, raw column names).
    tables: BTreeMap<String, (String, Vec<String>)>,
}

fn collect_side(store: &Store, scope: &[String], options: &GraphEquivOptions) -> Result<SideData> {
    let globs = build_globset(scope)?;
    let nodes = store.list_all_nodes().context("listing nodes")?;

    let mut counts = SideNodeCounts::default();
    let mut keyset: BTreeMap<String, String> = BTreeMap::new();
    let mut tables: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let mut eligible_ids: BTreeSet<String> = BTreeSet::new();

    for n in &nodes {
        let (Some(path), Some(name)) = (n.path.as_deref(), n.name.as_deref()) else {
            continue;
        };
        let is_table = n.kind == NodeKind::DbTable;
        let eligible = is_callable(n.kind)
            || (options.include_types && is_type(n.kind))
            || (options.include_tables && is_table);
        if !eligible {
            continue;
        }
        if options.skip_generated && is_generated_path(path) {
            continue;
        }
        if options.skip_tests && is_test_path(path) {
            continue;
        }
        if let Some(g) = &globs {
            if !g.is_match(path) {
                continue;
            }
        }
        counts.total += 1;
        *counts
            .by_kind
            .entry(n.kind.as_str().to_string())
            .or_default() += 1;
        *counts
            .by_family
            .entry(family_of_kind(n).to_string())
            .or_default() += 1;
        eligible_ids.insert(n.id.as_str().to_string());
        if is_table {
            tables
                .entry(name_key(name, options))
                .or_insert_with(|| (name.to_string(), table_columns(n)));
        } else {
            // Tables stay out of code-name coverage to avoid conflating a
            // table `craft` with a class `Craft`.
            keyset
                .entry(name_key(name, options))
                .or_insert_with(|| name.to_string());
        }
    }

    let mut edges = SideEdgeCounts::default();
    for e in store.list_all_edges().context("listing edges")? {
        if eligible_ids.contains(e.from_id.as_str()) && eligible_ids.contains(e.to_id.as_str()) {
            edges.total_internal += 1;
            *edges
                .by_kind
                .entry(e.kind.as_str().to_string())
                .or_default() += 1;
        }
    }

    Ok(SideData {
        counts,
        edges,
        keyset,
        tables,
    })
}

/// Read a `DbTable` node's column names from its `metadata_json`.
fn table_columns(node: &Node) -> Vec<String> {
    node.metadata_json
        .as_deref()
        .and_then(|j| serde_json::from_str::<DbTableMeta>(j).ok())
        .map(|m| m.columns.into_iter().map(|c| c.name).collect())
        .unwrap_or_default()
}

fn compare_tables(
    src: &BTreeMap<String, (String, Vec<String>)>,
    tgt: &BTreeMap<String, (String, Vec<String>)>,
    max_items: usize,
) -> TableComparison {
    let mut matched_tables = Vec::new();
    let mut missing_tables = Vec::new();
    let mut per_table = Vec::new();
    let mut total_src_cols = 0usize;
    let mut total_matched_cols = 0usize;

    for (key, (raw, src_cols)) in src {
        match tgt.get(key) {
            Some((_, tgt_cols)) => {
                matched_tables.push(raw.clone());
                let tgt_norm: BTreeSet<String> =
                    tgt_cols.iter().map(|c| normalize_column(c)).collect();
                let src_norm: BTreeSet<String> =
                    src_cols.iter().map(|c| normalize_column(c)).collect();
                let mut missing_columns: Vec<String> = src_cols
                    .iter()
                    .filter(|c| !tgt_norm.contains(&normalize_column(c)))
                    .cloned()
                    .collect();
                let mut extra_columns: Vec<String> = tgt_cols
                    .iter()
                    .filter(|c| !src_norm.contains(&normalize_column(c)))
                    .cloned()
                    .collect();
                // Count matches on the deduped, normalized column sets so a
                // duplicate column name (e.g. parser emitting `id, id`, or an
                // un-deduped `ALTER TABLE ADD COLUMN`) can neither over- nor
                // under-count coverage — `matched + missing` need not equal the
                // raw `src_cols.len()` (#96).
                let matched = src_norm.iter().filter(|c| tgt_norm.contains(*c)).count();
                total_src_cols += src_norm.len();
                total_matched_cols += matched;
                let coverage = if src_norm.is_empty() {
                    1.0
                } else {
                    matched as f32 / src_norm.len() as f32
                };
                missing_columns.sort();
                extra_columns.sort();
                if max_items > 0 {
                    missing_columns.truncate(max_items);
                    extra_columns.truncate(max_items);
                }
                per_table.push(TableColumnsDiff {
                    table: raw.clone(),
                    source_columns: src_cols.len(),
                    target_columns: tgt_cols.len(),
                    matched_columns: matched,
                    missing_columns,
                    extra_columns,
                    coverage,
                });
            }
            None => {
                missing_tables.push(raw.clone());
                total_src_cols += src_cols.len();
            }
        }
    }

    let src_keys: BTreeSet<&String> = src.keys().collect();
    let mut extra_tables: Vec<String> = tgt
        .iter()
        .filter(|(k, _)| !src_keys.contains(k))
        .map(|(_, (raw, _))| raw.clone())
        .collect();
    matched_tables.sort();
    missing_tables.sort();
    extra_tables.sort();
    per_table.sort_by(|a, b| a.table.cmp(&b.table));

    let column_coverage = if total_src_cols == 0 {
        0.0
    } else {
        total_matched_cols as f32 / total_src_cols as f32
    };

    TableComparison {
        source_tables: src.len(),
        target_tables: tgt.len(),
        matched_tables,
        missing_tables,
        extra_tables,
        column_coverage,
        per_table,
    }
}

fn build_globset(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p).with_context(|| format!("invalid scope glob: {p}"))?);
    }
    Ok(Some(builder.build().context("building scope globset")?))
}

fn family_of_kind(node: &Node) -> &'static str {
    if is_callable(node.kind) {
        "callable"
    } else if is_type(node.kind) {
        "type"
    } else if node.kind == NodeKind::DbTable {
        "table"
    } else {
        "other"
    }
}

/// Normalise a symbol name for cross-language matching. `ignore_case` folds
/// Java `camelCase` ↔ Go `PascalCase`; `normalize_names` additionally drops
/// non-alphanumerics so `snake_case` ↔ `camelCase` line up.
fn name_key(name: &str, options: &GraphEquivOptions) -> String {
    let mut key = if options.normalize_names {
        name.chars().filter(|c| c.is_alphanumeric()).collect()
    } else {
        name.to_string()
    };
    if options.ignore_case {
        key = key.to_lowercase();
    }
    key
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, NodeKind};

    fn store_with(
        nodes: &[(&str, NodeKind, &str)],
        edges: &[(&str, &str, EdgeKind)],
    ) -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        let mut id_of: BTreeMap<String, ArtifactId> = BTreeMap::new();
        for (i, (name, kind, path)) in nodes.iter().enumerate() {
            let id = ArtifactId::new(format!("{path}::{name}::{i}"));
            let mut n = Node::new(id.clone(), *kind);
            n.name = Some((*name).to_string());
            n.path = Some((*path).to_string());
            n.start_line = Some(u32::try_from(i + 1).unwrap());
            store.upsert_node(&n).unwrap();
            id_of.insert((*name).to_string(), id);
        }
        for (from, to, kind) in edges {
            let e = EdgeAssertion::declared(
                id_of[*from].clone(),
                id_of[*to].clone(),
                *kind,
                EdgeSource::Filesystem,
            );
            store.upsert_edge(&e).unwrap();
        }
        (store, dir)
    }

    #[test]
    fn quantifies_nodes_edges_and_name_coverage_in_scope() {
        // Source = Java craft-conflict slice; target = Go craft slice.
        let (source, _s) = store_with(
            &[
                (
                    "selectTrademarkConflictListTreeByCloth",
                    NodeKind::JavaMethod,
                    "craft/CraftConflictServiceImpl.java",
                ),
                (
                    "selectEmbConflictListTreeByCloth",
                    NodeKind::JavaMethod,
                    "craft/CraftConflictServiceImpl.java",
                ),
                (
                    "CraftConflict",
                    NodeKind::JavaClass,
                    "craft/CraftConflict.java",
                ),
                // out of scope — must be ignored by the source scope glob.
                (
                    "payOrder",
                    NodeKind::JavaMethod,
                    "order/OrderServiceImpl.java",
                ),
            ],
            &[(
                "selectTrademarkConflictListTreeByCloth",
                "selectEmbConflictListTreeByCloth",
                EdgeKind::Calls,
            )],
        );
        let (target, _t) = store_with(
            &[
                (
                    "SelectTrademarkConflictListTreeByCloth",
                    NodeKind::GoFunction,
                    "internal/craft/service/conflict.go",
                ),
                (
                    "CraftConflict",
                    NodeKind::GoMethod,
                    "internal/craft/model/models.go",
                ),
            ],
            &[],
        );

        let report = analyze_graph_equiv_with_stores(
            &source,
            &target,
            &GraphEquivOptions {
                source_scope: vec!["craft/**".to_string()],
                target_scope: vec!["internal/craft/**".to_string()],
                ignore_case: true,
                ..Default::default()
            },
        )
        .unwrap();

        // Nodes: scope drops payOrder → 3 source, 2 target.
        assert_eq!(report.nodes.source.total, 3);
        assert_eq!(report.nodes.target.total, 2);
        assert_eq!(report.nodes.source.by_family.get("callable"), Some(&2));
        assert_eq!(report.nodes.source.by_family.get("type"), Some(&1));

        // Edges: one internal Calls edge on the source side, none on target.
        assert_eq!(report.edges.source.total_internal, 1);
        assert_eq!(report.edges.source.by_kind.get("calls"), Some(&1));
        assert_eq!(report.edges.target.total_internal, 0);

        // Names (ignore_case): Trademark + CraftConflict ported; Emb missing.
        assert_eq!(report.names.source_distinct, 3);
        assert_eq!(report.names.ported, 2);
        assert_eq!(report.names.missing, 1);
        assert_eq!(
            report.names.missing_names,
            vec!["selectEmbConflictListTreeByCloth".to_string()]
        );
        assert!((report.names.coverage - 2.0 / 3.0).abs() < 1e-6);
    }

    fn add_table(store: &mut Store, path: &str, name: &str, cols: &[&str]) {
        use crate::schema_indexer::{db_table_node, ParsedColumn, ParsedTable};
        let table = ParsedTable {
            name: name.to_string(),
            columns: cols
                .iter()
                .map(|c| ParsedColumn {
                    name: (*c).to_string(),
                    definition: String::new(),
                })
                .collect(),
            source: "sql",
            line: 1,
        };
        store.upsert_node(&db_table_node(path, &table)).unwrap();
    }

    #[test]
    fn compares_db_table_columns_as_evidence() {
        // Source = Java entity (camelCase fields); target = Go schema (snake_case),
        // missing one column. Table name matches; columns audited.
        let dir_s = tempfile::TempDir::new().unwrap();
        let mut source = Store::open(dir_s.path().join("g.db")).unwrap();
        source.migrate().unwrap();
        add_table(
            &mut source,
            "craft/CraftConflict.java",
            "craft_conflict",
            &["id", "categoryId", "craftId", "legacyOnly"],
        );

        let dir_t = tempfile::TempDir::new().unwrap();
        let mut target = Store::open(dir_t.path().join("g.db")).unwrap();
        target.migrate().unwrap();
        add_table(
            &mut target,
            "internal/db/schema.sql",
            "craft_conflict",
            &["id", "category_id", "craft_id"],
        );

        let report = analyze_graph_equiv_with_stores(
            &source,
            &target,
            &GraphEquivOptions {
                ignore_case: true,
                ..Default::default()
            },
        )
        .unwrap();

        // Tables: one matched, no missing/extra table.
        let t = &report.tables;
        assert_eq!(t.source_tables, 1);
        assert_eq!(t.target_tables, 1);
        assert_eq!(t.matched_tables, vec!["craft_conflict".to_string()]);
        assert!(t.missing_tables.is_empty());
        // Column parity: id/categoryId/craftId match snake_case; legacyOnly missing.
        let pt = &t.per_table[0];
        assert_eq!(pt.source_columns, 4);
        assert_eq!(pt.matched_columns, 3);
        assert_eq!(pt.missing_columns, vec!["legacyOnly".to_string()]);
        assert!((pt.coverage - 0.75).abs() < 1e-6);
        // DbTable also shows up in the node-family audit as "table".
        assert_eq!(report.nodes.source.by_family.get("table"), Some(&1));
    }

    #[test]
    fn empty_scope_covers_whole_graph() {
        let (source, _s) = store_with(&[("A", NodeKind::JavaMethod, "x/A.java")], &[]);
        let (target, _t) = store_with(&[("A", NodeKind::GoFunction, "y/a.go")], &[]);
        let report =
            analyze_graph_equiv_with_stores(&source, &target, &GraphEquivOptions::default())
                .unwrap();
        assert_eq!(report.nodes.source.total, 1);
        assert_eq!(report.names.ported, 1);
        assert!((report.names.coverage - 1.0).abs() < 1e-6);
    }
}
