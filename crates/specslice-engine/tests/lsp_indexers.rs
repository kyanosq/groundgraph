//! P11 acceptance — drive the Swift and Go LSP-backed indexers against
//! their minimal fixtures. Both tests are env-gated: when the language
//! server is not on PATH (and no override is set) we still verify the
//! graceful-skip contract so the suite is green on machines without
//! Swift / Go toolchains.
//!
//! Run the full LSP path with:
//!
//! ```
//! cargo test -p specslice-engine --test lsp_indexers -- --nocapture
//! ```
//!
//! Override the binaries on a hermetic CI machine with:
//!
//! ```
//! SPECSLICE_SWIFT_LSP_BIN=/path/to/sourcekit-lsp \
//! SPECSLICE_GO_LSP_BIN=/path/to/gopls \
//!   cargo test -p specslice-engine --test lsp_indexers
//! ```

use std::path::{Path, PathBuf};

use specslice_engine::{
    go_indexer::{go_lsp_available, index_go, GoIndexOptions, GO_LSP_COMMAND_ENV},
    swift_indexer::{index_swift, swift_lsp_available, SwiftIndexOptions, SWIFT_LSP_COMMAND_ENV},
};
use specslice_store::Store;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn open_temp_store(repo: &Path) -> (Store, PathBuf) {
    let db = repo.join(".specslice/graph.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let mut store = Store::open(&db).unwrap();
    store.migrate().unwrap();
    (store, db)
}

#[test]
fn swift_indexer_skips_when_sourcekit_lsp_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = SwiftIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("Sources")],
        exclude_globs: Vec::new(),
        lsp_command: Some("specslice_nonexistent_swift_lsp_xyz".into()),
    };
    let (mut store, _db) = open_temp_store(tmp.path());
    let result = index_swift(&mut store, &opts).expect("index_swift Ok");
    assert_eq!(result.files, 0);
    assert_eq!(result.symbols, 0);
    assert!(result.resolver_used.is_empty());
    assert!(
        result.sidecar_skip_reason.contains("PATH"),
        "expected PATH hint in skip reason, got `{}`",
        result.sidecar_skip_reason
    );
}

#[test]
fn go_indexer_skips_when_gopls_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = GoIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from(".")],
        exclude_globs: Vec::new(),
        lsp_command: Some("specslice_nonexistent_gopls_xyz".into()),
    };
    let (mut store, _db) = open_temp_store(tmp.path());
    let result = index_go(&mut store, &opts).expect("index_go Ok");
    assert_eq!(result.files, 0);
    assert_eq!(result.symbols, 0);
    assert!(result.resolver_used.is_empty());
    assert!(
        result.sidecar_skip_reason.contains("PATH"),
        "expected PATH hint, got `{}`",
        result.sidecar_skip_reason
    );
}

