//! `groundgraph questions` — P19 clarifying-questions report.

use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_engine::questions::{analyze_questions, QuestionsOptions, QuestionsReport};

#[derive(Debug, Clone)]
pub struct QuestionsRunArgs {
    pub repo_root: PathBuf,
    pub max_per_category: usize,
    pub format: String,
}

pub fn run(args: QuestionsRunArgs) -> Result<()> {
    let report = analyze_questions(QuestionsOptions {
        repo_root: args.repo_root,
        max_per_category: args.max_per_category,
    })
    .context("computing clarification questions")?;
    match args.format.as_str() {
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialising questions report")?
        ),
        "text" | "" => print_text(&report),
        other => anyhow::bail!("unsupported --format `{other}` (expected text|json)"),
    }
    Ok(())
}

fn print_text(r: &QuestionsReport) {
    println!("GroundGraph questions (P19 · AI 澄清问题包)");
    println!("总计 {} 条问题", r.stats.total_questions);
    for (cat, n) in &r.stats.by_category {
        println!("  · {cat}: {n}");
    }
    println!();
    if r.questions.is_empty() {
        println!("(代码图已经足够完整 — 没有需要澄清的事实)");
        return;
    }
    let mut last_category: Option<&str> = None;
    for q in &r.questions {
        if Some(q.category.as_str()) != last_category {
            println!();
            println!("== {} ({}) ==", q.category, q.severity);
            last_category = Some(q.category.as_str());
        }
        println!("  Q: {}", q.prompt);
        if let Some(id) = q.artifact_id.as_deref() {
            println!("     id:   {}", id);
        }
        if let Some(p) = q.path.as_deref() {
            println!("     path: {}", p);
        }
    }
    println!();
    println!(
        "提示: 这份列表是给 AI 助手或评审者读的 — 它不会自动决定 accept / reject。每条问题都附带 artifact id，可以用 `groundgraph graph --focus <id>` 查看上下文。"
    );
}
