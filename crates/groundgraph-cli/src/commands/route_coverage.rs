//! `groundgraph route-coverage` — HTTP route porting coverage (P26).
//!
//! Diffs two prebuilt graph databases by **normalized route path** to show
//! which routes the client consumes that the rewritten server already serves,
//! and which are still missing — the API-surface counterpart of
//! `port-coverage` (which diffs symbol names).
//!
//! ```text
//! groundgraph route-coverage --source-db client/.groundgraph/graph.db \
//!                          --target-db server/.groundgraph/graph.db
//! groundgraph route-coverage --source-db a.db --target-db b.db --include-extra --json
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_engine::route_coverage::{
    analyze_route_coverage, RouteCoverageOptions, RouteCoverageReport,
};

#[derive(Debug, Clone)]
pub struct RouteCoverageRunArgs {
    pub source_db: PathBuf,
    pub target_db: PathBuf,
    pub suffix_segments: usize,
    pub include_extra: bool,
    pub exclude: Vec<String>,
    pub max: usize,
    pub json: bool,
}

pub fn run(args: RouteCoverageRunArgs) -> Result<()> {
    let report = analyze_route_coverage(RouteCoverageOptions {
        source_db: args.source_db,
        target_db: args.target_db,
        suffix_segments: args.suffix_segments,
        include_extra: args.include_extra,
        exclude: args.exclude,
        max_items: args.max,
    })
    .context("计算路由移植覆盖率")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("序列化路由覆盖率账本")?
        );
    } else {
        print_human(&report);
    }
    Ok(())
}

fn print_human(report: &RouteCoverageReport) {
    let s = &report.stats;
    println!(
        "GroundGraph 路由移植覆盖率账本 (schema v{} · 匹配键=末 {} 段)",
        report.schema_version, report.suffix_segments,
    );
    println!(
        "消费路由 {} (去重端点 {}) · 服务端路由 {} (去重端点 {})",
        s.source_routes, s.source_distinct_keys, s.target_routes, s.target_distinct_keys,
    );
    println!(
        "已服务 {} · 缺失 {} · 服务端独有 {} · 覆盖率 {:.1}%",
        s.ported_keys,
        s.missing_keys,
        s.extra_keys,
        s.coverage * 100.0,
    );

    if !report.by_service.is_empty() {
        println!();
        println!("== 按服务覆盖率（覆盖低者在前）==");
        for svc in &report.by_service {
            println!(
                "{:>5.1}%  {}/{}  {}",
                svc.coverage * 100.0,
                svc.ported,
                svc.total,
                if svc.service.is_empty() {
                    "(无前缀)"
                } else {
                    &svc.service
                },
            );
        }
    }

    if !report.missing.is_empty() {
        println!();
        println!("== 仍缺失的消费路由（移植待办）==");
        for m in &report.missing {
            println!("- {}", m.path);
        }
    }

    if !report.extra.is_empty() {
        println!();
        println!("== 服务端独有路由（无消费方）==");
        for e in &report.extra {
            println!("+ {}", e.path);
        }
    }
}
