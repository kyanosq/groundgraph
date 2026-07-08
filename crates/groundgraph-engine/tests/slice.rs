//! Integration tests for `slice_requirement` using the watermark fixture.

use std::path::PathBuf;

use groundgraph_core::edge::EdgeKind;
use groundgraph_core::node::NodeKind;
use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeSource, Node, SymbolRange};
use groundgraph_engine::dart_indexer::{index_dart, DartIndexOptions};
use groundgraph_engine::docs_indexer::{index_docs, DocsIndexOptions};
use groundgraph_engine::links_indexer::{index_links, LinksIndexOptions};
use groundgraph_engine::slice::{
    slice_from_store, slice_from_store_with_options, SliceFanoutOptions,
};
use groundgraph_engine::SliceItem;
use groundgraph_store::Store;
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
            include_globs: Vec::new(),
        },
    )
    .unwrap();
    index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: fixture.clone(),
            code_roots: vec![PathBuf::from("lib"), PathBuf::from("test")],
            ..Default::default()
        },
    )
    .unwrap();
    index_links(
        &mut store,
        &LinksIndexOptions {
            repo_root: fixture,
            manifest_path: PathBuf::from(".groundgraph/links.yaml"),
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
    assert_eq!(
        slice.title.as_deref(),
        None,
        "links manifest creates confirmed IDs, not AI-parsed business titles"
    );

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

    // Has linked tests → risk should warn that links are not coverage proof.
    assert!(slice.risks.iter().any(|r| r.contains("linked")));
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
    use groundgraph_core::{
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
            EdgeSource::ExternalManifest,
        ))
        .unwrap();

    let slice = slice_from_store(&store, "REQ-X").unwrap();
    assert!(slice.linked_tests.is_empty());
    assert!(slice
        .risks
        .iter()
        .any(|r| r.contains("no linked verification tests")));
}

/// P14 — slice must fan out one hop along forward `Calls` / `References`
/// edges from declared implementation symbols, so reviewers see the
/// transitive code touched by a requirement even when the manifest only
/// declares the class entry-point. The previous behaviour (manifest-
/// only) is still reachable via `call_depth = 0`.
#[test]
fn slicing_fans_out_via_calls_and_references_from_implementations() {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();

    let req_id = ArtifactId::new("req::REQ-FANOUT");
    let mut req = Node::new(req_id.clone(), NodeKind::Requirement);
    req.name = Some("REQ-FANOUT".into());
    store.upsert_node(&req).unwrap();

    // Two implementations: A (DartMethod) calls B (DartMethod), B
    // references C (DartFunction). Slice depth=1 must surface B; depth=2
    // must surface both B and C. Depth=0 keeps the legacy behaviour.
    let a_id = ArtifactId::new("dart_method::lib/a.dart#A.run");
    let b_id = ArtifactId::new("dart_method::lib/a.dart#B.helper");
    let c_id = ArtifactId::new("dart_function::lib/util.dart#util");

    for (id, kind, path, name) in [
        (a_id.clone(), NodeKind::DartMethod, "lib/a.dart", "A.run"),
        (b_id.clone(), NodeKind::DartMethod, "lib/a.dart", "B.helper"),
        (
            c_id.clone(),
            NodeKind::DartFunction,
            "lib/util.dart",
            "util",
        ),
    ] {
        let mut node = Node::new(id.clone(), kind);
        node.name = Some(name.to_string());
        node.path = Some(path.to_string());
        node.start_line = Some(1);
        node.end_line = Some(10);
        store.upsert_node(&node).unwrap();
        store
            .upsert_symbol_range(&SymbolRange {
                file_path: path.into(),
                symbol_id: id,
                start_line: 1,
                end_line: 10,
                symbol_kind: kind,
                qualified_name: name.into(),
                parent_symbol_id: None,
            })
            .unwrap();
    }

    store
        .upsert_edge(&EdgeAssertion::declared(
            a_id.clone(),
            req_id.clone(),
            EdgeKind::DeclaresImplementation,
            EdgeSource::ExternalManifest,
        ))
        .unwrap();
    store
        .upsert_edge(&EdgeAssertion::fact(
            a_id.clone(),
            b_id.clone(),
            EdgeKind::Calls,
            EdgeSource::LanguageAdapter,
        ))
        .unwrap();
    store
        .upsert_edge(&EdgeAssertion::fact(
            b_id.clone(),
            c_id.clone(),
            EdgeKind::References,
            EdgeSource::LanguageAdapter,
        ))
        .unwrap();

    // Default behaviour (depth=1): must surface B but not C.
    let slice = slice_from_store(&store, "REQ-FANOUT").unwrap();
    let fanout_ids: Vec<&str> = slice.code_fanout.iter().map(|s| s.id.as_str()).collect();
    assert!(
        fanout_ids.contains(&b_id.as_str()),
        "depth=1 callee B must be in code_fanout, got {fanout_ids:?}"
    );
    assert!(
        !fanout_ids.contains(&c_id.as_str()),
        "depth=1 must not reach C (transitive), got {fanout_ids:?}"
    );
    assert!(
        !fanout_ids.contains(&a_id.as_str()),
        "implementation symbol must not duplicate into code_fanout"
    );

    // depth=2 reaches C.
    let slice2 =
        slice_from_store_with_options(&store, "REQ-FANOUT", SliceFanoutOptions { call_depth: 2 })
            .unwrap();
    let fanout2: Vec<&str> = slice2.code_fanout.iter().map(|s| s.id.as_str()).collect();
    assert!(
        fanout2.contains(&c_id.as_str()),
        "depth=2 must reach transitive callee C, got {fanout2:?}"
    );

    // depth=0 disables propagation (back-compat with pre-P14).
    let slice0 =
        slice_from_store_with_options(&store, "REQ-FANOUT", SliceFanoutOptions { call_depth: 0 })
            .unwrap();
    assert!(slice0.code_fanout.is_empty(), "depth=0 must disable fanout");
}
