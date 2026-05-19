use std::path::Path;

use anyhow::Result;
use specslice_engine::{run_impact, ImpactOptions, ImpactReport};

pub fn run(repo_root: &Path, base: &str, head: &str, json: bool) -> Result<()> {
    let report = run_impact(ImpactOptions {
        repo_root: repo_root.to_path_buf(),
        base_ref: base.to_string(),
        head_ref: head.to_string(),
        reindex: true,
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    Ok(())
}

fn print_human(report: &ImpactReport) {
    println!("SpecSlice Impact Report");
    println!();
    println!("Changed files:");
    if report.changed_files.is_empty() {
        println!("- (none)");
    } else {
        for f in &report.changed_files {
            println!("- {f}");
        }
    }
    println!();
    println!("Changed symbols:");
    if report.changed_symbols.is_empty() {
        println!("- (none)");
    } else {
        for s in &report.changed_symbols {
            println!(
                "- {} ({})",
                s.name.clone().unwrap_or_else(|| s.id.clone()),
                s.path.clone().unwrap_or_else(|| s.id.clone())
            );
        }
    }
    if !report.changed_doc_sections.is_empty() {
        println!();
        println!("Changed doc sections:");
        for d in &report.changed_doc_sections {
            println!(
                "- {} ({})",
                d.name.clone().unwrap_or_else(|| d.id.clone()),
                d.path.clone().unwrap_or_default()
            );
        }
    }
    println!();
    println!("Affected requirements:");
    if report.affected_requirements.is_empty() {
        println!("- (none)");
    } else {
        for r in &report.affected_requirements {
            println!("- {} {}", r.id, r.name.clone().unwrap_or_default());
        }
    }
    if !report.linked_implementations.is_empty() {
        println!();
        println!("Linked implementation:");
        for i in &report.linked_implementations {
            println!(
                "- {} ({})",
                i.name.clone().unwrap_or_else(|| i.id.clone()),
                i.path.clone().unwrap_or_else(|| i.id.clone())
            );
        }
    }
    if !report.linked_tests.is_empty() {
        println!();
        println!("Linked tests:");
        for t in &report.linked_tests {
            println!("- {}", t.path.clone().unwrap_or_else(|| t.id.clone()));
        }
    }
    if !report.warnings.is_empty() {
        println!();
        println!("Warnings:");
        for w in &report.warnings {
            println!("- {w}");
        }
    }
    if !report.info.is_empty() {
        println!();
        println!("Info:");
        for i in &report.info {
            println!("- {i}");
        }
    }
}
