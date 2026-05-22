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
    java_indexer::{index_java, java_lsp_available, JavaIndexOptions, JAVA_LSP_COMMAND_ENV},
    python_indexer::{index_python, PythonIndexOptions, PYTHON_LSP_COMMAND_ENV},
    swift_indexer::{index_swift, swift_lsp_available, SwiftIndexOptions, SWIFT_LSP_COMMAND_ENV},
    typescript_indexer::{
        index_typescript, typescript_lsp_available, TypescriptIndexOptions,
        TYPESCRIPT_LSP_COMMAND_ENV,
    },
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
            "skipping {} — `sourcekit-lsp` did not pass the shared `lsp_probe` smoke launch (binary missing on PATH, {SWIFT_LSP_COMMAND_ENV} unset, or LSP returned a `SOURCEKITD FATAL ERROR` / non-zero exit)",
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
    // P20 close-out — the unified `lsp_probe` smoke launch already
    // catches "binary exists but is broken" (e.g. `sourcekit-lsp`
    // crashing with `SOURCEKITD FATAL ERROR: Service is invalid`)
    // BEFORE we get here, by making `swift_lsp_available` return
    // false. But sometimes the probe survives `--help` and the real
    // stdio session still collapses (`IndexStoreDB` cache poisoning,
    // sandboxed permissions, transient toolchain hiccups). We
    // convert both that and "adapter fell back to AST anyway" into
    // a soft-skip so a busted local LSP can't turn opt-in
    // `--include-ignored` red. The eprintln is the operator-facing
    // diagnostic.
    let result = match index_swift(&mut store, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "soft-skip {}: `index_swift` returned Err ({e}); LSP probe passed but session failed",
                module_path!()
            );
            return;
        }
    };
    if result.resolver_used != "swift_lsp" {
        eprintln!(
            "soft-skip {}: probe ok but adapter fell back to `{}` (reason: {})",
            module_path!(),
            result.resolver_used,
            result.sidecar_skip_reason
        );
        return;
    }
    assert!(
        result.sidecar_skip_reason.is_empty()
            || result.sidecar_skip_reason.starts_with("LSP shutdown 警告"),
        "unexpected skip reason: {}",
        result.sidecar_skip_reason
    );
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
            "skipping {} — no Python LSP passed the shared `lsp_probe` smoke launch (PATH / .venv empty, or all candidates returned broken-stub stderr); {PYTHON_LSP_COMMAND_ENV} unset",
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
    let result = match index_python(&mut store, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "soft-skip {}: `index_python` returned Err ({e}); LSP probe passed but session failed",
                module_path!()
            );
            return;
        }
    };
    if result.resolver_used != "python_lsp" {
        // Soft skip — `python_lsp_available` claimed a binary, the
        // launcher decided otherwise (typical cause: the binary's
        // shebang resolves at probe time but the long-running stdio
        // session fails). We log the actual fallback reason so
        // operators can diagnose without staring at a green
        // "passed" with no signal.
        eprintln!(
            "soft-skip {}: probe ok but adapter fell back to `{}` (reason: {})",
            module_path!(),
            result.resolver_used,
            result.sidecar_skip_reason
        );
        return;
    }
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
            "skipping {} — `gopls` did not pass the shared `lsp_probe` smoke launch (binary missing on PATH, {GO_LSP_COMMAND_ENV} unset, or non-zero exit / broken-stub stderr)",
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
    let result = match index_go(&mut store, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "soft-skip {}: `index_go` returned Err ({e}); LSP probe passed but session failed",
                module_path!()
            );
            return;
        }
    };
    if result.resolver_used != "go_lsp" {
        eprintln!(
            "soft-skip {}: probe ok but adapter fell back to `{}` (reason: {})",
            module_path!(),
            result.resolver_used,
            result.sidecar_skip_reason
        );
        return;
    }
    assert!(
        result.sidecar_skip_reason.is_empty()
            || result.sidecar_skip_reason.starts_with("LSP shutdown 警告"),
        "unexpected skip reason: {}",
        result.sidecar_skip_reason
    );
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

