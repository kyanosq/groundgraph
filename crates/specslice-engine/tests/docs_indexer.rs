//! Integration tests for the markdown / requirement indexer.

use std::path::PathBuf;

use specslice_core::{
    artifact_id::{doc_section_id, file_id, requirement_id},
    EdgeKind, NodeKind,
};
use specslice_engine::docs_indexer::{index_docs, DocsIndexOptions, DOCS_INDEXER_NAME};
use specslice_store::Store;
use tempfile::TempDir;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("flutter_watermark_app")
}

fn fresh_store() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();
    (tmp, store)
}

#[test]
fn indexing_watermark_fixture_creates_requirement_and_doc_section() {
    let (_tmp, mut store) = fresh_store();
    let fixture = fixture_path();
    let opts = DocsIndexOptions {
        repo_root: fixture,
        doc_roots: vec![PathBuf::from("docs")],
        include_globs: Vec::new(),
    };
    let result = index_docs(&mut store, &opts).unwrap();

    assert_eq!(result.files, 1, "expected exactly 1 file");
    assert_eq!(result.requirements, 1, "expected exactly 1 requirement");
    assert!(result.doc_sections >= 1, "expected at least 1 doc section");

    let req = store
        .find_node(&requirement_id("REQ-WATERMARK-001"))
        .unwrap()
        .expect("requirement node");
    assert_eq!(req.kind, NodeKind::Requirement);
    assert_eq!(req.name.as_deref(), Some("Auto watermark placement"));

    let section = store
        .find_node(&doc_section_id(
            "docs/watermark.md",
            "auto-watermark-placement",
        ))
        .unwrap()
        .expect("doc section node");
    assert_eq!(section.kind, NodeKind::DocSection);
    assert_eq!(section.start_line, Some(8));

    let file = store
        .find_node(&file_id("docs/watermark.md"))
        .unwrap()
        .expect("file node");
    assert_eq!(file.kind, NodeKind::File);
    assert!(file.content_hash.is_some());

    // documents edge: section -> requirement
    let docs_edges = store.list_edges_by_kind(EdgeKind::Documents).unwrap();
    assert_eq!(docs_edges.len(), 1);
    assert_eq!(docs_edges[0].from_id, section.id);
    assert_eq!(docs_edges[0].to_id, req.id);

    // contains edge: file -> section (at least one)
    let contains_edges = store.list_edges_by_kind(EdgeKind::Contains).unwrap();
    assert!(contains_edges.iter().any(|e| e.from_id == file.id));
}

#[test]
fn re_indexing_is_idempotent_and_clears_previous_outputs() {
    let (_tmp, mut store) = fresh_store();
    let fixture = fixture_path();
    let opts = DocsIndexOptions {
        repo_root: fixture,
        doc_roots: vec![PathBuf::from("docs")],
        include_globs: Vec::new(),
    };

    let first = index_docs(&mut store, &opts).unwrap();
    store.clear_indexer_outputs(DOCS_INDEXER_NAME).unwrap();
    let second = index_docs(&mut store, &opts).unwrap();

    assert_eq!(first.requirements, second.requirements);
    assert_eq!(first.doc_sections, second.doc_sections);
    assert_eq!(
        store
            .list_nodes_by_kind(NodeKind::Requirement)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.list_edges_by_kind(EdgeKind::Documents).unwrap().len(),
        1
    );
}

#[test]
fn missing_doc_root_is_skipped() {
    let (_tmp, mut store) = fresh_store();
    let tmp_root = tempfile::tempdir().unwrap();
    let opts = DocsIndexOptions {
        repo_root: tmp_root.path().into(),
        doc_roots: vec![PathBuf::from("does_not_exist")],
        include_globs: Vec::new(),
    };
    let result = index_docs(&mut store, &opts).unwrap();
    assert_eq!(result.files, 0);
    assert_eq!(result.requirements, 0);
}

#[test]
fn duplicate_doc_roots_do_not_double_count_files() {
    let (_tmp, mut store) = fresh_store();
    let opts = DocsIndexOptions {
        repo_root: fixture_path(),
        doc_roots: vec![PathBuf::from("docs"), PathBuf::from("docs")],
        include_globs: Vec::new(),
    };
    let result = index_docs(&mut store, &opts).unwrap();
    assert_eq!(result.files, 1);
    assert_eq!(result.requirements, 1);
}
