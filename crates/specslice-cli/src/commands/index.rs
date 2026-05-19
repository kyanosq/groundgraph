use std::path::Path;

use anyhow::Result;
use specslice_engine::{index_repository, IndexOptions, IndexResult};

pub fn run(repo_root: &Path, docs_only: bool) -> Result<()> {
    let options = if docs_only {
        IndexOptions::docs_only(repo_root.to_path_buf())
    } else {
        IndexOptions::all(repo_root.to_path_buf())
    };
    let result = index_repository(options)?;
    print_result(&result);
    Ok(())
}

fn print_result(result: &IndexResult) {
    if let Some(docs) = &result.docs {
        println!("Docs index:");
        println!("  Files: {}", docs.files);
        println!("  Requirements: {}", docs.requirements);
        println!("  DocSections: {}", docs.doc_sections);
        println!("  Edges: {}", docs.edges);
        if !docs.unresolved_references.is_empty() {
            println!(
                "  Unresolved references: {}",
                docs.unresolved_references.len()
            );
        }
    }
    if let Some(code) = &result.code {
        println!("Code index:");
        println!("  Dart files: {}", code.files);
        println!("  Symbols: {}", code.symbols);
        println!("  TestCases: {}", code.tests);
        println!(
            "  Declared implementations: {}",
            code.declared_implementations
        );
        println!("  Declared verifications: {}", code.declared_verifications);
    }
}
