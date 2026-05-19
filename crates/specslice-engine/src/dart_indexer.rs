//! Bridge between the Dart lightweight adapter and the SpecSlice store.
//!
//! MVP-2 scope (PRD §3.1 / implementation plan §MVP-2):
//! - Walk `lib/` and `test/` via [`specslice_lang_dart::index_dart_paths`].
//! - Ingest the [`LanguageIndexBatch`] into the store:
//!   - File / class / method / function / constructor / test-case nodes.
//!   - `File --contains--> Symbol` (Fact) edges and Class -> Method `contains` edges.
//!   - `File --imports--> File` (Fact) edges when the target file resolves locally.
//!   - Symbol ranges and parent-child hierarchy.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::language_batch::LanguageIndexBatch;
use specslice_core::{EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use specslice_lang_dart::index_dart_paths;
use specslice_store::Store;

pub const DART_INDEXER_NAME: &str = "dart_lightweight";

#[derive(Debug, Clone, Default)]
pub struct DartIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    /// Optional `glob`-style patterns to filter out paths that are technically
    /// inside a `code_roots` entry but should be skipped (generated files,
    /// build output, etc.). Patterns use the simple matcher in
    /// [`crate::dart_indexer::path_matches_glob`] — only `**`, `*`, `?`, `.`
    /// and `/` are honoured.
    pub exclude_globs: Vec<String>,
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
    let mut batch = index_dart_paths(&options.repo_root, &options.code_roots)
        .context("scanning Dart sources")?;
    if !options.exclude_globs.is_empty() {
        let exclude = options.exclude_globs.clone();
        let drop_file = |path: &str| exclude.iter().any(|g| path_matches_glob(g, path));
        batch.files.retain(|f| !drop_file(&f.path));
        batch.symbols.retain(|s| !drop_file(&s.path));
        batch.tests.retain(|t| !drop_file(&t.path));
        batch.imports.retain(|i| {
            // Imports are keyed by the *file id* they came from, not by path,
            // so we conservatively keep them all. They will become dangling
            // for excluded files but Impact already handles missing nodes.
            let _ = i;
            true
        });
        batch.symbol_ranges.retain(|r| !drop_file(&r.file_path));
    }
    ingest(store, &batch)
}

/// Minimal glob matcher. Recognises `**` (cross-directory wildcard) and `*`
/// (within a single segment). Implemented locally so we do not have to pull
/// in a heavier dependency for what is essentially `path.ends_with(".g.dart")`.
fn path_matches_glob(pattern: &str, path: &str) -> bool {
    let pat = pattern.replace('\\', "/");
    let p = path.replace('\\', "/");
    let mut pi = pat.as_bytes();
    let si = p.as_bytes();

    // Drop leading `./`.
    if pi.starts_with(b"./") {
        pi = &pi[2..];
    }

    if let Some(rest) = pi.strip_prefix(b"**/") {
        let suffix = std::str::from_utf8(rest).unwrap_or("");
        return path_contains_pattern_segment(suffix, std::str::from_utf8(si).unwrap_or(""));
    }
    if pi == b"**" {
        return true;
    }

    // Direct prefix match (e.g. `.dart_tool`, `build`, `generated`).
    let pat_str = std::str::from_utf8(pi).unwrap_or("");
    let path_str = std::str::from_utf8(si).unwrap_or("");
    if !pat_str.contains('*') && !pat_str.contains('?') {
        return path_str == pat_str
            || path_str.starts_with(&format!("{pat_str}/"))
            || path_str.contains(&format!("/{pat_str}/"));
    }

    path_contains_pattern_segment(pat_str, path_str)
}

fn path_contains_pattern_segment(pattern: &str, path: &str) -> bool {
    // We only handle one wildcard tail of the form `*.ext` or literal suffix.
    if let Some(stripped) = pattern.strip_prefix("*.") {
        let needle = format!(".{stripped}");
        return path.ends_with(&needle);
    }
    if !pattern.contains('*') {
        return path == pattern || path.ends_with(&format!("/{pattern}"));
    }
    // Fallback: treat `*` as ".*" and do an end-anchored check.
    let regex_like = pattern.replace('.', "\\.").replace('*', ".*");
    let re = format!("(?:^|/){regex_like}$");
    matches_regex_like(&re, path)
}

fn matches_regex_like(escaped: &str, path: &str) -> bool {
    // Cheap regex emulation: split on `.*` and require each literal piece
    // in order, anchored as documented.
    let pieces: Vec<&str> = escaped.splitn(2, ".*").collect();
    match pieces.as_slice() {
        [single] => path.ends_with(*single),
        [head, tail] => {
            let head_lit = head.trim_start_matches("(?:^|/)").trim_start_matches('^');
            let tail_lit = tail.trim_end_matches('$').replace("\\.", ".");
            path.contains(head_lit) && path.ends_with(&tail_lit)
        }
        _ => false,
    }
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

#[cfg(test)]
mod glob_tests {
    use super::path_matches_glob;

    #[test]
    fn double_star_matches_anything() {
        assert!(path_matches_glob("**", "lib/a.dart"));
        assert!(path_matches_glob("**", "anything"));
    }

    #[test]
    fn double_star_slash_extension_matches_any_directory() {
        assert!(path_matches_glob("**/*.g.dart", "lib/gen/a.g.dart"));
        assert!(path_matches_glob(
            "**/*.freezed.dart",
            "build/x.freezed.dart"
        ));
        assert!(!path_matches_glob("**/*.g.dart", "lib/normal.dart"));
    }

    #[test]
    fn double_star_with_literal_tail_matches_basename() {
        assert!(path_matches_glob("**/Foo.dart", "lib/sub/Foo.dart"));
        assert!(!path_matches_glob("**/Foo.dart", "lib/Bar.dart"));
    }

    #[test]
    fn literal_directory_matches_top_or_nested_paths() {
        assert!(path_matches_glob("build", "build/x.dart"));
        assert!(path_matches_glob("build", "build"));
        assert!(path_matches_glob("build", "lib/build/x.dart"));
        assert!(!path_matches_glob("build", "lib/x.dart"));
    }

    #[test]
    fn dot_prefix_is_stripped_from_pattern() {
        assert!(path_matches_glob("./build", "build/x.dart"));
    }

    #[test]
    fn pattern_with_explicit_star_extension_falls_back_to_regex_like() {
        assert!(path_matches_glob("*.dart", "x.dart"));
        assert!(path_matches_glob("*.dart", "lib/x.dart"));
        assert!(!path_matches_glob("*.dart", "x.yaml"));
    }
}
