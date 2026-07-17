//! Breadth-backend (tree-sitter) golden for the six fixtures added in
//! issues.md #238 — csharp / ruby / php / kotlin / cpp / c. Before this,
//! wave-3 coverage lived as tiny inline strings (~30–200 bytes) in
//! `p22_treesitter_multilang`, too small to exercise real cross-file
//! structure or the heuristic call resolver. Each fixture here is a small
//! but real sample repo (multiple files, cross-file references, same-file
//! call edges) copied into a temp repo, indexed through the real
//! `index_repository` pass, and asserted on key nodes + Calls edges.
//!
//! The C# fixture additionally carries a LINQ `query_expression` and a
//! two-file `partial class`; the cross-file Calls edge that joins them is the
//! subject of issues.md #125, asserted below in
//! `csharp_linq_query_expression_and_partial_class_merge_resolve_helper`.

use std::fs;
use std::path::{Path, PathBuf};

use groundgraph_core::{EdgeKind, NodeKind};
use groundgraph_engine::index::index_repository;
use groundgraph_engine::{IndexOptions, IndexResult};
use groundgraph_store::Store;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn copy_fixture(name: &str, dst: &Path) {
    let src = workspace_root().join("tests/fixtures").join(name);
    for entry in walkdir::WalkDir::new(&src) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&src).unwrap();
        let target = dst.join(rel);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(entry.path(), &target).unwrap();
    }
}

/// Enable only the unified tree-sitter backend for `langs` and scan the whole
/// repo (`paths: [.]`) so whatever sub-layout a fixture uses (App/, lib/,
/// src/, flat) is indexed.
fn enable_treesitter(root: &Path, langs: &str) {
    fs::create_dir_all(root.join(".groundgraph")).unwrap();
    fs::write(
        root.join(".groundgraph.yaml"),
        format!("treesitter:\n  enabled: true\n  languages: [{langs}]\n  paths: [.]\n"),
    )
    .unwrap();
}

fn index(root: &Path) -> IndexResult {
    index_repository(IndexOptions::all(root)).expect("index must succeed")
}

fn opened_store(root: &Path) -> Store {
    let mut store = Store::open(root.join(".groundgraph/graph.db")).unwrap();
    store.migrate().unwrap();
    store
}

fn edge_debug(edges: &[groundgraph_core::EdgeAssertion]) -> String {
    format!(
        "{:?}",
        edges
            .iter()
            .map(|e| (e.kind, e.from_id.as_str(), e.to_id.as_str()))
            .collect::<Vec<_>>()
    )
}

fn has(nodes: &[groundgraph_core::Node], kind: NodeKind, name: &str) -> bool {
    nodes
        .iter()
        .any(|n| n.kind == kind && n.name.as_deref() == Some(name))
}

#[test]
fn csharp_fixture_indexes_partial_class_methods_and_same_file_call() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture("csharp_hello", root);
    enable_treesitter(root, "csharp");
    let result = index(root);
    assert!(
        result
            .treesitter
            .iter()
            .any(|r| r.language == "csharp" && r.files >= 3),
        "csharp must index >=3 files: {:?}",
        result.treesitter
    );

    let store = opened_store(root);
    let nodes = store.list_all_nodes().unwrap();
    assert!(
        has(&nodes, NodeKind::CSharpClass, "Greeter"),
        "Greeter class"
    );
    assert!(has(&nodes, NodeKind::CSharpClass, "Item"), "Item class");
    assert!(has(&nodes, NodeKind::CSharpMethod, "RenderActive"));
    assert!(
        has(&nodes, NodeKind::CSharpMethod, "helper"),
        "partial companion method `helper` must be indexed"
    );

    let edges = store.list_all_edges().unwrap();
    // Same-file call edge RenderActive -> JoinWith, independent of the
    // partial-class companion — proves the heuristic call resolver is wired
    // for C#. The cross-file partial edge RenderActive -> helper is the
    // subject of issues.md #125.
    assert!(
        edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.from_id.as_str().contains("RenderActive")
            && e.to_id.as_str().contains("JoinWith")),
        "RenderActive -> JoinWith call edge missing: {}",
        edge_debug(&edges)
    );
}

#[test]
fn ruby_fixture_indexes_module_class_methods_and_same_file_call() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture("ruby_hello", root);
    enable_treesitter(root, "ruby");
    index(root);

    let store = opened_store(root);
    let nodes = store.list_all_nodes().unwrap();
    assert!(has(&nodes, NodeKind::RubyModule, "Billing"));
    assert!(has(&nodes, NodeKind::RubyClass, "Invoice"));
    assert!(
        has(&nodes, NodeKind::RubyClass, "Receipt"),
        "cross-file class"
    );
    assert!(has(&nodes, NodeKind::RubyMethod, "total"));

    let edges = store.list_all_edges().unwrap();
    assert!(
        edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.from_id.as_str().contains("::total")
            && e.to_id.as_str().contains("::tax")),
        "Invoice::total -> Invoice::tax call edge missing: {}",
        edge_debug(&edges)
    );
}

