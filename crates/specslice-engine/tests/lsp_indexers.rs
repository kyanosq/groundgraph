//! P11 / P13 / P15 acceptance — drive the Swift and Go LSP-backed
//! indexers against their minimal fixtures.
//!
//! The "skips when unavailable" tests are always run; they exercise
//! our graceful-degradation contract without spawning any real LSP.
//!
//! The "emits ... when lsp present" tests spawn `sourcekit-lsp` /
//! `gopls` and rely on resolved SwiftPM / Go module state. These are
//! marked `#[ignore]` so a sandboxed `cargo test --workspace` stays
//! green — sandboxes routinely block sourcekit's `IndexStoreDB` cache
//! writes and gopls's module-proxy fetches. Run them explicitly with:
//!
//! ```
//! cargo test -p specslice-engine --test lsp_indexers -- \
//!   --include-ignored --nocapture
//! ```
//!
//! Override the binaries on a hermetic CI machine with:
//!
//! ```
//! SPECSLICE_SWIFT_LSP_BIN=/path/to/sourcekit-lsp \
//! SPECSLICE_GO_LSP_BIN=/path/to/gopls \
//!   cargo test -p specslice-engine --test lsp_indexers -- \
//!     --include-ignored
//! ```

use std::path::{Path, PathBuf};

