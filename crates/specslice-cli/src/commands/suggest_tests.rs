//! `specslice suggest-tests` — test suggestions from facts (P24 gap #5).
//!
//! ```text
//! specslice suggest-tests                 # prioritised checklist, branchy first
//! specslice suggest-tests --only-pure     # cheap deterministic wins first
//! specslice suggest-tests --max 20        # top 20 highest-value symbols
//! specslice suggest-tests --json          # machine-readable for an agent
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_engine::test_suggestions::{
    analyze_test_suggestions, TestSuggestionsOptions, TestSuggestionsReport,
};

#[derive(Debug, Clone)]
pub struct SuggestTestsRunArgs {
    pub repo_root: PathBuf,
    pub include_types: bool,
    pub only_pure: bool,
    pub min_priority: u32,
    pub max: usize,
    pub json: bool,
}

pub fn run(args: SuggestTestsRunArgs) -> Result<()> {
    let report = analyze_test_suggestions(TestSuggestionsOptions {
        repo_root: args.repo_root,
        include_types: args.include_types,
        only_pure: args.only_pure,
        min_priority: args.min_priority,
        max_symbols: args.max,
    })
    .context("生成测试建议")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("序列化测试建议")?
        );
    } else {
        print_human(&report);
    }
    Ok(())
}

fn print_human(report: &TestSuggestionsReport) {
    println!("SpecSlice 测试建议 (schema v{})", report.schema_version);
    println!(
        "分析 {} · 有建议 {} · 建议总数 {} · 输出 {}{}",
        report.stats.analyzed,
        report.stats.with_suggestions,
        report.stats.total_suggestions,
        report.stats.returned,
        if report.stats.truncated {
            "（已截断）"
        } else {
            ""
        },
    );
    println!();
    if report.items.is_empty() {
        println!("(没有达到优先级阈值的符号)");
        return;
    }
    for item in &report.items {
        let loc = match (&item.path, item.line_range) {
            (Some(p), Some((s, e))) => format!("{p}:{s}-{e}"),
            (Some(p), None) => p.clone(),
            _ => String::new(),
        };
        let name = item.name.clone().unwrap_or_else(|| item.id.clone());
        println!(
            "- {name}  ({})  [{}]  优先级 {}",
            item.kind, item.purity, item.priority
        );
        if !loc.is_empty() {
            println!("    路径: {loc}");
        }
        for s in &item.suggestions {
            println!("    [{}] {}", s.kind.as_str(), s.message);
            for h in &s.hints {
                println!("        · {h}");
            }
        }
        println!();
    }
}