#[test]
fn php_fixture_indexes_class_methods_function_and_same_file_call() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture("php_hello", root);
    enable_treesitter(root, "php");
    index(root);

    let store = opened_store(root);
    let nodes = store.list_all_nodes().unwrap();
    assert!(has(&nodes, NodeKind::PhpClass, "Greeter"));
    assert!(has(&nodes, NodeKind::PhpMethod, "greet"));
    assert!(has(&nodes, NodeKind::PhpMethod, "format"));
    assert!(
        has(&nodes, NodeKind::PhpFunction, "salutation"),
        "cross-file free function in helpers.php"
    );

    let edges = store.list_all_edges().unwrap();
    assert!(
        edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.from_id.as_str().contains("::greet")
            && e.to_id.as_str().contains("::format")),
        "Greeter::greet -> Greeter::format call edge missing: {}",
        edge_debug(&edges)
    );
}

#[test]
fn kotlin_fixture_indexes_class_object_methods_and_same_file_call() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture("kotlin_hello", root);
    enable_treesitter(root, "kotlin");
    index(root);

    let store = opened_store(root);
    let nodes = store.list_all_nodes().unwrap();
    assert!(has(&nodes, NodeKind::KotlinClass, "Greeter"));
    assert!(has(&nodes, NodeKind::KotlinMethod, "greet"));
    assert!(
        has(&nodes, NodeKind::KotlinObject, "Registry"),
        "cross-file object"
    );

    let edges = store.list_all_edges().unwrap();
    assert!(
        edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.from_id.as_str().contains("Greeter.greet")
            && e.to_id.as_str().contains("Greeter.format")),
        "Greeter.greet -> Greeter.format call edge missing: {}",
        edge_debug(&edges)
    );
}

#[test]
fn cpp_fixture_indexes_namespace_class_methods_and_same_file_call() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture("cpp_hello", root);
    enable_treesitter(root, "cpp");
    index(root);

    let store = opened_store(root);
    let nodes = store.list_all_nodes().unwrap();
    assert!(has(&nodes, NodeKind::CppNamespace, "eng"));
    assert!(has(&nodes, NodeKind::CppClass, "Engine"));
    assert!(has(&nodes, NodeKind::CppMethod, "power"));
    assert!(has(&nodes, NodeKind::CppMethod, "boost"));
    assert!(
        has(&nodes, NodeKind::CppFunction, "run"),
        "cross-file consumer"
    );

    let edges = store.list_all_edges().unwrap();
    assert!(
        edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.from_id.as_str().contains("::power")
            && e.to_id.as_str().contains("::boost")),
        "Engine::power -> Engine::boost call edge missing: {}",
        edge_debug(&edges)
    );
}

#[test]
fn c_fixture_indexes_struct_functions_and_same_file_call() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture("c_hello", root);
    enable_treesitter(root, "c");
    index(root);

    let store = opened_store(root);
    let nodes = store.list_all_nodes().unwrap();
    assert!(has(&nodes, NodeKind::CStruct, "Buffer"));
    assert!(has(&nodes, NodeKind::CFunction, "buffer_size"));
    assert!(has(&nodes, NodeKind::CFunction, "buffer_round"));

    let edges = store.list_all_edges().unwrap();
    assert!(
        edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.from_id.as_str().contains("buffer_size")
            && e.to_id.as_str().contains("buffer_round")),
        "buffer_size -> buffer_round call edge missing: {}",
        edge_debug(&edges)
    );
}

#[test]
fn csharp_linq_query_expression_and_partial_class_merge_resolve_helper() {
    // issues.md #125: `RenderActive` (Greeter.cs) calls `helper`, which is
    // (a) referenced inside a LINQ `query_expression` (`select helper(x)`)
    // and (b) defined in the partial-class companion Greeter.Part.cs. The
    // call must resolve to a Calls edge — proving both that the LINQ query
    // body's calls are captured and that the two partial halves merge so the
    // cross-file call links to the companion's method.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture("csharp_hello", root);
    enable_treesitter(root, "csharp");
    index(root);

    let store = opened_store(root);
    let edges = store.list_all_edges().unwrap();
    assert!(
        edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.from_id.as_str().contains("RenderActive")
            && e.to_id.as_str().contains("helper")),
        "RenderActive -> helper (LINQ capture + partial merge) missing: {}",
        edge_debug(&edges)
    );
}
