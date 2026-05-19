//! Dart lightweight adapter.
//!
//! MVP-2 scope (PRD §2.1 / §3 / implementation plan §MVP-2):
//! - Scan `lib/` and `test/` for `*.dart` files.
//! - Extract file, class, method, function, constructor, import, `test(...)`,
//!   and `group(...)`.
//! - Output a [`LanguageIndexBatch`]; the engine handles SQLite ingestion.

pub mod parser;
pub mod references;

pub use parser::{parse_dart, ParseResult};
pub use references::{compute_references, FileSource};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use specslice_core::language_batch::LanguageIndexBatch;
use specslice_core::ArtifactId;

pub const DART_LANGUAGE_ID: &str = "dart";

/// Walk `repo_root/<code_root>/**/*.dart` and produce a merged batch. After
/// every file has been parsed, run the body-level reference scanner so
/// graph consumers see `calls` / `references` edges in addition to the
/// structural `contains` / `imports` edges.
pub fn index_dart_paths(repo_root: &Path, code_roots: &[PathBuf]) -> Result<LanguageIndexBatch> {
    let mut batch = LanguageIndexBatch {
        language: DART_LANGUAGE_ID.into(),
        ..Default::default()
    };
    let mut visited = Vec::new();
    let mut sources: Vec<FileSource> = Vec::new();
    let mut field_types: BTreeMap<ArtifactId, BTreeMap<String, String>> = BTreeMap::new();
    for code_root in code_roots {
        let abs_root = repo_root.join(code_root);
        if !abs_root.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&abs_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("dart") {
                continue;
            }
            let rel = path
                .strip_prefix(repo_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if visited.iter().any(|v| v == &rel) {
                continue;
            }
            visited.push(rel.clone());

            let source = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
            let parsed = parse_dart(&rel, &source, &hash);
            merge(&mut batch, &mut field_types, parsed);
            sources.push(FileSource { path: rel, source });
        }
    }
    batch.references =
        compute_references(&sources, &batch.symbols, &batch.symbol_ranges, &field_types);
    Ok(batch)
}

fn merge(
    into: &mut LanguageIndexBatch,
    field_types: &mut BTreeMap<ArtifactId, BTreeMap<String, String>>,
    mut from: ParseResult,
) {
    into.files.push(from.file);
    into.symbols.append(&mut from.symbols);
    into.tests.append(&mut from.tests);
    into.imports.append(&mut from.imports);
    into.symbol_ranges.append(&mut from.ranges);
    into.diagnostics.append(&mut from.diagnostics);
    for (class_id, fields) in from.field_types {
        field_types.entry(class_id).or_default().extend(fields);
    }
}
