use std::path::Path;

use anyhow::Result;
use groundgraph_engine::{run_checks, CheckOptions, CheckReport, CheckSeverity};

pub fn run(repo_root: &Path, json: bool, fail_on_warning: bool) -> Result<i32> {
    let report = run_checks(CheckOptions {
        repo_root: repo_root.to_path_buf(),
        impact: None,
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    let mut exit = 0;
    if report.has_errors() {
        // #233 — check findings are a user-correctable data problem (broken
        // links / missing tests), not an internal failure: exit 2.
        exit = i32::from(crate::exit_code::EXIT_USER_ERROR);
    }
    if fail_on_warning && report.warnings() > 0 {
        exit = i32::from(crate::exit_code::EXIT_USER_ERROR);
    }
    Ok(exit)
}

fn print_human(report: &CheckReport) {
    if report.findings.is_empty() {
        println!("GroundGraph Checks: 0 findings.");
        return;
    }
    println!(
        "GroundGraph Checks: {} error(s), {} warning(s).",
        report.errors(),
        report.warnings()
    );
    for f in &report.findings {
        let icon = match f.severity {
            CheckSeverity::Error => "ERROR",
            CheckSeverity::Warning => "WARN",
            CheckSeverity::Info => "INFO",
        };
        println!("[{icon}] {}: {}", f.code, f.message);
    }
}
