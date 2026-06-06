//! `specslice stats` — summarise the per-command usage ledger written to
//! `<repo_root>/.specslice/stats.jsonl`.
//!
//! Every CLI invocation appends one record (command, duration, ok, metrics).
//! This command aggregates them into per-command call counts, total/avg/max
//! duration, error counts, and summed metrics (nodes queried / results returned
//! / coverage …) — answering "每个命令调用了多少，返回了多少".

use std::path::PathBuf;

use anyhow::Result;
use specslice_engine::stats::{load_stats, summarize, StatsSummary, STATS_REL_PATH};

pub struct StatsRunArgs {
    pub repo_root: PathBuf,
    pub json: bool,
    pub reset: bool,
}

pub fn run(args: StatsRunArgs) -> Result<()> {
    let path = args.repo_root.join(".specslice").join(STATS_REL_PATH);

    if args.reset {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        println!("已清空命令统计：{}", path.display());
        return Ok(());
    }

    let stats = load_stats(&path)?;
    let summary = summarize(&stats);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    print_human(&summary, &path.display().to_string());
    Ok(())
}

fn print_human(summary: &StatsSummary, path: &str) {
    println!("SpecSlice 命令统计");
    println!("账本: {path}");
    if summary.total_calls == 0 {
        println!("（暂无记录：运行任意命令后会自动累积）");
        return;
    }
    println!(
        "总计: {} 次调用，{} 次失败",
        summary.total_calls, summary.total_errors
    );
    println!();
    println!(
        "{:<18} {:>6} {:>6} {:>9} {:>8} {:>8}  指标(累计)",
        "命令", "调用", "失败", "总耗时ms", "平均ms", "最大ms"
    );
    println!("{}", "-".repeat(78));
    for c in &summary.commands {
        let metrics = if c.metrics.is_empty() {
            String::new()
        } else {
            c.metrics
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!(
            "{:<18} {:>6} {:>6} {:>9} {:>8.1} {:>8}  {}",
            c.command, c.calls, c.errors, c.total_ms, c.avg_ms, c.max_ms, metrics
        );
    }
}
