//! Tests for `Related` link persistence + broken-related check (PRD §5/§6).
//!
//! PRD says broken Related references must be `broken_trace` errors. MVP-1
//! only collected them into an in-memory `unresolved_references` list. These
//! tests pin the new contract:
//! 1. Indexer emits `Requirement --RelatedTo--> <symbol://...>` edges.
//! 2. `compute_checks` flags Related targets that have no matching node.
//! 3. Resolved Related targets (matching symbol or test in the graph) do not
//!    trigger the warning.

use std::path::PathBuf;

use specslice_core::{
    artifact_id::{dart_class_id, dart_test_id, file_id, requirement_id},
    EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind,
};
use specslice_engine::checks::{compute_checks, CheckSeverity};
use specslice_engine::dart_indexer::{index_dart, DartIndexOptions};
use specslice_engine::docs_indexer::{index_docs, DocsIndexOptions};
use specslice_store::Store;
use tempfile::TempDir;

fn workspace() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();
    (tmp, store)
}

fn write_doc(tmp: &TempDir, path: &str, body: &str) {
    let dest = tmp.path().join(path);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(dest, body).unwrap();
}

#[test]
fn indexing_emits_related_edges_for_each_link() {
    let (tmp, mut store) = workspace();
    write_doc(
        &tmp,
        "docs/r.md",
        "---\nid: REQ-R-1\ntype: requirement\ntitle: T\n---\n\n# Top\n\n## Related\n\n- symbol://lib/a.dart#Foo\n- test://test/a_test.dart#case-x\n",
    );
    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: tmp.path().into(),
            doc_roots: vec![PathBuf::from("docs")],
        },
    )
    .unwrap();

    let edges = store
        .list_edges_from(&requirement_id("REQ-R-1"))
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EdgeKind::RelatedTo)
        .collect::<Vec<_>>();
    let targets: Vec<_> = edges.iter().map(|e| e.to_id.to_string()).collect();
    assert!(
        targets.iter().any(|t| t == "symbol://lib/a.dart#Foo"),
        "expected symbol related edge, got {targets:?}"
    );
    assert!(
        targets
            .iter()
            .any(|t| t == "test://test/a_test.dart#case-x"),
        "expected test related edge, got {targets:?}"
    );
}

#[test]
fn checks_flag_broken_related_when_target_missing() {
    let (tmp, mut store) = workspace();
    write_doc(
        &tmp,
        "docs/r.md",
        "---\nid: REQ-R-2\ntype: requirement\ntitle: T\n---\n\n# Top\n\n## Related\n\n- symbol://lib/nope.dart#Ghost\n",
    );
    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: tmp.path().into(),
            doc_roots: vec![PathBuf::from("docs")],
        },
    )
    .unwrap();

    let report = compute_checks(&store, None).unwrap();
    let broken: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.code == "broken_related")
        .collect();
    assert_eq!(broken.len(), 1, "{:?}", report.findings);
    assert_eq!(broken[0].severity, CheckSeverity::Error);
    assert!(broken[0].message.contains("symbol://lib/nope.dart#Ghost"));
}

#[test]
fn checks_resolve_symbol_uri_against_indexed_dart_class() {
    let (tmp, mut store) = workspace();
    // Create a doc requirement and a Dart file whose class matches the URI.
    write_doc(
        &tmp,
        "docs/r.md",
        "---\nid: REQ-R-3\ntype: requirement\ntitle: T\n---\n\n# Top\n\n## Related\n\n- symbol://lib/a.dart#Foo\n",
    );
    let lib = tmp.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("a.dart"), "class Foo {}\n").unwrap();

    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: tmp.path().into(),
            doc_roots: vec![PathBuf::from("docs")],
        },
    )
    .unwrap();
    index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: tmp.path().into(),
            code_roots: vec![PathBuf::from("lib")],
            ..Default::default()
        },
    )
    .unwrap();

    let report = compute_checks(&store, None).unwrap();
    assert!(
        !report.findings.iter().any(|f| f.code == "broken_related"),
        "should resolve symbol://lib/a.dart#Foo to dart_class node, got {:?}",
        report.findings
    );
}

