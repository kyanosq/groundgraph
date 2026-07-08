//! `groundgraph trace` — 接口 → 整张图：把一个端点/符号的**完整下游链路**
//! （controller → service → impl → mapper → SQL → table）一次性捞出来。
//!
//! 与 `search`（1 跳并集）、`graph --view focus`（焦点+后代+1 跳）不同，`trace`
//! 沿调用/引用/持久化边做有界前向传递闭包，回答「这个接口背后牵动了图里的哪些
//! 东西、最终落到哪几张表」。移植 / 影响分析的主力命令。

use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_engine::trace::{run_trace, TraceOptions, TraceResult};

#[derive(Debug, Clone)]
pub struct TraceRunArgs {
    pub repo_root: PathBuf,
    pub query: String,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_seeds: usize,
    pub include_noise: bool,
    pub json: bool,
}

/// Layer print order — data flows top to bottom.
const LAYER_ORDER: &[&str] = &[
    "route",
    "controller",
    "service",
    "service_impl",
    "mapper",
    "sql",
    "table",
    "other",
];

pub fn run(args: TraceRunArgs) -> Result<()> {
    let options = TraceOptions {
        repo_root: args.repo_root.clone(),
        query: args.query.clone(),
        max_nodes: args.max_nodes.max(1),
        max_depth: args.max_depth,
        max_seeds: args.max_seeds.max(1),
        include_noise: args.include_noise,
    };
    let result = run_trace(options).context("running trace")?;
    groundgraph_engine::stats::set_metric("trace_nodes", result.nodes.len() as i64);
    groundgraph_engine::stats::set_metric("trace_edges", result.edges.len() as i64);
    groundgraph_engine::stats::set_metric("trace_tables", result.tables.len() as i64);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).context("serialising trace result")?
        );
    } else {
        print_human(&result);
    }
    Ok(())
}

fn print_human(r: &TraceResult) {
    println!("GroundGraph trace（接口 → 整张图）");
    println!("查询: {}", r.query);
    if r.seeds.is_empty() {
        println!("\n(无命中：换个符号名，或先 `groundgraph index`)");
        return;
    }
    println!("种子: {} 个", r.seeds.len());
    for s in &r.seeds {
        println!("  • {s}");
    }
    println!(
        "\n链路规模: 节点 {} · 边 {} · 触达表 {}{}",
        r.nodes.len(),
        r.edges.len(),
        r.tables.len(),
        if r.truncated { " (已截断)" } else { "" }
    );

    println!("\n== 分层 ==");
    for layer in LAYER_ORDER {
        let count = r.layer_counts.get(*layer).copied().unwrap_or(0);
        if count == 0 {
            continue;
        }
        println!("  {layer:<13} {count}");
        for n in r.nodes.iter().filter(|n| &n.layer == layer).take(12) {
            let path = n.path.clone().unwrap_or_default();
            println!("      d{} {}  [{}]", n.depth, n.label, path);
        }
        let shown = r.nodes.iter().filter(|n| &n.layer == layer).count();
        if shown > 12 {
            println!("      … 其余 {} 个", shown - 12);
        }
    }

    if !r.tables.is_empty() {
        println!("\n== 最终触达的表 ({}) ==", r.tables.len());
        println!("  {}", r.tables.join(", "));
    }
}
