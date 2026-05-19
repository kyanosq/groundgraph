use std::path::Path;

use anyhow::Result;
use specslice_engine::{export, ExportFormat, ExportOptions, ExportOutcome};

pub fn run(repo_root: &Path, format: ExportFormat) -> Result<()> {
    let outcome = export(ExportOptions {
        repo_root: repo_root.to_path_buf(),
        format,
    })?;
    print_outcome(repo_root, &outcome);
    Ok(())
}

fn print_outcome(repo_root: &Path, outcome: &ExportOutcome) {
    let bundle = outcome
        .bundle_dir
        .strip_prefix(repo_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| outcome.bundle_dir.to_string_lossy().into_owned());
    println!("SpecSlice export written to {bundle}");
    for file in &outcome.files {
        let rel = file
            .strip_prefix(repo_root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| file.to_string_lossy().into_owned());
        println!("  - {rel}");
    }
}
