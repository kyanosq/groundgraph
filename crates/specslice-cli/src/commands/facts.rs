//! `specslice facts` — behavioural fact extraction (P24 gap #1) and
//! `specslice purity` — node purity classification (P24 gap #6).
//!
//! ```text
//! specslice facts                      # branches/returns/compares + evidence lines
//! specslice facts --purity pure        # only pure callables (great seeds for tests)
//! specslice facts --json               # machine-readable for an agent
//! specslice purity                     # pure / impure / unknown census + reasons
//! ```
//!
//! Both render the same engine report; `purity` is a compact census view.

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_engine::symbol_facts::{
    analyze_symbol_facts, Purity, SymbolFact, SymbolFactsOptions, SymbolFactsReport,
};

#[derive(Debug, Clone)]
pub struct FactsRunArgs {
    pub repo_root: PathBuf,
    pub include_types: bool,
    pub purity: Option<Purity>,
    pub max: usize,
    pub max_evidence: usize,
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct PurityRunArgs {
    pub repo_root: PathBuf,
    pub include_types: bool,
    pub only: Option<Purity>,
    pub json: bool,
}

pub fn parse_purity(s: &str) -> Result<Purity> {
    match s.to_ascii_lowercase().as_str() {
        "pure" => Ok(Purity::Pure),
        "impure" => Ok(Purity::Impure),
        "unknown" => Ok(Purity::Unknown),
        other => anyhow::bail!("未知 purity 取值 `{other}`（可选 pure / impure / unknown）"),
    }
}

pub fn run_facts(args: FactsRunArgs) -> Result<()> {
    let report = analyze_symbol_facts(SymbolFactsOptions {
        repo_root: args.repo_root,
        include_types: args.include_types,
        purity_filter: args.purity,
        max_symbols: args.max,
        max_evidence_per_symbol: args.max_evidence,
    })
    .context("抽取行为事实")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("序列化行为事实报告")?
        );
    } else {
        print_facts_human(&report);
    }
    Ok(())
}

pub fn run_purity(args: PurityRunArgs) -> Result<()> {
    let report = analyze_symbol_facts(SymbolFactsOptions {
        repo_root: args.repo_root,
        include_types: args.include_types,
        purity_filter: args.only,
        max_symbols: 0,
        max_evidence_per_symbol: 0,
    })
    .context("计算节点纯度")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("序列化纯度报告")?
        );
    } else {
        print_purity_human(&report);
    }
    Ok(())
}

fn print_facts_human(report: &SymbolFactsReport) {
    println!("SpecSlice 行为事实 (schema v{})", report.schema_version);
    println!(
        "分析 {} · 有源码 {} · 纯 {} · 有副作用 {} · 未知 {} · 输出 {}{}",
        report.stats.analyzed,
        report.stats.with_source,
        report.stats.pure,
        report.stats.impure,
        report.stats.unknown,
        report.stats.returned,
        if report.stats.truncated {
            "（已截断）"
        } else {
            ""
        },
    );
    println!();
    if report.facts.is_empty() {
        println!("(没有符合条件的符号)");
        return;
    }
    for f in &report.facts {
        print_fact(f);
    }
}

fn print_fact(f: &SymbolFact) {
    let loc = match (&f.path, f.line_range) {
        (Some(p), Some((s, e))) => format!("{p}:{s}-{e}"),
        (Some(p), None) => p.clone(),
        _ => String::new(),
    };
    let name = f.name.clone().unwrap_or_else(|| f.id.clone());
    println!("- {name}  ({})  [{}]", f.kind, f.purity.as_str());
    if !loc.is_empty() {
        println!("    路径: {loc}");
    }
    let c = &f.counts;
    println!(
        "    计数: 分支 {} · 循环 {} · return {} · 比较 {} · 空值 {} · 抛出 {} · await {}",
        c.branches, c.loops, c.early_returns, c.comparisons, c.null_checks, c.throws, c.awaits,
    );
    if !f.impurity_signals.is_empty() {
        println!("    副作用: {}", f.impurity_signals.join(", "));
    }
    if !f.evidence.is_empty() {
        println!("    证据行:");
        for e in &f.evidence {
            println!("      L{} [{}] {}", e.line, e.tags.join(","), e.text);
        }
    }
    println!();
}

fn print_purity_human(report: &SymbolFactsReport) {
    println!("SpecSlice 纯度普查 (schema v{})", report.schema_version);
    println!(
        "分析 {} · 纯 {} · 有副作用 {} · 未知 {}",
        report.stats.analyzed, report.stats.pure, report.stats.impure, report.stats.unknown,
    );
    println!();
    if report.facts.is_empty() {
        println!("(没有符合条件的符号)");
        return;
    }
    for f in &report.facts {
        let loc = match (&f.path, f.line_range) {
            (Some(p), Some((s, _))) => format!("{p}:{s}"),
            (Some(p), None) => p.clone(),
            _ => String::new(),
        };
        let name = f.name.clone().unwrap_or_else(|| f.id.clone());
        let signals = if f.impurity_signals.is_empty() {
            String::new()
        } else {
            format!("  <{}>", f.impurity_signals.join(","))
        };
        println!("[{}] {name}  {loc}{signals}", f.purity.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_purity_accepts_known_values() {
        assert_eq!(parse_purity("pure").unwrap(), Purity::Pure);
        assert_eq!(parse_purity("IMPURE").unwrap(), Purity::Impure);
        assert_eq!(parse_purity("unknown").unwrap(), Purity::Unknown);
        assert!(parse_purity("nope").is_err());
    }
}
