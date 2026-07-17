//! `groundgraph graph-diff` — P19 graph snapshot diff.

use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_engine::graph_diff::{diff_graphs, GraphDiff, GraphDiffOptions};

use super::output::TextJsonFormat;

#[derive(Debug, Clone)]
pub struct GraphDiffRunArgs {
    pub base_db: PathBuf,
    pub head_db: PathBuf,
    /// Optional repo root for the base snapshot. When both
    /// `base_repo_root` and `head_repo_root` are supplied, graph-diff
    /// also reports candidate added / removed / status-changed
    /// pulled from `.groundgraph/candidates/business_logic.yaml`.
    pub base_repo_root: Option<PathBuf>,
    pub head_repo_root: Option<PathBuf>,
    pub format: TextJsonFormat,
}

pub fn run(args: GraphDiffRunArgs) -> Result<()> {
    let report = diff_graphs(GraphDiffOptions {
        base_db: args.base_db,
        head_db: args.head_db,
        base_repo_root: args.base_repo_root,
        head_repo_root: args.head_repo_root,
    })
    .context("computing graph diff")?;
    match args.format {
        TextJsonFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialising graph diff")?
        ),
        TextJsonFormat::Text => print_text(&report),
    }
    Ok(())
}

fn print_text(r: &GraphDiff) {
    println!("GroundGraph graph-diff");
    println!(
        "base: {} 节点 / {} 边 → head: {} 节点 / {} 边",
        r.stats.base_nodes, r.stats.base_edges, r.stats.head_nodes, r.stats.head_edges
    );
    println!(
        "  + 节点 {} · - 节点 {} · 类型变更 {}",
        r.stats.nodes_added, r.stats.nodes_removed, r.stats.nodes_kind_changed
    );
    println!(
        "  + 边 {} · - 边 {} · 状态变更 {}",
        r.stats.edges_added, r.stats.edges_removed, r.stats.edges_status_changed
    );
    if !r.nodes_added.is_empty() {
        println!("\n新增节点:");
        for n in &r.nodes_added {
            println!(
                "  + {:18} {}  {}",
                n.kind,
                n.label,
                n.path.clone().unwrap_or_default()
            );
        }
    }
    if !r.nodes_removed.is_empty() {
        println!("\n删除节点:");
        for n in &r.nodes_removed {
            println!(
                "  - {:18} {}  {}",
                n.kind,
                n.label,
                n.path.clone().unwrap_or_default()
            );
        }
    }
    if !r.nodes_kind_changed.is_empty() {
        println!("\n节点类型变化:");
        for c in &r.nodes_kind_changed {
            println!("  ! {}  {} → {}", c.id, c.from_kind, c.to_kind);
        }
    }
    if !r.edges_status_changed.is_empty() {
        println!("\n边状态变化:");
        for c in &r.edges_status_changed {
            println!(
                "  ! {kind:18} {id}  {from} → {to}",
                kind = c.kind,
                id = c.id,
                from = c.from_status,
                to = c.to_status
            );
        }
    }
    if r.stats.base_candidates + r.stats.head_candidates > 0
        || !r.candidates_added.is_empty()
        || !r.candidates_removed.is_empty()
        || !r.candidates_status_changed.is_empty()
    {
        println!(
            "\n业务候选 (.groundgraph/candidates/business_logic.yaml): base {} → head {} (+{} / -{} / 状态 {})",
            r.stats.base_candidates,
            r.stats.head_candidates,
            r.stats.candidates_added,
            r.stats.candidates_removed,
            r.stats.candidates_status_changed,
        );
        for c in &r.candidates_added {
            println!("  + {:18} {}  status={}", c.id, c.name, c.status);
        }
        for c in &r.candidates_removed {
            println!("  - {:18} {}  status={}", c.id, c.name, c.status);
        }
        for c in &r.candidates_status_changed {
            println!(
                "  ! {:18} {}  {} → {}",
                c.id, c.name, c.from_status, c.to_status
            );
        }
    }
    println!();
    println!("提示: 把这份报告附在 PR 描述里 — 评审者可以一眼看到 confirmed graph 的真实增量。");
}
