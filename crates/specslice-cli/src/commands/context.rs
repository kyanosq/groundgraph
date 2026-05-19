use std::path::Path;

use anyhow::Result;
use specslice_engine::{build_context, ContextOptions};

pub fn run(repo_root: &Path, requirement: &str, include_snippets: bool, json: bool) -> Result<()> {
    let pack = build_context(ContextOptions {
        repo_root: repo_root.to_path_buf(),
        requirement: requirement.to_string(),
        include_snippets,
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&pack)?);
    } else {
        print_human(&pack);
    }
    Ok(())
}

fn print_human(pack: &specslice_engine::ContextPack) {
    println!(
        "Context Pack: {} {}",
        pack.requirement_id,
        pack.title.clone().unwrap_or_default()
    );
    println!();
    println!("Docs:");
    for d in &pack.slice.docs {
        println!("- {}", d.path.clone().unwrap_or_else(|| d.id.clone()));
    }
    println!();
    println!("Implementation:");
    for d in &pack.slice.implementation {
        println!("- {}", d.path.clone().unwrap_or_else(|| d.id.clone()));
    }
    println!();
    println!("Linked tests:");
    for d in &pack.slice.linked_tests {
        println!("- {}", d.path.clone().unwrap_or_else(|| d.id.clone()));
    }
    if !pack.files_to_read.is_empty() {
        println!();
        println!("Files to read:");
        for f in &pack.files_to_read {
            println!("- {f}");
        }
    }
    if !pack.tests_to_run.is_empty() {
        println!();
        println!("Tests to run:");
        for t in &pack.tests_to_run {
            println!("- {t}");
        }
    }
    if !pack.docs_snippets.is_empty()
        || !pack.impl_snippets.is_empty()
        || !pack.test_snippets.is_empty()
    {
        println!();
        println!(
            "Snippets included: docs={}, impl={}, test={}",
            pack.docs_snippets.len(),
            pack.impl_snippets.len(),
            pack.test_snippets.len(),
        );
    }
}