#[test]
fn swift_indexer_emits_class_struct_protocol_method_nodes_when_lsp_present() {
    let lsp_override = std::env::var(SWIFT_LSP_COMMAND_ENV).ok();
    let probe = SwiftIndexOptions {
        repo_root: workspace_root(),
        code_roots: Vec::new(),
        exclude_globs: Vec::new(),
        lsp_command: lsp_override.clone(),
    };
    if !swift_lsp_available(&probe) {
        eprintln!(
            "skipping {} — `sourcekit-lsp` not on PATH and {SWIFT_LSP_COMMAND_ENV} not set",
            module_path!()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/swift_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = SwiftIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("Sources"), PathBuf::from("Tests")],
        exclude_globs: Vec::new(),
        lsp_command: lsp_override,
    };
    let result = index_swift(&mut store, &opts).expect("Swift indexer ran");
    assert!(
        result.sidecar_skip_reason.is_empty()
            || result.sidecar_skip_reason.starts_with("LSP shutdown 警告"),
        "unexpected skip reason: {}",
        result.sidecar_skip_reason
    );
    assert_eq!(result.resolver_used, "swift_lsp");
    assert!(
        result.files >= 2,
        "expected at least 2 swift files, got {}",
        result.files
    );
    assert!(
        result.symbols >= 5,
        "expected >=5 symbols, got {}",
        result.symbols
    );

    let nodes = store.list_all_nodes().unwrap();
    let mut kinds: std::collections::BTreeSet<&str> =
        nodes.iter().map(|n| n.kind.as_str()).collect();
    assert!(kinds.contains("file"), "missing file kind: {kinds:?}");
    // Every Swift fixture symbol must be present so the operator gets a
    // recognisable graph immediately after enabling the adapter.
    for required in [
        "swift_class",
        "swift_struct",
        "swift_protocol",
        "swift_method",
        "swift_function",
    ] {
        assert!(
            kinds.contains(required),
            "expected `{required}` in {:?}",
            kinds
        );
    }
    kinds.clear(); // silence unused-mut lint after assertions.
    let _ = kinds;

    let _greeter_class = nodes
        .iter()
        .find(|n| n.kind.as_str() == "swift_class" && n.name.as_deref() == Some("Greeter"))
        .unwrap_or_else(|| panic!("Greeter class node missing; saw: {:?}", debug_kinds(&nodes)));
    // Some sourcekit-lsp builds report Swift methods as `function` rather
    // than `method` (per the upstream `SymbolKindExtensions.swift`
    // mapping). Accept either when verifying that the Greeter class has
    // a `greet` member.
    // `sourcekit-lsp` includes the call syntax in symbol names
    // (`greet()` rather than `greet`), so we match by prefix.
    let _greet_callable = nodes
        .iter()
        .find(|n| {
            matches!(n.kind.as_str(), "swift_method" | "swift_function")
                && n.name
                    .as_deref()
                    .is_some_and(|n| n == "greet" || n.starts_with("greet("))
        })
        .unwrap_or_else(|| panic!("greet callable missing; saw: {:?}", debug_kinds(&nodes)));
    // Likewise verify the initialiser landed under the right NodeKind.
    let _init_callable = nodes
        .iter()
        .find(|n| n.kind.as_str() == "swift_initializer")
        .unwrap_or_else(|| panic!("Swift initialiser missing; saw {:?}", debug_kinds(&nodes)));
}

fn debug_kinds(nodes: &[specslice_core::Node]) -> Vec<(String, String)> {
    nodes
        .iter()
        .map(|n| {
            (
                n.kind.as_str().to_string(),
                n.name.clone().unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn go_indexer_emits_struct_interface_method_function_nodes_when_lsp_present() {
    let lsp_override = std::env::var(GO_LSP_COMMAND_ENV).ok();
    let probe = GoIndexOptions {
        repo_root: workspace_root(),
        code_roots: Vec::new(),
        exclude_globs: Vec::new(),
        lsp_command: lsp_override.clone(),
    };
    if !go_lsp_available(&probe) {
        eprintln!(
            "skipping {} — `gopls` not on PATH and {GO_LSP_COMMAND_ENV} not set",
            module_path!()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/go_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = GoIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from(".")],
        exclude_globs: Vec::new(),
        lsp_command: lsp_override,
    };
    let result = index_go(&mut store, &opts).expect("Go indexer ran");
    assert!(
        result.sidecar_skip_reason.is_empty()
            || result.sidecar_skip_reason.starts_with("LSP shutdown 警告"),
        "unexpected skip reason: {}",
        result.sidecar_skip_reason
    );
    assert_eq!(result.resolver_used, "go_lsp");
    assert!(
        result.files >= 2,
        "expected >=2 go files, got {}",
        result.files
    );
    assert!(
        result.symbols >= 4,
        "expected >=4 symbols, got {}",
        result.symbols
    );

    let nodes = store.list_all_nodes().unwrap();
    let kinds: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in ["go_struct", "go_interface", "go_method", "go_function"] {
        assert!(
            kinds.contains(required),
            "expected `{required}` in {:?}",
            kinds
        );
    }

    let _server_struct = nodes
        .iter()
        .find(|n| n.kind.as_str() == "go_struct" && n.name.as_deref() == Some("Server"))
        .unwrap_or_else(|| panic!("Server struct missing; saw {:?}", debug_kinds(&nodes)));
    // `gopls` reports method symbols in two flavours depending on the
    // version: a flat `Server.Greet` (kind=Method) and a nested `Greet`
    // under `Server`. Either is acceptable for the structural pass.
    let _greet_callable = nodes
        .iter()
        .find(|n| {
            matches!(n.kind.as_str(), "go_method" | "go_function")
                && (n.name.as_deref() == Some("Greet") || n.name.as_deref() == Some("Server.Greet"))
        })
        .unwrap_or_else(|| panic!("Greet callable missing; saw {:?}", debug_kinds(&nodes)));
}
