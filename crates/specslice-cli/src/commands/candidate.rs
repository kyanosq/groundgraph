//! P5: CLI 候选审阅闭环。
//!
//! 三条入口：
//! - `specslice candidate list`     列出待审 / 已审候选（中文输出，带编号）
//! - `specslice candidate show <id>` 查看单个候选的完整业务描述、证据、风险、待确认问题
//! - `specslice candidate review <id> --accept|--reject|--needs-changes|--pending [--note "..."] [--answer "Q…"]`
//!   将审阅结果写回 `.specslice/candidates/business_logic.yaml`。

use std::path::Path;

use anyhow::{Context, Result};
use specslice_engine::{
    apply_review, list_for_review, BusinessCandidate, CandidateListSnapshot, ReviewStatus,
    ReviewVerdict,
};

/// CLI 状态过滤选项。
#[derive(Debug, Clone, Copy)]
pub enum ListMode {
    /// 仅列出 still needs review (默认)。
    Pending,
    /// 列出所有候选，含已 accepted / rejected。
    All,
}

pub fn run_list(repo_root: &Path, mode: ListMode, json: bool) -> Result<()> {
    let snapshot = list_for_review(repo_root)
        .with_context(|| format!("加载候选 ({})", repo_root.display()))?;
    if json {
        print_json(&snapshot, mode)?;
    } else {
        print_human_list(&snapshot, mode);
    }
    Ok(())
}

pub fn run_show(repo_root: &Path, candidate_id: &str, json: bool) -> Result<i32> {
    let snapshot = list_for_review(repo_root)
        .with_context(|| format!("加载候选 ({})", repo_root.display()))?;
    let candidate = snapshot
        .needs_review
        .iter()
        .chain(snapshot.already_reviewed.iter())
        .find(|c| c.id == candidate_id);
    let Some(c) = candidate else {
        eprintln!("specslice: 找不到候选 `{candidate_id}`。");
        return Ok(2);
    };
    if json {
        println!("{}", serde_json::to_string_pretty(c)?);
    } else {
        print_human_show(c);
    }
    Ok(0)
}

pub struct ReviewArgs<'a> {
    pub status: ReviewStatus,
    pub reviewer: Option<&'a str>,
    pub note: Option<&'a str>,
    /// 已回答 / 已忽略的 open question 文本。
    pub answered: Vec<String>,
    pub json: bool,
}

pub fn run_review(repo_root: &Path, candidate_id: &str, args: ReviewArgs<'_>) -> Result<i32> {
    let verdict = ReviewVerdict {
        status: args.status,
        reviewer: args.reviewer.map(String::from),
        note: args.note.map(String::from),
        answered_questions: args.answered,
        reviewed_at: None,
    };
    let outcome = apply_review(repo_root, candidate_id, verdict)
        .with_context(|| format!("写回审阅结果 ({})", candidate_id))?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        println!(
            "已记录审阅：候选 `{}` 状态 = {}（写入 {}）",
            outcome.candidate_id,
            outcome.status,
            outcome.path.display()
        );
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// 中文人类输出
// ---------------------------------------------------------------------------

fn print_human_list(snapshot: &CandidateListSnapshot, mode: ListMode) {
    if snapshot.warnings.iter().any(|w| !w.is_empty()) {
        for w in &snapshot.warnings {
            eprintln!("specslice: 候选文件警告：{w}");
        }
    }
    println!("SpecSlice 候选审阅清单");
    println!("文件: {}", snapshot.path.display());
    println!(
        "状态: 待审 {} 条，已审 {} 条。",
        snapshot.needs_review.len(),
        snapshot.already_reviewed.len()
    );

    if matches!(mode, ListMode::All | ListMode::Pending) {
        println!();
        println!("== 待审 ({}) ==", snapshot.needs_review.len());
        if snapshot.needs_review.is_empty() {
            println!("（无）");
        } else {
            for (i, c) in snapshot.needs_review.iter().enumerate() {
                print_list_entry(i + 1, c);
            }
        }
    }

    if matches!(mode, ListMode::All) {
        println!();
        println!("== 已审 ({}) ==", snapshot.already_reviewed.len());
        if snapshot.already_reviewed.is_empty() {
            println!("（无）");
        } else {
            for (i, c) in snapshot.already_reviewed.iter().enumerate() {
                print_list_entry(i + 1, c);
            }
        }
    }

    println!();
    println!(
        "提示: `specslice candidate show <id>` 查看详情；\
         `specslice candidate review <id> --accept|--reject|--needs-changes|--pending --note \"...\"` 写回审阅结果。"
    );
}

fn print_list_entry(index: usize, c: &BusinessCandidate) {
    let conf = c
        .confidence
        .map(|v| format!("{:.2}", v))
        .unwrap_or_else(|| "—".into());
    let status_label = status_label_cn(c);
    println!();
    println!("[{:>2}] {}  ({})", index, c.name, c.id);
    println!("     状态: {}  可信度: {}", status_label, conf);
    if !c.description.trim().is_empty() {
        for line in c.description.lines().take(4) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            println!("     {}", truncate_for_terminal(line, 100));
        }
    }
    if !c.risks.is_empty() {
        println!("     风险:");
        for r in &c.risks {
            println!("       · {}", truncate_for_terminal(r, 96));
        }
    }
    if let Some(rec) = c.recommendation.as_deref() {
        println!("     建议: {}", truncate_for_terminal(rec, 96));
    }
    let pending = c.pending_open_questions();
    if !pending.is_empty() {
        println!("     待确认问题 ({}):", pending.len());
        for q in pending {
            println!("       ? {}", truncate_for_terminal(q, 96));
        }
    }
    if let Some(review) = c.review.as_ref() {
        let mut bits = Vec::new();
        if let Some(r) = review.reviewer.as_deref() {
            bits.push(format!("by {r}"));
        }
        if let Some(t) = review.reviewed_at.as_deref() {
            bits.push(format!("at {t}"));
        }
        if !bits.is_empty() {
            println!("     审阅: {}", bits.join("，"));
        }
        if let Some(n) = review.note.as_deref() {
            println!("     备注: {}", truncate_for_terminal(n, 100));
        }
    }
}

