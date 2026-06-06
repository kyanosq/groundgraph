//! `specslice contract` — data contract view (P24 gap #3).
//!
//! ```text
//! specslice contract               # SQL CREATE TABLE schemas + JSON keymaps
//! specslice contract --tables-only # only persistence schema
//! specslice contract --keys-only   # only serialization keymap
//! specslice contract --json        # machine-readable for an agent
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_engine::data_contract::{
    analyze_data_contract, DataContractOptions, DataContractReport,
};

#[derive(Debug, Clone)]
pub struct ContractRunArgs {
    pub repo_root: PathBuf,
    pub tables_only: bool,
    pub keys_only: bool,
    pub json: bool,
}

pub fn run(args: ContractRunArgs) -> Result<()> {
    let report = analyze_data_contract(DataContractOptions {
        repo_root: args.repo_root,
        tables_only: args.tables_only,
        keys_only: args.keys_only,
        max_sites_per_key: 25,
    })
    .context("抽取数据契约")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("序列化数据契约")?
        );
    } else {
        print_human(&report);
    }
    Ok(())
}

fn print_human(report: &DataContractReport) {
    println!("SpecSlice 数据契约 (schema v{})", report.schema_version);
    println!(
        "扫描文件 {} · 表 {} · JSON 键(去重) {} · 键引用 {}",
        report.stats.files_scanned,
        report.stats.tables,
        report.stats.json_keys_distinct,
        report.stats.json_key_refs,
    );

    if !report.tables.is_empty() {
        println!();
        println!("== 持久化 schema (CREATE TABLE) ==");
        for t in &report.tables {
            println!("表 {}  ({}:{})", t.name, t.path, t.line);
            for c in &t.columns {
                if c.definition.is_empty() {
                    println!("    - {}", c.name);
                } else {
                    println!("    - {}  {}", c.name, c.definition);
                }
            }
        }
    }

    if !report.json_keys.is_empty() {
        println!();
        println!("== 序列化键 (obj['key'] / ?? default) ==");
        for k in &report.json_keys {
            let def = if k.defaults.is_empty() {
                String::new()
            } else {
                format!("  默认: {}", k.defaults.join(" | "))
            };
            println!("{}  ×{}{}", k.key, k.occurrences, def);
            for s in k.sites.iter().take(3) {
                println!("    {}:{}", s.path, s.line);
            }
            if k.sites.len() > 3 {
                println!("    ... 还有 {} 处", k.sites.len() - 3);
            }
        }
    }

    if report.tables.is_empty() && report.json_keys.is_empty() {
        println!();
        println!("(未发现 CREATE TABLE 或 obj['key'] 形式的契约)");
    }
}
