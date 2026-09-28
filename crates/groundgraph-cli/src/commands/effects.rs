use std::path::Path;

use anyhow::{Context, Result};
use groundgraph_engine::effects::{EffectsCard, WritersResult};
use groundgraph_engine::trace::TraceOptions;

pub fn run_effects(root: &Path, query: &str, json: bool) -> Result<()> {
    let card = groundgraph_engine::effects::run_effects(TraceOptions::new(root, query))
        .context("analyzing business effects")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&card)?);
    } else {
        print_card(&card);
    }
    Ok(())
}

fn print_card(card: &EffectsCard) {
    println!("业务动作: {}", card.entry);
    println!("入口: {}", if card.seeds.is_empty() { "未命中".into() } else { card.seeds.join(", ") });
    println!("表操作 ({} 条证据):", card.tables.len());
    for row in card.tables.iter().take(40) {
        let columns = if row.columns.is_empty() { String::new() } else { format!(" [{}]", row.columns.join(", ")) };
        println!("  {} {}{} · {} {:.0}% · {}", row.operation, row.table, columns, row.certainty, row.confidence * 100.0, row.source.as_deref().unwrap_or("来源未知"));
    }
    if card.tables.len() > 40 { println!("  …其余 {} 条表证据，请用 --json 查看", card.tables.len() - 40); }
    println!("外部调用 ({}):", card.external_calls.len());
    for item in card.external_calls.iter().take(20) { println!("  {} {} · {}", item.effect, item.name, item.source.as_deref().unwrap_or("来源未知")); }
    if card.external_calls.len() > 20 { println!("  …其余 {} 个，请用 --json 查看", card.external_calls.len() - 20); }
    println!("事件 ({}):", card.events.len());
    for item in card.events.iter().take(20) { println!("  {} · {}", item.name, item.source.as_deref().unwrap_or("来源未知")); }
    if card.events.len() > 20 { println!("  …其余 {} 个，请用 --json 查看", card.events.len() - 20); }
    println!("事务组 ({}):", card.transactions.groups.len());
    for group in &card.transactions.groups {
        println!("  {} · {} · {} 节点{}", group.owner, group.propagation, group.nodes.len(), group.no_rollback_for.as_ref().map(|v| format!(" · noRollbackFor={v}")).unwrap_or_default());
    }
    println!("风险 ({}):", card.transactions.risks.len());
    for risk in &card.transactions.risks { println!("  {} · {}", risk.kind, risk.at); }
    println!("未解析调用: {}", card.unresolved_count);
    for gap in &card.breakpoints { println!("  {}:{} {} · {}", gap.path, gap.line, gap.reason, gap.expression.replace('\n', " ")); }
    if card.breakpoints_truncated { println!("  …其余断点请用 --json 查看或缩小入口"); }
    println!("已确认效果链占比: {:.1}%", card.confirmed_effect_ratio * 100.0);
    if card.truncated { println!("链路已截断：扩大 trace 深度或节点上限后复核"); }
}

pub fn run_writers(root: &Path, table: &str, json: bool) -> Result<()> {
    let result = groundgraph_engine::effects::run_writers(root, table).context("finding table writers")?;
    if json { println!("{}", serde_json::to_string_pretty(&result)?); } else { print_writers(&result); }
    Ok(())
}

fn print_writers(result: &WritersResult) {
    println!("表: {} · 写入证据 {} · 入口 {}", result.table, result.write_edges, result.entries.len());
    for entry in result.entries.iter().take(100) {
        println!("  {} {} · {} · {}", entry.kind, entry.id, entry.certainty, entry.path.as_deref().unwrap_or("来源未知"));
    }
    if result.entries.len() > 100 { println!("  …其余 {} 个入口，请用 --json 查看", result.entries.len() - 100); }
    if result.truncated { println!("反向链路已截断：结果不完整"); }
}
