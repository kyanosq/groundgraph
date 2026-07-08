//! Content-layer search regression (FTS5 / BM25).
//!
//! The 2026-06 neutral review proved the gap with real queries: words that
//! live only in code comments or doc bodies — `byte boundary panic`,
//! `错位竞争` — returned irrelevant or zero hits, because `score_node` only
//! looks at ids / names / paths. GitNexus & CodeGraph both search content.
//! These tests pin the fix: the fulltext content layer must make
//! comment-only and doc-body-only phrases findable, bilingually, and must
//! degrade gracefully (warning, not error) on a pre-FTS database.

use std::path::{Path, PathBuf};

use groundgraph_engine::docs_indexer::{index_docs, DocsIndexOptions};
use groundgraph_engine::fulltext_indexer::rebuild_fulltext_index;
use groundgraph_engine::search::{run_search_with_store, SearchOptions};
use groundgraph_engine::{index_rust, RustIndexOptions};
use groundgraph_store::Store;

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Index a small bilingual fixture: one Rust file whose *comment* carries a
/// phrase absent from every identifier, and one markdown doc whose *body*
/// carries a Chinese phrase absent from its heading.
fn indexed_fixture() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    write(
        &root,
        "src/parser.rs",
        r#"/// Advance the scan cursor past the closing paren. Computing the next
/// offset from the body length used to land inside a multi-byte char —
/// the classic byte boundary panic this module guards against.
pub fn advance_cursor(input: &str) -> usize {
    input.len()
}

/// Unrelated helper that talks about routing tables only.
pub fn route_tables() -> u32 {
    7
}
"#,
    );
    write(
        &root,
        "docs/strategy.md",
        "# 战略定位\n\n与 CodeGraph 的关系是错位竞争：内化检索层，对外讲意图对齐层。\n",
    );

    let mut store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    store.migrate().unwrap();
    index_rust(
        &mut store,
        &RustIndexOptions {
            repo_root: root.clone(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        },
    )
    .unwrap();
    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: root.clone(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: vec!["**/*.md".into()],
        },
    )
    .unwrap();
    let stats = rebuild_fulltext_index(&mut store, &root).unwrap();
    assert!(
        stats.nodes_indexed >= 3,
        "expected fn + doc section bodies in the fulltext index, got {}",
        stats.nodes_indexed
    );
    (tmp, store)
}

#[test]
fn comment_only_phrase_hits_the_function_via_content_layer() {
    let (tmp, store) = indexed_fixture();
    let result = run_search_with_store(
        &store,
        SearchOptions::keywords(tmp.path(), "byte boundary panic"),
    )
    .unwrap();
    let top = result.matches.first().expect("must hit something");
    assert_eq!(
        top.label,
        "advance_cursor",
        "comment-only phrase must rank the documented fn first, got {:?}",
        result
            .matches
            .iter()
            .map(|m| m.label.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        top.match_reasons.iter().any(|r| r.contains("content")),
        "the hit must be explained as a content match: {:?}",
        top.match_reasons
    );
    // The unrelated fn must not outrank the documented one.
    assert!(
        result
            .matches
            .iter()
            .all(|m| m.label != "route_tables" || m.score < top.score),
        "unrelated fn must not tie/beat the content hit"
    );
}

#[test]
fn chinese_doc_body_phrase_hits_the_doc_section() {
    let (tmp, store) = indexed_fixture();
    let result =
        run_search_with_store(&store, SearchOptions::keywords(tmp.path(), "错位竞争")).unwrap();
    assert!(
        result
            .matches
            .iter()
            .any(|m| m.kind == "doc_section" && m.score > 0),
        "Chinese body phrase must surface the doc section, got {:?}",
        result
            .matches
            .iter()
            .map(|m| (m.kind.as_str(), m.label.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn matches_carry_a_source_snippet_showing_the_matching_line() {
    let (tmp, store) = indexed_fixture();
    let result = run_search_with_store(
        &store,
        SearchOptions::keywords(tmp.path(), "byte boundary panic"),
    )
    .unwrap();
    let top = result.matches.first().expect("must hit");
    let snippet = top
        .snippet
        .as_deref()
        .expect("top hit must carry a source snippet");
    assert!(
        snippet.contains("byte boundary panic"),
        "snippet should surface the matching line (incl. leading doc comment), got: {snippet}"
    );
}

#[test]
fn pre_fts_database_degrades_with_a_warning_not_an_error() {
    let (tmp, store) = indexed_fixture();
    // Simulate a graph.db produced by an older binary: content table absent.
    store
        .connection()
        .execute_batch("DROP TABLE node_fts;")
        .unwrap();
    let result = run_search_with_store(
        &store,
        SearchOptions::keywords(tmp.path(), "byte boundary panic"),
    )
    .expect("search must not error on a pre-FTS database");
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("groundgraph index")),
        "must tell the operator how to enable the content layer, got {:?}",
        result.warnings
    );
}
