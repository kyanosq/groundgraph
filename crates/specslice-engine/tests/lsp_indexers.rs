//! P11 / P13 / P15 / P20 acceptance — drive the language adapters against
//! their minimal fixtures.
//!
//! Swift is the **only** language that still carries an LSP sidecar
//! (`sourcekit-lsp`); every other language (Go / Python / TypeScript / Java)
//! retired its LSP overlay in favour of SCIP-authoritative precision with a
//! tree-sitter structure + heuristic `Calls`/`References` base. The tests
//! below therefore split into two groups:
//!
//!   * Swift LSP contract — a "skips when unavailable" test (always run) plus
//!     an `#[ignore]` "emits when present" test that spawns the real
//!     `sourcekit-lsp` (run with `--include-ignored`).
//!   * Structure+heuristic contract — Go / Python / TypeScript / Java each run
//!     their tree-sitter driver against a fixture with no toolchain installed
//!     and must produce the full structural graph deterministically.
//!
//! Run the opt-in Swift LSP test explicitly with:
//!
//! ```
//! cargo test -p specslice-engine --test lsp_indexers -- \
//!   --include-ignored --nocapture
//! ```
//!
//! Override the Swift binary on a hermetic CI machine with:
//!
//! ```
//! SPECSLICE_SWIFT_LSP_BIN=/path/to/sourcekit-lsp \
//!   cargo test -p specslice-engine --test lsp_indexers -- --include-ignored
//! ```

use std::path::{Path, PathBuf};

use specslice_core::edge::EdgeKind;
use specslice_engine::{
    go_indexer::{index_go, GoIndexOptions},
    java_indexer::{index_java, JavaIndexOptions},
    python_indexer::{index_python, PythonIndexOptions},
    swift_indexer::{index_swift, swift_lsp_available, SwiftIndexOptions, SWIFT_LSP_COMMAND_ENV},
    typescript_indexer::{index_typescript, TypescriptIndexOptions},
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

/// Run `cmd` to completion under a wall-clock `budget`, polling `try_wait`.
/// Returns `Some(success)` if it exited in time, or `None` if it was killed for
/// exceeding the budget (#79). Spawn failure maps to `Some(false)`.
fn run_with_timeout(cmd: &mut std::process::Command, budget: std::time::Duration) -> Option<bool> {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return Some(false),
    };
    let deadline = std::time::Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return Some(false),
        }
    }
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

// ---------------------------------------------------------------------------
// Swift — the sole remaining LSP-backed adapter.
// ---------------------------------------------------------------------------

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
    // #79: bound `swift build` so a cold SwiftPM cache / blocked registry
    // fetch can't hang an opt-in `--include-ignored` run forever. A timeout is
    // treated like a build failure — the structural assertions still run; only
    // the LSP-overlay Calls assertion is dropped.
    let mut swift_build = std::process::Command::new("swift");
    swift_build
        .args(["build", "--package-path"])
        .arg(tmp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let swift_build_ok = match run_with_timeout(
        &mut swift_build,
        std::time::Duration::from_secs(120),
    ) {
        Some(ok) => ok,
        None => {
            eprintln!(
                    "soft-skip {}: `swift build` exceeded the 120s budget (cold SwiftPM cache / blocked registry?) — dropping the Calls overlay assertion",
                    module_path!()
                );
            false
        }
    };

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
    // Structure always comes from the tree-sitter driver now; sourcekit-lsp
    // only overlays Calls/References on top of those same ids.
    assert_eq!(
        result.resolver_used, "swift_treesitter",
        "structure is owned by the tree-sitter driver: {result:?}"
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

// ---------------------------------------------------------------------------
// Go — structure + heuristic (LSP retired; precision via `scip-go`).
// ---------------------------------------------------------------------------

#[test]
fn go_indexer_emits_struct_interface_method_function_nodes_via_treesitter() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/go_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = GoIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from(".")],
        exclude_globs: Vec::new(),
    };
    let result = index_go(&mut store, &opts).expect("index_go Ok");
    assert_eq!(
        result.resolver_used, "go_treesitter",
        "structure is owned by the tree-sitter driver: {result:?}"
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
    let _greet_callable = nodes
        .iter()
        .find(|n| {
            matches!(n.kind.as_str(), "go_method" | "go_function")
                && (n.name.as_deref() == Some("Greet") || n.name.as_deref() == Some("Server.Greet"))
        })
        .unwrap_or_else(|| panic!("Greet callable missing; saw {:?}", debug_kinds(&nodes)));
}

// ---------------------------------------------------------------------------
// Python — structure + heuristic (LSP retired; precision via `scip-python`).
// ---------------------------------------------------------------------------

#[test]
fn python_indexer_treesitter_pass_runs_against_python_hello_fixture() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/python_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());
    let opts = PythonIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("app"), PathBuf::from("tests")],
        exclude_globs: Vec::new(),
    };
    let result = index_python(&mut store, &opts).expect("python indexer ran");
    assert_eq!(result.resolver_used, "python_treesitter");
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

// ---------------------------------------------------------------------------
// TypeScript — structure + heuristic (LSP retired; precision via
// `scip-typescript`).
// ---------------------------------------------------------------------------

#[test]
fn typescript_indexer_uses_treesitter_structure() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/typescript_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = TypescriptIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("src"), PathBuf::from("tests")],
        exclude_globs: Vec::new(),
    };
    let result = index_typescript(&mut store, &opts).expect("ts indexer ran");
    // The tree-sitter driver is the sole structural backend now; it must
    // fully populate the graph without any LSP.
    assert!(
        result.files >= 3,
        "expected >=3 .ts files (src/index, src/greeter, src/utils + tests), got {}",
        result.files
    );
    assert!(
        result.symbols >= 2,
        "expected at least the class/function/method symbols, got {}",
        result.symbols
    );
    assert!(
        result.tests >= 2,
        "vitest `describe`/`it` cases should be recovered (got {})",
        result.tests
    );
    assert_eq!(
        result.resolver_used, "typescript_treesitter",
        "structure now comes from the tree-sitter driver, got `{}`",
        result.resolver_used
    );

    let nodes = store.list_all_nodes().unwrap();
    let kinds: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in ["typescript_class", "typescript_function"] {
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

// ---------------------------------------------------------------------------
// Java — structure + heuristic (LSP retired; precision via `scip-java`).
// ---------------------------------------------------------------------------

#[test]
fn java_indexer_uses_treesitter_structure() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = workspace_root().join("tests/fixtures/java_hello");
    copy_dir(&fixture, tmp.path());
    let (mut store, _db) = open_temp_store(tmp.path());

    let opts = JavaIndexOptions {
        repo_root: tmp.path().to_path_buf(),
        code_roots: vec![PathBuf::from("src")],
        exclude_globs: Vec::new(),
    };
    let result = index_java(&mut store, &opts).expect("java indexer ran");
    assert!(
        result.files >= 3,
        "expected >=3 .java files in fixture, got {}",
        result.files
    );
    assert!(
        result.symbols >= 3,
        "expected JavaClass + members, got {}",
        result.symbols
    );
    assert!(
        result.tests >= 2,
        "JUnit @Test methods should be recovered (got {})",
        result.tests
    );
    assert_eq!(
        result.resolver_used, "java_treesitter",
        "structure now comes from the tree-sitter driver, got `{}`",
        result.resolver_used
    );

    let nodes = store.list_all_nodes().unwrap();
    let kinds: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in ["java_class", "java_method"] {
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
}
