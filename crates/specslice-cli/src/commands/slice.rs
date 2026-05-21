use std::path::Path;

use anyhow::Result;
use specslice_engine::slice::SliceFanoutOptions;
use specslice_engine::{slice_requirement, FeatureSlice, SliceItem, SliceOptions};

pub fn run(repo_root: &Path, requirement: &str, json: bool, call_depth: usize) -> Result<()> {
    let slice = slice_requirement(SliceOptions {
        repo_root: repo_root.to_path_buf(),
        requirement: requirement.to_string(),
        fanout: SliceFanoutOptions { call_depth },
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&slice)?);
    } else {
        print_human(&slice);
    }
    Ok(())
}

fn print_human(slice: &FeatureSlice) {
    println!(
        "Feature Slice: {} {}",
        slice.requirement_id,
        slice.title.clone().unwrap_or_default()
    );
    println!();
    println!("Docs:");
    print_items(&slice.docs);
    println!();
    println!("Declared Implementation:");
    print_items(&slice.implementation);
    println!();
    println!("Linked Tests:");
    print_items(&slice.linked_tests);
    if !slice.code_fanout.is_empty() {
        println!();
        println!("Code Fan-out (calls / references):");
        print_items(&slice.code_fanout);
    }
    if !slice.risks.is_empty() {
        println!();
        println!("Risks:");
        for risk in &slice.risks {
            println!("- {risk}");
        }
    }
}

fn print_items(items: &[SliceItem]) {
    if items.is_empty() {
        println!("- (none)");
        return;
    }
    for item in items {
        let where_ = item
            .path
            .clone()
            .or_else(|| Some(item.id.clone()))
            .unwrap_or_default();
        let label = item.name.clone().unwrap_or_else(|| item.id.clone());
        println!("- {label} ({where_})");
    }
}
