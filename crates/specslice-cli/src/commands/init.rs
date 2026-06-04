use std::path::Path;

use anyhow::Result;
use specslice_engine::{init_repository, InitOptions, InitOutcome};

pub fn run(repo_root: &Path) -> Result<()> {
    let outcome = init_repository(InitOptions::new(repo_root.to_path_buf()))?;
    print_outcome(repo_root, &outcome);
    Ok(())
}

fn print_outcome(repo_root: &Path, outcome: &InitOutcome) {
    let config_label = display_relative(repo_root, &outcome.config_path);
    let links_label = display_relative(repo_root, &outcome.links_path);
    let db_label = display_relative(repo_root, &outcome.graph_db_path);
    let requirements_label = display_relative(repo_root, &outcome.requirements_dir);

    let config_action = if outcome.config_already_existed {
        "kept"
    } else {
        "created"
    };
    let db_action = if outcome.graph_db_already_existed {
        "kept"
    } else {
        "created"
    };
    let links_action = if outcome.links_already_existed {
        "kept"
    } else {
        "created"
    };
    let requirements_action = if outcome.requirements_already_existed {
        "kept"
    } else {
        "created"
    };

    println!("SpecSlice workspace ready.");
    println!("  {config_action}: {config_label}");
    println!("  {links_action}: {links_label}");
    println!("  {requirements_action}: {requirements_label}");
    println!("  {db_action}: {db_label}");
}

fn display_relative(repo_root: &Path, target: &Path) -> String {
    target
        .strip_prefix(repo_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| target.to_string_lossy().into_owned())
}
