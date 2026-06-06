//! `specslice feature-pack` — one-click feature slice export (P24 gap #7).
//!
//! Bundles everything an agent needs to re-implement one feature into a single
//! self-contained document: in-scope symbols + behavioural facts, the edges
//! among them (and external callees), constants, the data contract, and test
//! suggestions — all filtered to the selected scope.
//!
//! ```text
//! specslice feature-pack --path lib/alarm           # everything under a dir
//! specslice feature-pack --requirement REQ-ALARM    # files a requirement touches
//! specslice feature-pack --path lib/alarm --format text   # human summary
//! ```

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use specslice_engine::feature_pack::{
    build_feature_pack, FeaturePack, FeaturePackOptions, FeaturePackSelector,
};

#[derive(Debug, Clone)]
pub struct FeaturePackRunArgs {
    pub repo_root: PathBuf,
    pub path: Option<String>,
    pub requirement: Option<String>,
    pub max_evidence: usize,
    pub text: bool,
}

pub fn run(args: FeaturePackRunArgs) -> Result<()> {
    let selector = match (args.path, args.requirement) {
        (Some(p), None) => FeaturePackSelector::Path(p),
        (None, Some(r)) => FeaturePackSelector::Requirement(r),
        (Some(_), Some(_)) => bail!("--path 与 --requirement 二选一，不能同时给出"),
        (None, None) => bail!("必须给出 --path <前缀> 或 --requirement <需求ID>"),
    };
    let pack = build_feature_pack(FeaturePackOptions {
        repo_root: args.repo_root,
        selector,
        max_evidence_per_symbol: args.max_evidence,
    })
    .context("构建特性切片包")?;
    if args.text {
        print_human(&pack);
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&pack).context("序列化特性切片包")?
        );
    }
    Ok(())
}

fn print_human(pack: &FeaturePack) {
    println!("SpecSlice 特性切片包 (schema v{})", pack.schema_version);
    println!("焦点: {}", pack.focus);
    let s = &pack.stats;
    println!(
        "文件 {} · 符号 {} (纯 {} / 杂 {}) · 内部边 {} · 外部依赖边 {}",
        s.files, s.symbols, s.pure_symbols, s.impure_symbols, s.edges_internal, s.edges_external,
    );
    println!(
        "常量 {} · 表 {} · JSON 键 {} · 测试建议 {}",
        s.constants, s.tables, s.json_keys, s.test_suggestions,
    );
    println!();

    println!("文件:");
    for f in &pack.files {
        println!("  - {f}");
    }
    println!();

    println!("符号 (行为事实):");
    for sym in &pack.symbols {
        let loc = match sym.line_range {
            Some((a, b)) => format!(":{a}-{b}"),
            None => String::new(),
        };
        let name = sym.name.clone().unwrap_or_else(|| sym.id.clone());
        println!(
            "  - {name} [{}] 分支{} 比较{} 空值{} 抛出{} 循环{} await{}",
            purity_str(sym.purity),
            sym.counts.branches,
            sym.counts.comparisons,
            sym.counts.null_checks,
            sym.counts.throws,
            sym.counts.loops,
            sym.counts.awaits,
        );
        if !loc.is_empty() {
            if let Some(p) = &sym.path {
                println!("      {p}{loc}");
            }
        }
    }
    println!();

    if !pack.edges.is_empty() {
        println!("依赖边 (外部依赖以 → 标注):");
        for e in &pack.edges {
            let marker = if e.in_scope { "  " } else { "→ " };
            println!("  {marker}{} --{}-> {}", e.from, e.kind, e.to);
        }
        println!();
    }

    if !pack.constants.is_empty() {
        println!("常量 (按出现次数):");
        for c in pack.constants.iter().take(40) {
            println!("  - {} ({}) ×{}", c.value, c.kind.as_str(), c.occurrences);
        }
        println!();
    }

    if !pack.tables.is_empty() {
        println!("持久化表:");
        for t in &pack.tables {
            let cols: Vec<String> = t.columns.iter().map(|c| c.name.clone()).collect();
            println!("  - {} ({})", t.name, cols.join(", "));
        }
        println!();
    }

    if !pack.json_keys.is_empty() {
        println!("JSON 键:");
        for k in pack.json_keys.iter().take(40) {
            let def = if k.defaults.is_empty() {
                String::new()
            } else {
                format!("  默认 {}", k.defaults.join(" | "))
            };
            println!("  - {} ×{}{}", k.key, k.occurrences, def);
        }
        println!();
    }

    if !pack.test_suggestions.is_empty() {
        println!("测试建议 (优先级降序):");
        for item in &pack.test_suggestions {
            let name = item.name.clone().unwrap_or_else(|| item.id.clone());
            println!("  - {name} [优先级 {}]", item.priority);
            for sug in &item.suggestions {
                println!("      [{}] {}", sug.kind.as_str(), sug.message);
            }
        }
    }
}

fn purity_str(p: specslice_engine::symbol_facts::Purity) -> &'static str {
    use specslice_engine::symbol_facts::Purity;
    match p {
        Purity::Pure => "pure",
        Purity::Impure => "impure",
        Purity::Unknown => "unknown",
    }
}