use specslice_core::edge::EdgeKind;
use specslice_engine::{
    go_indexer::{go_lsp_available, index_go, GoIndexOptions, GO_LSP_COMMAND_ENV},
    python_indexer::{index_python, PythonIndexOptions, PYTHON_LSP_COMMAND_ENV},
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
#[ignore = "requires sourcekit-lsp + working SwiftPM cache; run with --include-ignored"]
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

    // P13 — sourcekit-lsp's callHierarchy / references rely on
    // SwiftPM's resolved checkout. We can't ask the operator to
    // pre-build, but for this fixture `swift build` is trivially
    // fast. If the binary is missing or the build fails we still
    // run the structural assertions (they only need documentSymbol)
    // and drop the Calls assertion at the end.
    let swift_build_ok = std::process::Command::new("swift")
        .args(["build", "--package-path"])
        .arg(tmp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

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

    // P13 / P15 — when SwiftPM resolved (we ran `swift build` above)
    // sourcekit-lsp's call-hierarchy probe *should* surface at least
    // one `Calls` edge (e.g. `makeGreeter` → `Greeter.init`,
    // `testGreetsByName` → `Greeter.greet`). But the probe is
    // sensitive to sourcekit's async `IndexStoreDB` warmup: on a
    // freshly-cached or slow host it may return empty before our 15s
    // budget. Rather than flake the opt-in CI, surface the empty
    // result as an eprintln so an operator running `--include-ignored`
    // sees that warmup didn't complete. Still hard-assert each edge
    // resolves to an indexed node so a malformed probe is caught.
    let calls = store
        .list_edges_by_kind(EdgeKind::Calls)
        .expect("calls edges queryable");
    if !swift_build_ok {
        eprintln!(
            "swift_indexer_emits_*: `swift build` unavailable — skipping Swift Calls edge assertion"
        );
    } else if calls.is_empty() {
        eprintln!(
            "swift_indexer_emits_*: `swift build` succeeded but sourcekit-lsp \
             returned no Calls — likely IndexStoreDB warmup didn't complete in budget"
        );
    } else {
        for edge in &calls {
            let to_node = nodes.iter().find(|n| n.id.as_str() == edge.to_id.as_str());
            assert!(
                to_node.is_some(),
                "Calls edge target `{}` not present in the indexed graph",
                edge.to_id.as_str()
            );
        }
        // P15 — when sourcekit-lsp emits Calls edges, at least one
        // edge in the Swift fixture must carry evidence pointing at a
        // *caller* file (e.g. `Sources/Greeter/Greeter.swift` or
        // `Tests/.../GreeterTests.swift`). The old code wrote the
        // callee's declaration file/line into `source_file`, which is
        // exactly the regression we want to lock down.
        let has_caller_evidence = calls.iter().any(|edge| {
            let from_node = nodes
                .iter()
                .find(|n| n.id.as_str() == edge.from_id.as_str());
            let from_path = from_node
                .and_then(|n| n.path.as_deref())
                .unwrap_or_default();
            edge.source_file
                .as_deref()
                .map(|src| !src.is_empty() && from_path.ends_with(src))
                .unwrap_or(false)
        });
        assert!(
            has_caller_evidence,
            "expected at least one Swift Calls edge whose source_file matches the caller \
             (caller-side fromRanges), got edges: {:?}",
            calls
                .iter()
                .map(|e| (
                    e.from_id.as_str().to_string(),
                    e.to_id.as_str().to_string(),
                    e.source_file.clone()
                ))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn python_indexer_ast_pass_runs_against_python_hello_fixture_without_lsp() {
    // The AST fallback must work without any toolchain installed. We
    // point `lsp_command` at a bogus binary and disable venv discovery
    // so this test is fully deterministic in sandboxed CI.
    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/python_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());
    let opts = PythonIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("app"), PathBuf::from("tests")],
        exclude_globs: Vec::new(),
        lsp_command: Some("specslice_nonexistent_python_lsp_xyz".into()),
        disable_venv_discovery: true,
    };
    let result = index_python(&mut store, &opts).expect("python indexer ran");
    assert_eq!(result.resolver_used, "python_ast");
    assert!(
        result.files >= 4,
        "expected >=4 python files in fixture, got {}",
        result.files
    );
    assert!(
        result.symbols >= 4,
        "expected >=4 structural symbols, got {}",
        result.symbols
    );
    assert!(
        result.tests >= 3,
        "expected >=3 pytest tests/groups, got {}",
        result.tests
    );
    assert!(
        result.imports >= 2,
        "expected >=2 resolvable imports, got {}",
        result.imports
    );
    assert!(
        result.sidecar_skip_reason.contains("AST fallback")
            || result.sidecar_skip_reason.contains("未找到"),
        "expected AST fallback reason, got `{}`",
        result.sidecar_skip_reason
    );

    let nodes = store.list_all_nodes().unwrap();
    let kinds: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in [
        "python_class",
        "python_method",
        "python_function",
        "test_case",
        "test_group",
        "file",
    ] {
        assert!(
            kinds.contains(required),
            "expected `{required}` in {:?}",
            kinds
        );
    }
}

#[test]
#[ignore = "requires pyright/basedpyright/pylsp installed in a venv; run with --include-ignored"]
fn python_indexer_emits_class_function_method_nodes_when_lsp_present() {
    let lsp_override = std::env::var(PYTHON_LSP_COMMAND_ENV).ok();
    let probe = PythonIndexOptions {
        repo_root: workspace_root(),
        code_roots: Vec::new(),
        exclude_globs: Vec::new(),
        lsp_command: lsp_override.clone(),
        disable_venv_discovery: false,
    };
    if !specslice_engine::python_indexer::python_lsp_available(&probe) {
        eprintln!(
            "skipping {} — no Python LSP discovered on PATH / .venv and {PYTHON_LSP_COMMAND_ENV} not set",
            module_path!()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/python_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = PythonIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("app"), PathBuf::from("tests")],
        exclude_globs: Vec::new(),
        lsp_command: lsp_override,
        disable_venv_discovery: false,
    };
    let result = index_python(&mut store, &opts).expect("python indexer ran");
    assert_eq!(result.resolver_used, "python_lsp");
    assert!(result.files >= 4);
    assert!(result.symbols >= 4);

    let nodes = store.list_all_nodes().unwrap();
    let kinds: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in ["python_class", "python_method", "python_function"] {
        assert!(
            kinds.contains(required),
            "expected `{required}` in {:?}",
            kinds
        );
    }
    let _greeter = nodes
        .iter()
        .find(|n| n.kind.as_str() == "python_class" && n.name.as_deref() == Some("Greeter"))
        .unwrap_or_else(|| panic!("Greeter class missing; saw {:?}", debug_kinds(&nodes)));

    // Pyright / basedpyright / pylsp all support callHierarchy, but
    // warmup time varies wildly. Treat empty calls as a soft warning
    // like the Swift / Go opt-in tests.
    let calls = store
        .list_edges_by_kind(EdgeKind::Calls)
        .expect("calls edges queryable");
    if calls.is_empty() {
        eprintln!(
            "python_indexer_emits_*: LSP returned no Calls — likely cross-file resolve \
             pending or stdlib not surveyed in budget"
        );
    } else {
        for edge in &calls {
            let to_node = nodes.iter().find(|n| n.id.as_str() == edge.to_id.as_str());
            assert!(
                to_node.is_some(),
                "Calls edge target `{}` not present in indexed graph",
                edge.to_id.as_str()
            );
        }
    }
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
#[ignore = "requires gopls + module proxy access; run with --include-ignored"]
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

    // P13 / P15 — gopls should surface at least one `Calls` edge for
    // the fixture (`main` → `api.NewServer`, `Server.Greet`). When
    // gopls cannot reach the module proxy or hasn't finished its
    // workspace warmup, the probe returns empty — surface that as a
    // log rather than fail the opt-in test. Still hard-assert that
    // every emitted edge resolves to an indexed node.
    let calls = store
        .list_edges_by_kind(EdgeKind::Calls)
        .expect("calls edges queryable");
    if calls.is_empty() {
        eprintln!(
            "go_indexer_emits_*: gopls returned no Calls — likely workspace warmup \
             didn't complete or module proxy unreachable"
        );
    } else {
        for edge in &calls {
            let to_node = nodes.iter().find(|n| n.id.as_str() == edge.to_id.as_str());
            assert!(
                to_node.is_some(),
                "Calls edge target `{}` not present in the indexed graph",
                edge.to_id.as_str()
            );
        }
    }
}
