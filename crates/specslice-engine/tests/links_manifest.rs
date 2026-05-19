//! External links manifest tests.
//!
//! SpecSlice must be non-invasive: business Markdown, Dart implementation, and
//! Dart tests are indexed as facts only. Requirement relationships are declared
//! in `.specslice/links.yaml`.

use specslice_core::{artifact_id::requirement_id, EdgeKind, NodeKind};
use specslice_engine::checks::{compute_checks, CheckSeverity};
use specslice_engine::index::{index_repository, IndexOptions};
use specslice_engine::init::{init_repository, InitOptions};
use specslice_store::Store;
use tempfile::TempDir;

fn write(path: &std::path::Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    write(
        &tmp.path().join("docs/watermark.md"),
        "---\nid: REQ-WATERMARK-001\ntype: requirement\ntitle: Auto watermark placement\n---\n\n# Auto watermark placement\n\nThe watermark must avoid detected face regions.\n",
    );
    write(
        &tmp.path()
            .join("lib/domain/watermark/auto_placement_service.dart"),
        "class AutoPlacementService {\n  void placeWatermark() {}\n}\n",
    );
    write(
        &tmp.path()
            .join("test/watermark/auto_placement_service_test.dart"),
        "void main() {\n  test('places watermark outside face region', () {});\n}\n",
    );
    tmp
}

#[test]
fn index_repository_uses_external_links_without_inline_annotations() {
    let tmp = workspace();
    write(
        &tmp.path().join(".specslice/links.yaml"),
        "requirements:\n  REQ-WATERMARK-001:\n    docs:\n      - docs/watermark.md#auto-watermark-placement\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n    tests:\n      - test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region\n",
    );

    let result = index_repository(IndexOptions::all(tmp.path())).unwrap();
    assert_eq!(result.code.unwrap().declared_implementations, 0);
    let links = result.links.expect("links result");
    assert_eq!(links.docs, 1);
    assert_eq!(links.implementations, 1);
    assert_eq!(links.tests, 1);

    let store = Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let req = requirement_id("REQ-WATERMARK-001");
    let incoming = store.list_edges_to(&req).unwrap();
    assert!(incoming.iter().any(|e| e.kind == EdgeKind::Documents));
    assert!(incoming
        .iter()
        .any(|e| e.kind == EdgeKind::DeclaresImplementation));
    assert!(incoming
        .iter()
        .any(|e| e.kind == EdgeKind::DeclaresVerification));

    let report = compute_checks(&store, None).unwrap();
    assert!(report.findings.is_empty(), "{:?}", report.findings);
}

#[test]
fn broken_manifest_link_is_reported_as_broken_link() {
    let tmp = workspace();
    write(
        &tmp.path().join(".specslice/links.yaml"),
        "requirements:\n  REQ-WATERMARK-001:\n    implementations:\n      - lib/domain/watermark/missing.dart#MissingService\n    tests:\n      - test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region\n",
    );

    index_repository(IndexOptions::all(tmp.path())).unwrap();
    let store = Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let report = compute_checks(&store, None).unwrap();
    let broken = report
        .findings
        .iter()
        .find(|f| f.code == "broken_link")
        .expect("broken link finding");
    assert_eq!(broken.severity, CheckSeverity::Error);
    assert!(broken.message.contains("MissingService"));
}

#[test]
fn links_manifest_can_create_requirement_without_frontmatter_doc() {
    let tmp = TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    write(
        &tmp.path().join("docs/notes.md"),
        "# Plain notes\n\nNo SpecSlice frontmatter is required here.\n",
    );
    write(
        &tmp.path().join(".specslice/links.yaml"),
        "requirements:\n  REQ-PLAIN-001:\n    docs:\n      - docs/notes.md#plain-notes\n",
    );

    let result = index_repository(IndexOptions::all(tmp.path())).unwrap();
    assert_eq!(result.docs.unwrap().requirements, 0);
    assert_eq!(result.links.unwrap().requirements, 1);

    let store = Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let req = store
        .find_node(&requirement_id("REQ-PLAIN-001"))
        .unwrap()
        .expect("requirement from links manifest");
    assert_eq!(req.kind, NodeKind::Requirement);
    assert_eq!(req.source_file.as_deref(), Some(".specslice/links.yaml"));
}
