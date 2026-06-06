//! `specslice port-coverage` — porting coverage ledger (P24 gap #4).
//!
//! Diffs two prebuilt graph databases by symbol name to show what has been
//! ported from a source project into a target rewrite, and what is missing.
//!
//! ```text
//! specslice port-coverage --source-db old/.specslice/graph.db \
//!                         --target-db new/.specslice/graph.db
//! specslice port-coverage --source-db a.db --target-db b.db --json
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_engine::port_coverage::{
    analyze_port_coverage, load_port_map, PortCoverageOptions, PortCoverageReport,
};

#[derive(Debug, Clone)]
pub struct PortCoverageRunArgs {
    pub source_db: PathBuf,
    pub target_db: PathBuf,
    pub include_types: bool,
    pub include_extra: bool,
    pub include_generated: bool,
    pub include_tests: bool,
    pub include_synthetic: bool,
    pub normalize_names: bool,
    pub ignore_case: bool,
    /// Optional YAML port-map of source→target name aliases.
    pub port_map: Option<PathBuf>,
    pub exclude: Vec<String>,
    pub max: usize,
    pub json: bool,
}

pub fn run(args: PortCoverageRunArgs) -> Result<()> {
    let port_map = match &args.port_map {
        Some(path) => load_port_map(path).context("加载移植映射 (--port-map)")?,
        None => Default::default(),
    };
    let report = analyze_port_coverage(PortCoverageOptions {
        source_db: args.source_db,
        target_db: args.target_db,
        include_types: args.include_types,
        include_extra: args.include_extra,
        skip_generated: !args.include_generated,
        skip_tests: !args.include_tests,
        skip_synthetic_names: !args.include_synthetic,
        normalize_names: args.normalize_names,
        ignore_case: args.ignore_case,
        aliases: port_map.aliases,
        ignore_names: port_map.ignore_names.into_iter().collect(),
        ignore_name_prefixes: port_map.ignore_name_prefixes,
        exclude: args.exclude,
        max_items: args.max,
    })
    .context("计算移植覆盖率")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("序列化覆盖率账本")?
        );
    } else {
        print_human(&report);
    }
    Ok(())
}

fn print_human(report: &PortCoverageReport) {
    let s = &report.stats;
    println!("SpecSlice 移植覆盖率账本 (schema v{})", report.schema_version);
    println!(
        "源符号 {} (去重名 {}) · 目标符号 {} (去重名 {})",
        s.source_symbols, s.source_distinct_names, s.target_symbols, s.target_distinct_names,
    );
    println!(
        "已移植 {} · 缺失 {} · 目标独有 {} · 覆盖率 {:.1}%",
        s.ported_names,
        s.missing_names,
        s.extra_names,
        s.coverage * 100.0,
    );
    let via_alias = report.ported.iter().filter(|p| p.via_alias.is_some()).count();
    if via_alias > 0 {
        println!("其中经移植映射(--port-map)命中 {via_alias} 个改名符号");
    }

    if !report.missing.is_empty() {
        println!();
        println!("== 仍缺失的源符号 ({}) ==", report.stats.missing_names);
        for m in &report.missing {
            let loc = match (&m.path, m.line) {
                (Some(p), Some(l)) => format!("  {p}:{l}"),
                (Some(p), None) => format!("  {p}"),
                _ => String::new(),
            };
            println!("- {}  ({}){}", m.name, m.kind, loc);
        }
    }

    if !report.by_file.is_empty() {
        println!();
        println!("== 按源文件覆盖率（覆盖低者在前）==");
        for f in report.by_file.iter().take(20) {
            println!(
                "{:>5.1}%  {}/{}  {}",
                f.coverage * 100.0,
                f.ported,
                f.total,
                f.path
            );
        }
        if report.by_file.len() > 20 {
            println!("... 还有 {} 个文件", report.by_file.len() - 20);
        }
    }

    if !report.extra.is_empty() {
        println!();
        println!("== 目标独有的名字（源中无）==");
        for name in report.extra.iter().take(30) {
            println!("+ {name}");
        }
        if report.extra.len() > 30 {
            println!("... 还有 {} 个", report.extra.len() - 30);
        }
    }
}
