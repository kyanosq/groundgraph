//! Integration tests for `slice_requirement` using the watermark fixture.

use std::path::PathBuf;

use specslice_engine::dart_indexer::{index_dart, DartIndexOptions};
use specslice_engine::docs_indexer::{index_docs, DocsIndexOptions};
use specslice_engine::slice::slice_from_store;
use specslice_engine::SliceItem;
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

fn fresh_store_with_index() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();
    let fixture = fixture_path();
    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: fixture.clone(),
            doc_roots: vec![PathBuf::from("docs")],
        },
    )
    .unwrap();
    index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: fixture,
            code_roots: vec![PathBuf::from("lib"), PathBuf::from("test")],
            ..Default::default()
        },
    )
    .unwrap();
    (tmp, store)
}

fn item_paths(items: &[SliceItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| i.path.clone())
        .collect::<Vec<_>>()
}

#[test]
fn slicing_watermark_requirement_returns_docs_impl_and_tests() {
    let (_tmp, store) = fresh_store_with_index();
    let slice = slice_from_store(&store, "REQ-WATERMARK-001").unwrap();

    assert_eq!(slice.requirement_id, "REQ-WATERMARK-001");
    assert_eq!(slice.title.as_deref(), Some("Auto watermark placement"));

    let docs = item_paths(&slice.docs);
    assert!(
        docs.iter().any(|p| p == "docs/watermark.md"),
        "docs: {docs:?}"
    );

    let impls = item_paths(&slice.implementation);
    assert!(
        impls
            .iter()
            .any(|p| p == "lib/domain/watermark/auto_placement_service.dart"),
        "impl: {impls:?}"
    );

    let tests = item_paths(&slice.linked_tests);
    assert!(
        tests
            .iter()
            .any(|p| p == "test/watermark/auto_placement_service_test.dart"),
        "tests: {tests:?}"
    );

    // Has linked tests → risk should warn about declared-not-proven.
    assert!(slice.risks.iter().any(|r| r.contains("declared")));
}

#[test]
fn slicing_unknown_requirement_errors() {
    let (_tmp, store) = fresh_store_with_index();
    let err = slice_from_store(&store, "REQ-DOES-NOT-EXIST")
        .unwrap_err()
        .to_string();
    assert!(err.contains("REQ-DOES-NOT-EXIST"), "err = {err}");
}

#[test]
fn slicing_requirement_without_tests_reports_missing_linked_test_risk() {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();

    // Build a minimal graph: one Requirement + one implementing class, no test.
    use specslice_core::{
        artifact_id::{dart_class_id, requirement_id},
        EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind,
    };
    let mut req = Node::new(requirement_id("REQ-X"), NodeKind::Requirement);
    req.name = Some("X".into());
    store.upsert_node(&req).unwrap();
    let mut cls = Node::new(dart_class_id("lib/x.dart", "X"), NodeKind::DartClass);
    cls.path = Some("lib/x.dart".into());
    cls.name = Some("X".into());
    store.upsert_node(&cls).unwrap();
    store
        .upsert_edge(&EdgeAssertion::declared(
            cls.id.clone(),
            req.id.clone(),
            EdgeKind::DeclaresImplementation,
            EdgeSource::ExplicitTrace,
        ))
        .unwrap();

    let slice = slice_from_store(&store, "REQ-X").unwrap();
    assert!(slice.linked_tests.is_empty());
    assert!(slice
        .risks
        .iter()
        .any(|r| r.contains("no linked tests") || r.contains("missing @verifies")));
}
