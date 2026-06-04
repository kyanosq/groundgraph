//! P21 self-hosting proof.
//!
//! The LSP-first adapters could never index SpecSlice's own Rust sources
//! — the most embarrassing gap surfaced by the CodeGraph benchmark. This
//! test pins the fix permanently: the in-process tree-sitter backend
//! indexes the *actual* `crates/` tree of this very workspace and must
//! recover a meaningful graph, including symbols this file's sibling
//! modules define. If self-hosting ever regresses, CI goes red.

use std::path::{Path, PathBuf};

use specslice_core::NodeKind;
use specslice_engine::{index_rust, RustIndexOptions, RUST_INDEXER_NAME};
use specslice_store::Store;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/crates/specslice-engine
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root is two levels above the engine crate")
        .to_path_buf()
}

#[test]
fn specslice_indexes_its_own_rust_workspace() {
    let root = workspace_root();
    assert!(
        root.join("crates").join("specslice-engine").exists(),
        "expected to run from the SpecSlice workspace, got {}",
        root.display()
    );

    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();

    let result = index_rust(
        &mut store,
        &RustIndexOptions {
            repo_root: root.clone(),
            code_roots: vec![PathBuf::from("crates")],
            exclude_globs: vec![],
        },
    )
    .expect("self-indexing must not error");

    // The workspace has six crates with many source files; these are
    // deliberately loose lower bounds so the test survives normal growth
    // and refactors without becoming brittle.
    assert!(
        result.files >= 20,
        "expected to index a substantial number of .rs files, got {}",
        result.files
    );
    assert!(
        result.symbols >= 300,
        "expected a rich symbol graph, got {}",
        result.symbols
    );
    assert_eq!(result.resolver_used, RUST_INDEXER_NAME);

    let nodes = store.list_all_nodes().unwrap();
    let has = |kind: NodeKind, name: &str| {
        nodes
            .iter()
            .any(|n| n.kind == kind && n.name.as_deref() == Some(name))
    };

    // Symbols defined by the P21 implementation itself — proof that the
    // backend parsed real, current source rather than a stale fixture.
    assert!(
        has(NodeKind::RustFunction, "index_rust"),
        "missing free fn `index_rust`"
    );
    assert!(
        has(NodeKind::RustFunction, "scan"),
        "missing free fn `scan`"
    );
    assert!(
        has(NodeKind::RustStruct, "LangSpec"),
        "missing struct `LangSpec` (the P22 generic driver spec)"
    );
    assert!(
        has(NodeKind::RustStruct, "RustIndexResult"),
        "missing struct `RustIndexResult`"
    );

    // Every structural family must be represented across the workspace.
    let count = |kind: NodeKind| nodes.iter().filter(|n| n.kind == kind).count();
    assert!(count(NodeKind::RustStruct) >= 10, "too few structs");
    assert!(count(NodeKind::RustEnum) >= 3, "too few enums");
    assert!(count(NodeKind::RustTrait) >= 1, "too few traits");
    assert!(count(NodeKind::RustMethod) >= 50, "too few methods");
    assert!(count(NodeKind::RustFunction) >= 50, "too few functions");
}
