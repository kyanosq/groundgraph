//! P3 — `groundgraph logic` 输出业务逻辑可信度报告。

use std::path::Path;

use anyhow::Result;
use groundgraph_engine::{
    run_logic_confidence, LogicConfidenceItem, LogicConfidenceKind, LogicConfidenceOptions,
    LogicConfidenceReport, LogicConfidenceSource,
};

pub fn run(repo_root: &Path, json: bool, only_risks: bool) -> Result<i32> {
    let mut report = run_logic_confidence(LogicConfidenceOptions {
        repo_root: repo_root.to_path_buf(),
    })?;
    if only_risks {
        report.items.retain(|it| it.verdict.is_risk());
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    let exit = if report.items.iter().any(|it| {
        matches!(
            it.verdict,
            LogicConfidenceKind::StaleLink
                | LogicConfidenceKind::MissingLink
                | LogicConfidenceKind::MissingTest
        )
    }) {
        // CLI 友好：存在硬风险 (stale/missing) 时返回 1，便于 CI 捕获。
        1
    } else {
        0
    };
    Ok(exit)
}

fn print_human(report: &LogicConfidenceReport) {
    if !report.warnings.is_empty() {
        for w in &report.warnings {
            eprintln!("groundgraph: 警告：{w}");
        }
    }
    let s = &report.summary;
    println!("GroundGraph 逻辑可信度报告");
    println!("仓库: {}", report.repo_root);
    println!(
        "汇总: 已确认 {} / 需复核 {} / 需补充 {} / 缺测试 {} / 缺文档 {} / 未关联 {} / 候选 {} / 已拒绝 {} / 未知 {}",
        s.confirmed_link,
        s.stale_link,
        s.needs_changes,
        s.missing_test,
        s.missing_doc,
        s.missing_link,
        s.candidate_only,
        s.rejected,
        s.unknown,
    );
    if report.items.is_empty() {
        println!();
        println!("（无 requirement / 业务候选条目）");
        return;
    }
    println!();
    for (i, it) in report.items.iter().enumerate() {
        print_item(i + 1, it);
    }
}

fn print_item(index: usize, it: &LogicConfidenceItem) {
    let kind_label = match it.kind {
        LogicConfidenceSource::Requirement => "需求",
        LogicConfidenceSource::BusinessCandidate => "AI 候选",
    };
    let conf = it
        .confidence
        .map(|v| format!("{:.2}", v))
        .unwrap_or_else(|| "—".into());
    println!(
        "[{:>2}] {} [{}] {}",
        index, it.label_cn, kind_label, it.title
    );
    println!("     id: {}  可信度: {}", it.id, conf);
    if let Some(p) = it.path.as_deref() {
        println!("     文件: {p}");
    }
    if !it.risks.is_empty() {
        println!("     风险:");
        for r in &it.risks {
            println!("       · {r}");
        }
    }
    if !it.issues.is_empty() {
        println!("     问题:");
        for issue in &it.issues {
            println!("       · {issue}");
        }
    }
    println!();
}