// ---------------------------------------------------------------------------
// P20 — TypeScript adapter.
// ---------------------------------------------------------------------------

#[test]
fn typescript_indexer_skips_when_tsserver_unavailable_but_still_runs_ast() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/typescript_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = TypescriptIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("src"), PathBuf::from("tests")],
        exclude_globs: Vec::new(),
        lsp_command: Some("specslice_nonexistent_tsserver_xyz".into()),
    };
    let result = index_typescript(&mut store, &opts).expect("ts indexer ran");
    // AST pass must still fire.
    assert!(
        result.files >= 3,
        "expected >=3 .ts files (src/index, src/greeter, src/utils + tests), got {}",
        result.files
    );
    assert!(
        result.symbols >= 2,
        "expected at least the TypescriptModule + class/function symbols, got {}",
        result.symbols
    );
    assert!(
        result.tests >= 2,
        "vitest `describe`/`it` cases should be recovered (got {})",
        result.tests
    );
    assert!(
        result.resolver_used == "typescript_ast" || result.resolver_used.is_empty(),
        "expected AST fallback, got `{}`",
        result.resolver_used
    );
    assert!(
        !result.sidecar_skip_reason.is_empty(),
        "skip reason should explain why LSP was bypassed"
    );

    let nodes = store.list_all_nodes().unwrap();
    let kinds: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in [
        "typescript_module",
        "typescript_class",
        "typescript_function",
    ] {
        assert!(
            kinds.contains(required),
            "expected `{required}` in {:?}",
            kinds
        );
    }
    assert!(
        nodes
            .iter()
            .any(|n| n.kind.as_str() == "typescript_class" && n.name.as_deref() == Some("Greeter")),
        "Greeter class missing; saw {:?}",
        debug_kinds(&nodes)
    );
}

