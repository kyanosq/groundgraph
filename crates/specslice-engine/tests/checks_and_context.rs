//! Integration tests for checks and context_pack.

use std::path::PathBuf;

use specslice_core::{
    artifact_id::{dart_class_id, requirement_id},
    EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind,
};
use specslice_engine::checks::{compute_checks, CheckSeverity};
use specslice_engine::context_pack::{node_kind_counts, ContextOptions};
use specslice_engine::dart_indexer::{index_dart, DartIndexOptions};
use specslice_engine::docs_indexer::{index_docs, DocsIndexOptions};
use specslice_engine::impact::ImpactReport;
use specslice_engine::links_indexer::{index_links, LinksIndexOptions};
use specslice_engine::{build_context, EngineConfig};
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
            include_globs: Vec::new(),
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
    index_links(
        &mut store,
        &LinksIndexOptions {
            repo_root: fixture_path(),
            manifest_path: PathBuf::from(".specslice/links.yaml"),
        },
    )
    .unwrap();
    (tmp, store)
}

fn write_workspace(tmp: &std::path::Path) {
    std::fs::write(
        tmp.join(".specslice.yaml"),
        serde_yml::to_string(&EngineConfig::default()).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join(".specslice")).unwrap();
}

#[test]
fn watermark_fixture_has_clean_checks() {
    let (_tmp, store) = fresh_store_with_index();
    let report = compute_checks(&store, None).unwrap();
    assert_eq!(
        report
            .findings
            .iter()
            .filter(|f| f.severity == CheckSeverity::Error)
            .count(),
        0,
        "no errors expected"
    );
    // The fixture is a happy path so we expect no warnings.
    assert!(report.findings.is_empty(), "{:#?}", report.findings);
}

#[test]
fn broken_link_is_reported_as_error() {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();
    // Class that declares implementation of a non-existent requirement.
    let mut cls = Node::new(dart_class_id("lib/x.dart", "X"), NodeKind::DartClass);
    cls.path = Some("lib/x.dart".into());
    cls.name = Some("X".into());
    store.upsert_node(&cls).unwrap();
    store
        .upsert_edge(&EdgeAssertion::declared(
            cls.id.clone(),
            requirement_id("REQ-MISSING"),
            EdgeKind::DeclaresImplementation,
            EdgeSource::ExternalManifest,
        ))
        .unwrap();
    let report = compute_checks(&store, None).unwrap();
    assert!(report.has_errors());
    assert!(report
        .findings
        .iter()
        .any(|f| f.code == "broken_link" && f.message.contains("REQ-MISSING")));
}

#[test]
fn orphan_requirement_is_warning() {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();
    let mut req = Node::new(requirement_id("REQ-LONELY"), NodeKind::Requirement);
    req.name = Some("Lonely".into());
    store.upsert_node(&req).unwrap();
    let report = compute_checks(&store, None).unwrap();
    assert_eq!(report.errors(), 0);
    assert!(report
        .findings
        .iter()
        .any(|f| f.code == "orphan_requirement"));
}

#[test]
fn impact_warnings_propagate_as_check_findings() {
    let (_tmp, store) = fresh_store_with_index();
    let impact = ImpactReport {
        warnings: vec!["something changed without tests".into()],
        info: vec!["docs not touched".into()],
        ..Default::default()
    };
    let report = compute_checks(&store, Some(&impact)).unwrap();
    assert!(report
        .findings
        .iter()
        .any(|f| f.code == "impact_review" && f.severity == CheckSeverity::Warning));
    assert!(report
        .findings
        .iter()
        .any(|f| f.code == "impact_review" && f.severity == CheckSeverity::Info));
}

#[test]
fn context_pack_includes_slice_snippets_and_edges() {
    let tmp = TempDir::new().unwrap();
    let fixture = fixture_path();
    // Copy fixture to a temp root so context can read snippets.
    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let target = dst.join(entry.file_name());
            if path.is_dir() {
                copy_dir(&path, &target);
            } else {
                std::fs::copy(&path, &target).unwrap();
            }
        }
    }
    copy_dir(&fixture, tmp.path());
    write_workspace(tmp.path());

    // Index into the right .specslice/graph.db.
    let db_path = tmp.path().join(".specslice/graph.db");
    let mut store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: tmp.path().into(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: Vec::new(),
        },
    )
    .unwrap();
    index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: tmp.path().into(),
            code_roots: vec![PathBuf::from("lib"), PathBuf::from("test")],
            ..Default::default()
        },
    )
    .unwrap();
    index_links(
        &mut store,
        &LinksIndexOptions {
            repo_root: tmp.path().into(),
            manifest_path: PathBuf::from(".specslice/links.yaml"),
        },
    )
    .unwrap();
    drop(store);

    let pack = build_context(ContextOptions {
        repo_root: tmp.path().into(),
        requirement: "REQ-WATERMARK-001".into(),
        include_snippets: true,
    })
    .unwrap();
    assert_eq!(pack.requirement_id, "REQ-WATERMARK-001");
    assert!(!pack.docs_snippets.is_empty());
    assert!(!pack.impl_snippets.is_empty());
    assert!(!pack.test_snippets.is_empty());
    assert!(!pack.edges.is_empty());
    // The doc snippet must reference the heading line.
    assert!(pack
        .docs_snippets
        .iter()
        .any(|s| s.text.contains("Auto watermark placement")));
}

#[test]
fn context_pack_exposes_files_to_read_and_tests_to_run() {
    let tmp = TempDir::new().unwrap();
    let fixture = fixture_path();
    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let target = dst.join(entry.file_name());
            if path.is_dir() {
                copy_dir(&path, &target);
            } else {
                std::fs::copy(&path, &target).unwrap();
            }
        }
    }
    copy_dir(&fixture, tmp.path());
    write_workspace(tmp.path());

    let db_path = tmp.path().join(".specslice/graph.db");
    let mut store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: tmp.path().into(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: Vec::new(),
        },
    )
    .unwrap();
    index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: tmp.path().into(),
            code_roots: vec![PathBuf::from("lib"), PathBuf::from("test")],
            ..Default::default()
        },
    )
    .unwrap();
    index_links(
        &mut store,
        &LinksIndexOptions {
            repo_root: tmp.path().into(),
            manifest_path: PathBuf::from(".specslice/links.yaml"),
        },
    )
    .unwrap();
    drop(store);

    let pack = build_context(ContextOptions {
        repo_root: tmp.path().into(),
        requirement: "REQ-WATERMARK-001".into(),
        include_snippets: false,
    })
    .unwrap();

    // PRD §7: the JSON contract exposes a flat list of files an Agent must
    // read and a flat list of test files it must run.
    assert!(pack.files_to_read.iter().any(|p| p == "docs/watermark.md"));
    assert!(pack
        .files_to_read
        .iter()
        .any(|p| p == "lib/domain/watermark/auto_placement_service.dart"));
    assert!(pack
        .files_to_read
        .iter()
        .any(|p| p == "test/watermark/auto_placement_service_test.dart"));
    assert!(pack
        .tests_to_run
        .iter()
        .any(|p| p == "test/watermark/auto_placement_service_test.dart"));

    // Both lists must be deduplicated.
    let mut sorted = pack.files_to_read.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        pack.files_to_read.len(),
        "duplicates not allowed"
    );
}

#[test]
fn node_kind_counts_summarises_store() {
    let (_tmp, store) = fresh_store_with_index();
    let counts = node_kind_counts(&store).unwrap();
    assert!(counts.get("requirement").copied().unwrap_or(0) >= 1);
    assert!(counts.get("dart_class").copied().unwrap_or(0) >= 1);
}
