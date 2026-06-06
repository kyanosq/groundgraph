//! `specslice graph-equiv` — 业务图等价 (P24+).
//!
//! Scopes the *same business slice* in two prebuilt graph databases (e.g.
//! Java source ↔ Go rewrite) and emits a quantified structural comparison:
//! node counts (by kind / family), internal edge counts, and name coverage.
//! The JSON output is meant to be fed to an AI to walk the subgraph and audit
//! each divergence.
//!
//! ```text
//! specslice graph-equiv --source-db java/.specslice/graph.db \
//!                       --target-db go/.specslice/graph.db \
//!                       --source-scope 'rcmtm-cloud-craft/**' \
//!                       --target-scope 'internal/craft/**' --ignore-case
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_engine::graph_equiv::{
    analyze_graph_equiv, GraphEquivOptions, GraphEquivReport, SideNodeCounts,
};

#[derive(Debug, Clone)]
pub struct GraphEquivRunArgs {
    pub source_db: PathBuf,
    pub target_db: PathBuf,
    pub source_scope: Vec<String>,
    pub target_scope: Vec<String>,
    pub callables_only: bool,
    pub ignore_case: bool,
    pub normalize_names: bool,
    pub include_generated: bool,
    pub include_tests: bool,
    pub max: usize,
    pub json: bool,
}

pub fn run(args: GraphEquivRunArgs) -> Result<()> {
    let report = analyze_graph_equiv(GraphEquivOptions {
        source_db: args.source_db,
        target_db: args.target_db,
        source_scope: args.source_scope,
        target_scope: args.target_scope,
        include_types: !args.callables_only,
        include_tables: true,
        ignore_case: args.ignore_case,
        normalize_names: args.normalize_names,
        skip_generated: !args.include_generated,
        skip_tests: !args.include_tests,
        max_items: args.max,
    })
    .context("计算业务图等价")?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("序列化业务图等价报告")?
        );
    } else {
        print_human(&report);
    }
    Ok(())
}

fn scope_label(scope: &[String]) -> String {
    if scope.is_empty() {
        "<整图>".to_string()
    } else {
        scope.join(", ")
    }
}

fn fam(counts: &SideNodeCounts, family: &str) -> usize {
    counts.by_family.get(family).copied().unwrap_or(0)
}

fn print_human(report: &GraphEquivReport) {
    let n = &report.nodes;
    let m = &report.metrics;
    println!("SpecSlice 业务图等价 (schema v{})", report.schema_version);
    println!("源切片: {}", scope_label(&report.source_scope));
    println!("目标切片: {}", scope_label(&report.target_scope));
    println!();
    println!(
        "节点  源 {} (callable {} / type {})  ·  目标 {} (callable {} / type {})",
        n.source.total,
        fam(&n.source, "callable"),
        fam(&n.source, "type"),
        n.target.total,
        fam(&n.target, "callable"),
        fam(&n.target, "type"),
    );
    println!(
        "内部边  源 {}  ·  目标 {}",
        m.edge_internal_source, m.edge_internal_target,
    );
    let names = &report.names;
    println!(
        "名称  源去重 {} · 目标去重 {} · 已移植 {} · 缺失 {} · 目标独有 {} · 覆盖率 {:.1}%",
        names.source_distinct,
        names.target_distinct,
        names.ported,
        names.missing,
        names.extra,
        names.coverage * 100.0,
    );

    // Node-kind distribution (union of both sides).
    let mut kinds: Vec<&String> = n
        .source
        .by_kind
        .keys()
        .chain(n.target.by_kind.keys())
        .collect();
    kinds.sort();
    kinds.dedup();
    if !kinds.is_empty() {
        println!();
        println!("== 节点种类分布 (源 / 目标) ==");
        for k in kinds {
            let s = n.source.by_kind.get(k).copied().unwrap_or(0);
            let t = n.target.by_kind.get(k).copied().unwrap_or(0);
            println!("  {k:<24} {s:>4} / {t:<4}");
        }
    }

    // Internal-edge distribution.
    let e = &report.edges;
    let mut ekinds: Vec<&String> = e
        .source
        .by_kind
        .keys()
        .chain(e.target.by_kind.keys())
        .collect();
    ekinds.sort();
    ekinds.dedup();
    if !ekinds.is_empty() {
        println!();
        println!("== 内部边分布 (源 / 目标) ==");
        for k in ekinds {
            let s = e.source.by_kind.get(k).copied().unwrap_or(0);
            let t = e.target.by_kind.get(k).copied().unwrap_or(0);
            println!("  {k:<24} {s:>4} / {t:<4}");
        }
    }

    // Data-contract evidence: tables + columns.
    let tbl = &report.tables;
    if tbl.source_tables > 0 || tbl.target_tables > 0 {
        println!();
        println!(
            "== 数据契约(表结构)证据 — 表 源 {} / 目标 {}，匹配 {}，列覆盖率 {:.1}% ==",
            tbl.source_tables,
            tbl.target_tables,
            tbl.matched_tables.len(),
            tbl.column_coverage * 100.0,
        );
        for pt in &tbl.per_table {
            println!(
                "  {:<24} 列 源 {} / 目标 {} · 匹配 {} · 覆盖 {:.0}%{}",
                pt.table,
                pt.source_columns,
                pt.target_columns,
                pt.matched_columns,
                pt.coverage * 100.0,
                if pt.missing_columns.is_empty() {
                    String::new()
                } else {
                    format!(" · 缺列: {}", pt.missing_columns.join(", "))
                },
            );
        }
        for t in &tbl.missing_tables {
            println!("  - 缺表(源有目标无): {t}");
        }
        for t in &tbl.extra_tables {
            println!("  + 目标独有表: {t}");
        }
    }

    if !names.missing_names.is_empty() {
        println!();
        println!("== 缺失（源有·目标无）==");
        for name in &names.missing_names {
            println!("- {name}");
        }
    }
    if !names.extra_names.is_empty() {
        println!();
        println!("== 目标独有（源中无）==");
        for name in &names.extra_names {
            println!("+ {name}");
        }
    }
}