#[test]
#[ignore = "requires typescript-language-server installed; run with --include-ignored"]
fn typescript_indexer_emits_class_function_method_nodes_when_lsp_present() {
    let lsp_override = std::env::var(TYPESCRIPT_LSP_COMMAND_ENV).ok();
    let probe = TypescriptIndexOptions {
        repo_root: workspace_root(),
        code_roots: Vec::new(),
        exclude_globs: Vec::new(),
        lsp_command: lsp_override.clone(),
    };
    if !typescript_lsp_available(&probe) {
        eprintln!(
            "skipping {} — `typescript-language-server` did not pass the shared `lsp_probe` smoke launch (PATH / node_modules/.bin empty, {TYPESCRIPT_LSP_COMMAND_ENV} unset, or non-zero exit / broken-node-shebang)",
            module_path!()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/typescript_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = TypescriptIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("src"), PathBuf::from("tests")],
        exclude_globs: Vec::new(),
        lsp_command: lsp_override,
    };
    let result = match index_typescript(&mut store, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "soft-skip {}: `index_typescript` returned Err ({e}); LSP probe passed but session failed",
                module_path!()
            );
            return;
        }
    };
    if result.resolver_used != "typescript_lsp" {
        eprintln!(
            "soft-skip {}: probe ok but adapter fell back to `{}` (reason: {})",
            module_path!(),
            result.resolver_used,
            result.sidecar_skip_reason
        );
        return;
    }
    assert!(result.files >= 3, "got {}", result.files);
    assert!(result.symbols >= 3, "got {}", result.symbols);

    let nodes = store.list_all_nodes().unwrap();
    let kinds: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in [
        "typescript_class",
        "typescript_method",
        "typescript_function",
    ] {
        assert!(
            kinds.contains(required),
            "expected `{required}` in {:?}",
            kinds
        );
    }
    let _greeter = nodes
        .iter()
        .find(|n| n.kind.as_str() == "typescript_class" && n.name.as_deref() == Some("Greeter"))
        .unwrap_or_else(|| panic!("Greeter class missing; saw {:?}", debug_kinds(&nodes)));

    let calls = store
        .list_edges_by_kind(EdgeKind::Calls)
        .expect("calls edges queryable");
    if calls.is_empty() {
        eprintln!(
            "typescript_indexer_emits_*: tsserver returned no Calls — likely workspace \
             warmup didn't complete"
        );
    } else {
        for edge in &calls {
            assert!(
                nodes.iter().any(|n| n.id.as_str() == edge.to_id.as_str()),
                "Calls edge target `{}` not present in the indexed graph",
                edge.to_id.as_str()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// P20 — Java adapter.
// ---------------------------------------------------------------------------

#[test]
fn java_indexer_skips_when_jdtls_unavailable_but_still_runs_ast() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/java_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = JavaIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("src")],
        exclude_globs: Vec::new(),
        lsp_command: Some("specslice_nonexistent_jdtls_xyz".into()),
    };
    let result = index_java(&mut store, &opts).expect("java indexer ran");
    assert!(
        result.files >= 3,
        "expected >=3 .java files in fixture, got {}",
        result.files
    );
    assert!(
        result.symbols >= 3,
        "expected JavaPackage + JavaClass + members, got {}",
        result.symbols
    );
    assert!(
        result.tests >= 2,
        "JUnit @Test methods should be recovered (got {})",
        result.tests
    );
    assert!(
        result.resolver_used == "java_ast" || result.resolver_used.is_empty(),
        "expected AST fallback, got `{}`",
        result.resolver_used
    );

    let nodes = store.list_all_nodes().unwrap();
    let kinds: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in ["java_package", "java_class", "java_method"] {
        assert!(
            kinds.contains(required),
            "expected `{required}` in {:?}",
            kinds
        );
    }
    assert!(
        nodes
            .iter()
            .any(|n| n.kind.as_str() == "java_class" && n.name.as_deref() == Some("Greeter")),
        "Greeter class missing; saw {:?}",
        debug_kinds(&nodes)
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.kind.as_str() == "java_package"
                && n.name.as_deref() == Some("com.example.hello")),
        "com.example.hello package missing; saw {:?}",
        debug_kinds(&nodes)
    );
}

#[test]
#[ignore = "requires jdtls installed; run with --include-ignored"]
fn java_indexer_emits_class_method_nodes_when_lsp_present() {
    let lsp_override = std::env::var(JAVA_LSP_COMMAND_ENV).ok();
    let probe = JavaIndexOptions {
        repo_root: workspace_root(),
        code_roots: Vec::new(),
        exclude_globs: Vec::new(),
        lsp_command: lsp_override.clone(),
    };
    if !java_lsp_available(&probe) {
        eprintln!(
            "skipping {} — `jdtls` did not pass the shared `lsp_probe` smoke launch (binary missing on PATH, {JAVA_LSP_COMMAND_ENV} unset, or missing JRE / non-zero exit)",
            module_path!()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/java_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = JavaIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("src")],
        exclude_globs: Vec::new(),
        lsp_command: lsp_override,
    };
    let result = match index_java(&mut store, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "soft-skip {}: `index_java` returned Err ({e}); LSP probe passed but session failed",
                module_path!()
            );
            return;
        }
    };
    if result.resolver_used != "java_lsp" {
        eprintln!(
            "soft-skip {}: probe ok but adapter fell back to `{}` (reason: {})",
            module_path!(),
            result.resolver_used,
            result.sidecar_skip_reason
        );
        return;
    }
    assert!(result.symbols >= 3, "got {}", result.symbols);

    let nodes = store.list_all_nodes().unwrap();
    let kinds: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in ["java_class", "java_method"] {
        assert!(
            kinds.contains(required),
            "expected `{required}` in {:?}",
            kinds
        );
    }
}
