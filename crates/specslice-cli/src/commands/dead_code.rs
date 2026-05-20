//! `specslice dead-code` — reports symbols that no entry point can
//! reach, with explicit confidence + reasons. Never auto-deletes.
//!
//! ```text
//! specslice dead-code                       # default: medium+
//! specslice dead-code --json                # JSON for agents
//! specslice dead-code --min-confidence high # only confident hits
//! specslice dead-code --include-tests       # also report orphan tests
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_engine::dead_code::{
    analyze_dead_code, DeadCodeCandidate, DeadCodeConfidence, DeadCodeOptions, DeadCodeReport,
};

#[derive(Debug, Clone)]
pub struct DeadCodeRunArgs {
    pub repo_root: PathBuf,
    pub min_confidence: DeadCodeConfidence,
    pub include_tests: bool,
    pub json: bool,
}

pub fn run(args: DeadCodeRunArgs) -> Result<()> {
    let report = analyze_dead_code(DeadCodeOptions {
        repo_root: args.repo_root,
        min_confidence: args.min_confidence,
        include_tests: args.include_tests,
    })
    .context("running dead-code analysis")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialising dead-code report")?
        );
    } else {
        print_human(&report);
    }
    Ok(())
}

fn print_human(report: &DeadCodeReport) {
    println!("SpecSlice dead-code");
    println!(
        "总符号 {} · 入口点 {} · 可达 {} · 可能死 {} · 被 ignore 过滤 {}",
        report.stats.total_code_symbols,
        report.stats.entrypoints,
        report.stats.reachable,
        report.stats.possibly_dead,
        report.stats.ignored_by_pattern,
    );
    println!("最低置信度: {}", report.min_confidence);
    println!();
    if report.candidates.is_empty() {
        println!("(没有符合条件的死代码候选)");
        return;
    }
    let mut last_bucket: Option<DeadCodeConfidence> = None;
    for c in &report.candidates {
        if last_bucket != Some(c.confidence) {
            println!();
            println!("== 置信度: {} ==", confidence_label(c.confidence));
            last_bucket = Some(c.confidence);
        }
        print_candidate(c);
    }
    println!();
    println!(
        "提示: 该报告不会自动删除任何文件。先用 `specslice graph --focus <id>` 或 `specslice search` 确认符号没有被反射 / 代码生成 / 外部消费者使用，再做删除决策。"
    );
}

fn print_candidate(c: &DeadCodeCandidate) {
    let line = c
        .line_range
        .map(|(s, e)| format!(":{s}-{e}"))
        .unwrap_or_default();
    let path = c.path.clone().unwrap_or_default();
    println!("- {}  ({})", c.label, c.kind);
    println!("    id: {}", c.id);
    if !path.is_empty() {
        println!("    路径: {path}{line}");
    }
    if !c.reasons.is_empty() {
        println!("    原因:");
        for r in &c.reasons {
            println!("      - {r}");
        }
    }
    if !c.inbound_sources.is_empty() {
        println!("    入边来源:");
        for src in c.inbound_sources.iter().take(5) {
            println!("      - {src}");
        }
        if c.inbound_sources.len() > 5 {
            println!("      ... 还有 {} 个", c.inbound_sources.len() - 5);
        }
    }
}

fn confidence_label(c: DeadCodeConfidence) -> &'static str {
    match c {
        DeadCodeConfidence::High => "high — 大概率可以删除（请人工复核）",
        DeadCodeConfidence::Medium => "medium — 可能被框架 / 反射 / 外部消费者使用",
        DeadCodeConfidence::Low => "low — 仅形成 dead island，证据较弱",
    }
}
