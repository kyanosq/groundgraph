//! Bridge between the Dart lightweight adapter and the SpecSlice store.
//!
//! MVP-2 scope (PRD §3.1 / implementation plan §MVP-2):
//! - Walk `lib/` and `test/` via [`specslice_lang_dart::index_dart_paths`].
//! - Ingest the [`LanguageIndexBatch`] into the store:
//!   - File / class / method / function / constructor / test-case nodes.
//!   - `File --contains--> Symbol` (Fact) edges and Class -> Method `contains` edges.
//!   - `File --imports--> File` (Fact) edges when the target file resolves locally.
//!   - `Symbol/Test --declaresImplementation/declaresVerification--> Requirement`
//!     (Declared) edges for `@implements` / `@verifies` tags.
//!   - Symbol ranges and parent-child hierarchy.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::artifact_id::requirement_id;
use specslice_core::language_batch::{LanguageIndexBatch, TraceTag};
use specslice_core::{EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use specslice_lang_dart::index_dart_paths;
use specslice_store::Store;

pub const DART_INDEXER_NAME: &str = "dart_lightweight";

#[derive(Debug, Clone)]
pub struct DartIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DartIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub declared_implementations: usize,
    pub declared_verifications: usize,
}

pub fn index_dart(store: &mut Store, options: &DartIndexOptions) -> Result<DartIndexResult> {
    let batch = index_dart_paths(&options.repo_root, &options.code_roots)
        .context("scanning Dart sources")?;
    ingest(store, &batch)
}

fn ingest(store: &mut Store, batch: &LanguageIndexBatch) -> Result<DartIndexResult> {
    let mut result = DartIndexResult {
        files: batch.files.len(),
        ..Default::default()
    };

    for file in &batch.files {
        let mut node = Node::new(file.id.clone(), NodeKind::File);
        node.path = Some(file.path.clone());
        node.name = std::path::Path::new(&file.path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned());
        node.content_hash = Some(file.content_hash.clone());
        node.indexer = Some(DART_INDEXER_NAME.into());
        store.upsert_node(&node)?;
    }

    for symbol in &batch.symbols {
        let mut node = Node::new(symbol.id.clone(), symbol.kind);
        node.path = Some(symbol.path.clone());
        node.name = Some(symbol.name.clone());
        node.stable_key = Some(symbol.qualified_name.clone());
        node.start_line = Some(symbol.start_line);
        node.end_line = Some(symbol.end_line);
        node.indexer = Some(DART_INDEXER_NAME.into());
        store.upsert_node(&node)?;
        result.symbols += 1;

        let mut contains = if let Some(parent) = &symbol.parent_symbol_id {
            EdgeAssertion::fact(
                parent.clone(),
                symbol.id.clone(),
                EdgeKind::Contains,
                EdgeSource::LanguageAdapter,
            )
        } else {
            EdgeAssertion::fact(
                specslice_core::artifact_id::file_id(&symbol.path),
                symbol.id.clone(),
                EdgeKind::Contains,
                EdgeSource::LanguageAdapter,
            )
        };
        contains.indexer = Some(DART_INDEXER_NAME.into());
        store.upsert_edge(&contains)?;
    }

    for test in &batch.tests {
        let mut node = Node::new(test.id.clone(), test.kind);
        node.path = Some(test.path.clone());
        node.name = Some(test.name.clone());
        node.start_line = Some(test.start_line);
        node.end_line = Some(test.end_line);
        node.indexer = Some(DART_INDEXER_NAME.into());
        store.upsert_node(&node)?;
        if test.kind == NodeKind::TestCase {
            result.tests += 1;
        }
        let parent_id = test
            .parent_symbol_id
            .clone()
            .unwrap_or_else(|| specslice_core::artifact_id::file_id(&test.path));
        let mut contains = EdgeAssertion::fact(
            parent_id,
            test.id.clone(),
            EdgeKind::Contains,
            EdgeSource::LanguageAdapter,
        );
        contains.indexer = Some(DART_INDEXER_NAME.into());
        store.upsert_edge(&contains)?;
    }

    for import in &batch.imports {
        let mut edge = EdgeAssertion::fact(
            import.from_file.clone(),
            specslice_core::artifact_id::file_id(&import.to_path),
            EdgeKind::Imports,
            EdgeSource::LanguageAdapter,
        );
        edge.indexer = Some(DART_INDEXER_NAME.into());
        store.upsert_edge(&edge)?;
    }

    for trace in &batch.trace_links {
        let (edge_kind, increment_field) = match trace.tag {
            TraceTag::Implements => (EdgeKind::DeclaresImplementation, true),
            TraceTag::Verifies => (EdgeKind::DeclaresVerification, false),
            TraceTag::Related => (EdgeKind::RelatedTo, false),
        };
        let mut edge = EdgeAssertion::declared(
            trace.from_symbol_id.clone(),
            requirement_id(&trace.target),
            edge_kind,
            EdgeSource::ExplicitTrace,
        );
        edge.indexer = Some(DART_INDEXER_NAME.into());
        store.upsert_edge(&edge)?;
        match trace.tag {
            TraceTag::Implements => {
                if increment_field {
                    result.declared_implementations += 1;
                }
            }
            TraceTag::Verifies => result.declared_verifications += 1,
            TraceTag::Related => {}
        }
    }

    for range in &batch.symbol_ranges {
        store.upsert_symbol_range(range)?;
    }
    for symbol in &batch.symbols {
        store.upsert_symbol_range(&specslice_core::SymbolRange {
            file_path: symbol.path.clone(),
            symbol_id: symbol.id.clone(),
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            symbol_kind: symbol.kind,
            qualified_name: symbol.qualified_name.clone(),
            parent_symbol_id: symbol.parent_symbol_id.clone(),
        })?;
    }
    for test in &batch.tests {
        store.upsert_symbol_range(&specslice_core::SymbolRange {
            file_path: test.path.clone(),
            symbol_id: test.id.clone(),
            start_line: test.start_line,
            end_line: test.end_line,
            symbol_kind: test.kind,
            qualified_name: test.name.clone(),
            parent_symbol_id: test.parent_symbol_id.clone(),
        })?;
    }

    Ok(result)
}
