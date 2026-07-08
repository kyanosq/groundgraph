//! P23.9 — end-to-end coverage for the Markdown requirements indexer.
//!
//! Proves the recommended `.groundgraph/requirements/*.md` format flows through
//! the real `index_repository` pipeline: requirement nodes are created and
//! `Documents` / `DeclaresImplementation` / `DeclaresVerification` edges resolve
//! language-agnostically onto the same graph the manifest indexer feeds, with
//! deterministic reindexing and best-effort handling of dangling references.
//! It also confirms `init` scaffolds the directory (non-invasive: only under
//! `.groundgraph/`).

use groundgraph_core::artifact_id::requirement_id;
use groundgraph_core::{EdgeKind, NodeKind};
use groundgraph_engine::config::DEFAULT_CONFIG_FILE_NAME;
use groundgraph_engine::index::{index_repository, IndexOptions};
use groundgraph_engine::init::{init_repository, InitOptions};
use groundgraph_store::Store;
use tempfile::TempDir;

/// Unified P23.7 config: Dart structure via tree-sitter, analyzer overlay off
/// so the test is hermetic and fast.
const CONFIG: &str = "\
repo:
  root: .
  default_branch: main
storage:
  path: .groundgraph/graph.db
docs:
  paths:
    - docs
languages:
  - id: dart
    paths:
      - lib
      - test
enrichment:
  analyzer: false
";

fn write(root: &std::path::Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Build a repo with one doc section, one Dart class, one Dart test file, plus a
/// requirement file referencing all three. Returns the repo `TempDir`.
fn fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    std::fs::write(root.join(DEFAULT_CONFIG_FILE_NAME), CONFIG).unwrap();
    // `init` scaffolds a sample requirement; drop it so counts are exact.
    std::fs::remove_dir_all(root.join(".groundgraph/requirements")).ok();

    write(root, "docs/spec.md", "# Overview\n\nSome overview text.\n");
    write(root, "lib/a.dart", "class Widget {\n  void build() {}\n}\n");
    write(root, "test/a_test.dart", "void main() {}\n");
    write(
        root,
        ".groundgraph/requirements/0001-demo.md",
        "# REQ-DEMO 演示需求\n\
         \n\
         演示用需求，描述意图。\n\
         \n\
         ## 文档\n\
         - docs/spec.md#Overview\n\
         \n\
         ## 实现\n\
         - lib/a.dart#Widget\n\
         \n\
         ## 测试\n\
         - test/a_test.dart\n",
    );
    tmp
}

#[test]
fn requirements_md_resolves_doc_impl_and_test_edges() {
    let tmp = fixture();
    let root = tmp.path();

    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();

    // The manifest path still ran alongside the markdown path (compat).
    assert!(result.links.is_some(), "links phase should run");

    let reqs = result.requirements_md.expect("requirements_md present");
    assert_eq!(reqs.files, 1);
    assert_eq!(reqs.requirements, 1);
    assert_eq!(reqs.documents, 1);
    assert_eq!(reqs.implementations, 1);
    assert_eq!(reqs.verifications, 1);
    assert_eq!(reqs.edges, 3);
    assert_eq!(reqs.unresolved, 0, "every reference should resolve");

    let store = Store::open(root.join(".groundgraph/graph.db")).unwrap();

    // Requirement node created with the Chinese title.
    let req_nodes = store.list_nodes_by_kind(NodeKind::Requirement).unwrap();
    assert_eq!(req_nodes.len(), 1);
    assert_eq!(req_nodes[0].name.as_deref(), Some("演示需求"));

    let req_id = requirement_id("REQ-DEMO");
    let into_req = store.list_edges_to(&req_id).unwrap();
    assert_eq!(into_req.len(), 3, "doc + impl + test edges point into req");

    // The implementation edge resolves to the real Dart class node.
    let widget = store
        .list_nodes_by_kind(NodeKind::DartClass)
        .unwrap()
        .into_iter()
        .find(|n| n.name.as_deref() == Some("Widget"))
        .expect("Widget class indexed");
    let impl_edge = into_req
        .iter()
        .find(|e| e.kind == EdgeKind::DeclaresImplementation)
        .expect("implementation edge present");
    assert_eq!(impl_edge.from_id, widget.id);

    // The doc edge resolves to the DocSection (not a file fallback).
    let doc_edge = into_req
        .iter()
        .find(|e| e.kind == EdgeKind::Documents)
        .expect("documents edge present");
    assert!(
        doc_edge.from_id.as_str().starts_with("docsec::"),
        "doc edge should target a DocSection, got {}",
        doc_edge.from_id.as_str()
    );

    // The verification edge resolves to the test file node.
    let test_edge = into_req
        .iter()
        .find(|e| e.kind == EdgeKind::DeclaresVerification)
        .expect("verification edge present");
    assert_eq!(test_edge.from_id.as_str(), "file::test/a_test.dart");
}

#[test]
fn requirements_md_reindex_is_idempotent() {
    let tmp = fixture();
    let root = tmp.path();

    let first = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let second = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    assert_eq!(first.requirements_md, second.requirements_md);

    // No duplicate requirement nodes or edges after a second pass.
    let store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    assert_eq!(
        store
            .list_nodes_by_kind(NodeKind::Requirement)
            .unwrap()
            .len(),
        1
    );
    let req_id = requirement_id("REQ-DEMO");
    assert_eq!(store.list_edges_to(&req_id).unwrap().len(), 3);
}

#[test]
fn requirements_md_counts_unresolved_references() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    std::fs::write(root.join(DEFAULT_CONFIG_FILE_NAME), CONFIG).unwrap();
    std::fs::remove_dir_all(root.join(".groundgraph/requirements")).ok();
    write(root, "lib/a.dart", "class Widget {}\n");
    write(
        root,
        ".groundgraph/requirements/req.md",
        "# REQ-X 缺失引用\n\n## 实现\n- lib/a.dart#DoesNotExist\n",
    );

    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let reqs = result.requirements_md.expect("requirements_md present");
    assert_eq!(reqs.implementations, 1);
    assert_eq!(reqs.edges, 1);
    assert_eq!(reqs.unresolved, 1, "dangling fragment counts as unresolved");

    // A best-effort edge still lands on the file so `checks` can flag it.
    let store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    let into_req = store.list_edges_to(&requirement_id("REQ-X")).unwrap();
    assert_eq!(into_req.len(), 1);
    assert_eq!(into_req[0].from_id.as_str(), "file::lib/a.dart");
}

#[test]
fn init_scaffolds_requirements_directory() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let outcome = init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();

    assert!(!outcome.requirements_already_existed);
    assert!(outcome.requirements_dir.is_dir());
    assert!(
        outcome.requirements_dir.join("README.md").is_file(),
        "init scaffolds a README guide"
    );

    // Re-running init keeps the existing scaffold (idempotent, non-destructive).
    let again = init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    assert!(again.requirements_already_existed);
}

#[test]
fn scaffolded_readme_is_not_indexed_as_a_requirement() {
    // A fresh `init` (README scaffold only) must leave the graph empty — the
    // README is documentation, not a live requirement.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    std::fs::write(root.join(DEFAULT_CONFIG_FILE_NAME), CONFIG).unwrap();

    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let reqs = result.requirements_md.expect("requirements_md present");
    assert_eq!(reqs.files, 0, "README.md is skipped");
    assert_eq!(reqs.requirements, 0);

    let store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    assert!(store
        .list_nodes_by_kind(NodeKind::Requirement)
        .unwrap()
        .is_empty());
}