fn print_human_show(c: &BusinessCandidate) {
    println!("候选: {}", c.name);
    println!("ID: {}", c.id);
    println!(
        "状态: {}  可信度: {}",
        status_label_cn(c),
        c.confidence
            .map(|v| format!("{:.2}", v))
            .unwrap_or_else(|| "—".into())
    );
    if !c.description.trim().is_empty() {
        println!();
        println!("业务描述:");
        for line in c.description.lines() {
            println!("  {line}");
        }
    }
    if !c.evidence.is_empty() {
        println!();
        println!("证据 ({}):", c.evidence.len());
        for e in &c.evidence {
            println!("  · {e}");
        }
    }
    if !c.risks.is_empty() {
        println!();
        println!("风险 ({}):", c.risks.len());
        for r in &c.risks {
            println!("  · {r}");
        }
    }
    if let Some(rec) = c.recommendation.as_deref() {
        println!();
        println!("建议: {rec}");
    }
    let pending = c.pending_open_questions();
    if !pending.is_empty() {
        println!();
        println!("待确认问题 ({}):", pending.len());
        for (i, q) in pending.iter().enumerate() {
            println!("  {}. {}", i + 1, q);
        }
    }
    if let Some(review) = c.review.as_ref() {
        println!();
        println!("最近一次审阅:");
        println!("  状态: {}", review.status);
        if let Some(r) = review.reviewer.as_deref() {
            println!("  审阅人: {r}");
        }
        if let Some(t) = review.reviewed_at.as_deref() {
            println!("  时间: {t}");
        }
        if let Some(n) = review.note.as_deref() {
            println!("  备注: {n}");
        }
        if !review.answered_questions.is_empty() {
            println!("  已回答:");
            for q in &review.answered_questions {
                println!("    · {q}");
            }
        }
    }
    println!();
    println!(
        "动作: `specslice candidate review {} --accept|--reject|--needs-changes|--pending --note \"...\"`",
        c.id
    );
}

fn status_label_cn(c: &BusinessCandidate) -> String {
    match c.review_status() {
        Some(ReviewStatus::Accepted) => "已接受".into(),
        Some(ReviewStatus::Rejected) => "已拒绝".into(),
        Some(ReviewStatus::NeedsChanges) => "需补充".into(),
        Some(ReviewStatus::Pending) => "待定".into(),
        None => "AI 候选 (未审阅)".into(),
    }
}

fn truncate_for_terminal(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn print_json(snapshot: &CandidateListSnapshot, mode: ListMode) -> Result<()> {
    use serde::Serialize;
    #[derive(Serialize)]
    struct CandidateSummary<'a> {
        id: &'a str,
        name: &'a str,
        status_cn: String,
        confidence: Option<f32>,
        description: &'a str,
        risks: &'a [String],
        recommendation: Option<&'a str>,
        evidence: &'a [String],
        pending_open_questions: Vec<&'a str>,
        review: Option<&'a specslice_engine::CandidateReview>,
    }
    #[derive(Serialize)]
    struct Out<'a> {
        path: &'a std::path::Path,
        warnings: &'a [String],
        needs_review: Vec<CandidateSummary<'a>>,
        already_reviewed: Vec<CandidateSummary<'a>>,
    }

    fn summarise(c: &BusinessCandidate) -> CandidateSummary<'_> {
        CandidateSummary {
            id: &c.id,
            name: &c.name,
            status_cn: match c.review_status() {
                Some(ReviewStatus::Accepted) => "已接受".into(),
                Some(ReviewStatus::Rejected) => "已拒绝".into(),
                Some(ReviewStatus::NeedsChanges) => "需补充".into(),
                Some(ReviewStatus::Pending) => "待定".into(),
                None => "ai_proposed".into(),
            },
            confidence: c.confidence,
            description: &c.description,
            risks: &c.risks,
            recommendation: c.recommendation.as_deref(),
            evidence: &c.evidence,
            pending_open_questions: c.pending_open_questions(),
            review: c.review.as_ref(),
        }
    }

    let needs: Vec<_> = snapshot.needs_review.iter().map(summarise).collect();
    let done: Vec<_> = if matches!(mode, ListMode::All) {
        snapshot.already_reviewed.iter().map(summarise).collect()
    } else {
        Vec::new()
    };
    let out = Out {
        path: &snapshot.path,
        warnings: &snapshot.warnings,
        needs_review: needs,
        already_reviewed: done,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
