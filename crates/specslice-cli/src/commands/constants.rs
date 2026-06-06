//! `specslice constants` — constants & literals catalogue (P24 gap #2).
//!
//! ```text
//! specslice constants                       # magic ints/floats/strings, most-repeated first
//! specslice constants --kind str            # only string literals
//! specslice constants --min-occurrences 3   # values that repeat ≥3 times
//! specslice constants --json                # machine-readable for an agent
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_engine::constants::{
    analyze_constants, ConstantsOptions, ConstantsReport, LiteralKind,
};

#[derive(Debug, Clone)]
pub struct ConstantsRunArgs {
    pub repo_root: PathBuf,
    pub include_types: bool,
    pub include_trivial: bool,
    pub min_occurrences: usize,
    pub kind: Option<LiteralKind>,
    pub max: usize,
    pub json: bool,
}

pub fn parse_kind(s: &str) -> Result<LiteralKind> {
    match s.to_ascii_lowercase().as_str() {
        "int" => Ok(LiteralKind::Int),
        "float" => Ok(LiteralKind::Float),
        "str" | "string" => Ok(LiteralKind::Str),
        "bool" => Ok(LiteralKind::Bool),
        "char" => Ok(LiteralKind::Char),
        other => anyhow::bail!("未知字面量类型 `{other}`（可选 int / float / str / bool / char）"),
    }
}

pub fn run(args: ConstantsRunArgs) -> Result<()> {
    let report = analyze_constants(ConstantsOptions {
        repo_root: args.repo_root,
        include_types: args.include_types,
        include_trivial: args.include_trivial,
        min_occurrences: args.min_occurrences,
        kind_filter: args.kind,
        max_entries: args.max,
        max_sites_per_entry: 25,
    })
    .context("抽取常量 / 字面量目录")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("序列化常量目录")?
        );
    } else {
        print_human(&report);
    }
    Ok(())
}

fn print_human(report: &ConstantsReport) {
    println!("SpecSlice 常量 / 字面量目录 (schema v{})", report.schema_version);
    println!(
        "分析 {} · 有源码 {} · 字面量 {} · 去重值 {} · 输出 {}{}",
        report.stats.analyzed,
        report.stats.with_source,
        report.stats.total_literals,
        report.stats.distinct_values,
        report.stats.returned,
        if report.stats.truncated {
            "（已截断）"
        } else {
            ""
        },
    );
    println!();
    if report.entries.is_empty() {
        println!("(没有符合条件的字面量)");
        return;
    }
    for e in &report.entries {
        println!("{}  ({})  ×{}", e.value, e.kind.as_str(), e.occurrences);
        for s in e.sites.iter().take(5) {
            let loc = match (&s.path, s.line) {
                (Some(p), l) => format!("{p}:{l}"),
                (None, l) => format!("?:{l}"),
            };
            let name = s.symbol_name.clone().unwrap_or_else(|| s.symbol_id.clone());
            println!("    {loc}  {name}");
        }
        if e.sites.len() > 5 {
            println!("    ... 还有 {} 处", e.sites.len() - 5);
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kind_accepts_aliases() {
        assert_eq!(parse_kind("int").unwrap(), LiteralKind::Int);
        assert_eq!(parse_kind("STRING").unwrap(), LiteralKind::Str);
        assert_eq!(parse_kind("str").unwrap(), LiteralKind::Str);
        assert!(parse_kind("blob").is_err());
    }
}