#[test]
fn checks_resolve_test_uri_against_indexed_dart_test() {
    let (tmp, mut store) = workspace();
    write_doc(
        &tmp,
        "docs/r.md",
        "---\nid: REQ-R-4\ntype: requirement\ntitle: T\n---\n\n# Top\n\n## Related\n\n- test://test/a_test.dart#my-case\n",
    );
    // Manually upsert a matching test node so the resolver finds it without
    // depending on the Dart parser's slugifier nuances.
    let mut node = Node::new(
        dart_test_id("test/a_test.dart", "my-case"),
        NodeKind::TestCase,
    );
    node.path = Some("test/a_test.dart".into());
    node.name = Some("my-case".into());
    store.upsert_node(&node).unwrap();
    // Also stash the surrounding file so the resolver can match by path.
    let mut f = Node::new(file_id("test/a_test.dart"), NodeKind::File);
    f.path = Some("test/a_test.dart".into());
    store.upsert_node(&f).unwrap();

    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: tmp.path().into(),
            doc_roots: vec![PathBuf::from("docs")],
        },
    )
    .unwrap();

    let report = compute_checks(&store, None).unwrap();
    assert!(
        !report.findings.iter().any(|f| f.code == "broken_related"),
        "test:// uri should resolve, got {:?}",
        report.findings
    );
}

#[test]
fn ref_scheme_resolves_to_known_node_paths() {
    // `ref://path#name` is the catch-all bucket used by `@related` markers
    // that are neither pure symbol nor pure test references.
    let (_tmp, mut store) = workspace();
    let mut req = Node::new(requirement_id("REQ-R-REF"), NodeKind::Requirement);
    req.stable_key = Some("REQ-R-REF".into());
    store.upsert_node(&req).unwrap();
    let mut node = Node::new(file_id("docs/glossary.md"), NodeKind::File);
    node.path = Some("docs/glossary.md".into());
    store.upsert_node(&node).unwrap();
    // ref://path with no fragment should resolve via file_id.
    let edge = EdgeAssertion::declared(
        req.id.clone(),
        specslice_core::ArtifactId::new("ref://docs/glossary.md"),
        EdgeKind::RelatedTo,
        EdgeSource::Markdown,
    );
    store.upsert_edge(&edge).unwrap();

    let report = compute_checks(&store, None).unwrap();
    assert!(
        !report.findings.iter().any(|f| f.code == "broken_related"),
        "ref:// path should resolve, got {:?}",
        report.findings
    );
}

#[test]
fn ref_scheme_without_match_is_flagged_as_broken() {
    let (_tmp, mut store) = workspace();
    let mut req = Node::new(requirement_id("REQ-R-REF2"), NodeKind::Requirement);
    req.stable_key = Some("REQ-R-REF2".into());
    store.upsert_node(&req).unwrap();
    let edge = EdgeAssertion::declared(
        req.id.clone(),
        specslice_core::ArtifactId::new("ref://nope/dangling.md#section"),
        EdgeKind::RelatedTo,
        EdgeSource::Markdown,
    );
    store.upsert_edge(&edge).unwrap();
    let report = compute_checks(&store, None).unwrap();
    assert!(report.findings.iter().any(
        |f| f.code == "broken_related" && f.message.contains("ref://nope/dangling.md#section")
    ));
}

#[test]
fn related_edge_pointing_at_real_id_does_not_get_flagged() {
    // Edges with a fully qualified artifact id (e.g. from Dart's `@related`)
    // must keep working: a `RelatedTo` edge whose `to_id` is an existing
    // node should never trigger `broken_related`.
    let (_tmp, mut store) = workspace();
    let mut node = Node::new(dart_class_id("lib/a.dart", "Foo"), NodeKind::DartClass);
    node.path = Some("lib/a.dart".into());
    node.name = Some("Foo".into());
    store.upsert_node(&node).unwrap();

    let mut req = Node::new(requirement_id("REQ-R-5"), NodeKind::Requirement);
    req.stable_key = Some("REQ-R-5".into());
    store.upsert_node(&req).unwrap();

    let edge = EdgeAssertion::declared(
        req.id.clone(),
        node.id.clone(),
        EdgeKind::RelatedTo,
        EdgeSource::ExplicitTrace,
    );
    store.upsert_edge(&edge).unwrap();

    let report = compute_checks(&store, None).unwrap();
    assert!(!report.findings.iter().any(|f| f.code == "broken_related"));
}
