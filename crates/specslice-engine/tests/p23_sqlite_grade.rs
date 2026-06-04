//! P23.8 — SQLite-grade end-to-end guarantees.
//!
//! 1. **Reindex idempotency through the real pipeline**: a multi-language repo
//!    driven by the unified P23.7 `languages:` config indexes to a *byte
//!    identical* graph on every run (nodes + edges + symbol ranges).
//! 2. **Corpus totality + determinism**: the structural backends never panic
//!    and produce identical graphs across two runs over real repositories —
//!    the SpecSlice repo itself (Rust) and external Dart apps when present.
//!    All corpus indexing writes to a throwaway temp store, so target repos
//!    are never modified (the non-invasive invariant).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use specslice_engine::dart_indexer::{index_dart, DartIndexOptions};
use specslice_engine::index::{index_repository, IndexOptions};
use specslice_engine::init::{init_repository, InitOptions};
use specslice_engine::treesitter::{self, spec_for_language, TsIndexOptions};
use specslice_store::Store;
use tempfile::TempDir;

/// Deterministic, generation-independent fingerprint of the whole graph:
/// every node, edge and symbol range rendered to a sorted multi-line string.
/// Excludes volatile columns (index generation, hashes) so the comparison
/// reflects *structure*, not bookkeeping.
fn snapshot(store: &Store) -> String {
    let mut lines: Vec<String> = Vec::new();
    let nodes = store.list_all_nodes().expect("list nodes");
    for n in &nodes {
        lines.push(format!(
            "N {}|{:?}|{:?}|{:?}|{:?}|{:?}",
            n.id, n.kind, n.name, n.path, n.start_line, n.end_line
        ));
    }
    for e in store.list_all_edges().expect("list edges") {
        lines.push(format!(
            "E {}|{:?}|{}|{:?}|{:?}",
            e.from_id, e.kind, e.to_id, e.certainty, e.status
        ));
    }
    let files: BTreeSet<String> = nodes.iter().filter_map(|n| n.path.clone()).collect();
    for f in &files {
        for r in store.list_symbol_ranges_for_file(f).expect("ranges") {
            lines.push(format!(
                "R {}|{}|{}|{}|{:?}",
                r.file_path, r.symbol_id, r.start_line, r.end_line, r.symbol_kind
            ));
        }
    }
    lines.sort();
    lines.join("\n")
}

fn fresh_store() -> (TempDir, Store) {
    let tmp = TempDir::new().expect("tempdir");
    let mut store = Store::open(tmp.path().join("graph.db")).expect("open");
    store.migrate().expect("migrate");
    (tmp, store)
}

/// Index one tree-sitter language over `roots` of `repo_root` into a fresh
/// throwaway store and return its snapshot. Reads target files only; never
/// writes inside `repo_root`.
fn snapshot_generic(repo_root: &Path, language: &str, roots: &[&str], exclude: &[&str]) -> String {
    let (_tmp, mut store) = fresh_store();
    let spec = spec_for_language(language).expect("known language");
    treesitter::index_repo_with_spec(
        &mut store,
        spec,
        &TsIndexOptions {
            repo_root: repo_root.to_path_buf(),
            code_roots: roots.iter().map(PathBuf::from).collect(),
            exclude_globs: exclude.iter().map(|s| s.to_string()).collect(),
            resolution_paths: Vec::new(),
        },
    )
    .expect("index generic language");
    snapshot(&store)
}

/// Structure-only Dart index (analyzer overlay disabled → no Dart SDK needed)
/// into a throwaway store. Non-invasive.
fn snapshot_dart_structure(repo_root: &Path) -> String {
    let (_tmp, mut store) = fresh_store();
    index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: repo_root.to_path_buf(),
            code_roots: vec!["lib".into(), "test".into()],
            exclude_globs: vec![
                "**/*.g.dart".into(),
                "**/*.freezed.dart".into(),
                "**/build/**".into(),
                "**/.dart_tool/**".into(),
            ],
            disable_analyzer: true,
        },
    )
    .expect("index dart structure");
    snapshot(&store)
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR == <root>/crates/specslice-engine
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn p23_full_pipeline_reindex_is_idempotent_via_unified_config() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();

    // A four-language fixture.
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(
        root.join("lib/a.dart"),
        "class DartKeep {\n  void run() {}\n}\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("crates/r/src")).unwrap();
    std::fs::write(
        root.join("crates/r/src/lib.rs"),
        "pub fn rust_alpha() -> i32 { 1 }\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("py")).unwrap();
    std::fs::write(root.join("py/m.py"), "def py_beta():\n    return 2\n").unwrap();
    std::fs::create_dir_all(root.join("native")).unwrap();
    std::fs::write(root.join("native/x.c"), "int c_gamma(void) { return 3; }\n").unwrap();

    // Unified P23.7 selector. `enrichment` is off so the test is hermetic:
    // Dart indexes structure-only (no SDK), Python routes to the generic
    // tree-sitter driver (no LSP), Rust/C are always tree-sitter.
    std::fs::write(
        root.join(".specslice.yaml"),
        concat!(
            "repo:\n  root: .\n  default_branch: main\n",
            "storage:\n  path: .specslice/graph.db\n",
            "docs:\n  paths: []\n",
            "languages:\n",
            "  - id: dart\n    paths: [lib]\n",
            "  - id: rust\n    paths: [crates]\n",
            "  - id: python\n    paths: [py]\n",
            "  - id: c\n    paths: [native]\n",
            "enrichment:\n  lsp: false\n  analyzer: false\n",
        ),
    )
    .unwrap();

    let db = root.join(".specslice/graph.db");
    index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let first = snapshot(&Store::open(&db).unwrap());
    index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let second = snapshot(&Store::open(&db).unwrap());

    assert_eq!(
        first, second,
        "re-indexing unchanged sources must yield a byte-identical graph"
    );
    assert!(!first.is_empty(), "graph must not be empty");
    // Every language contributed a symbol.
    for needle in ["DartKeep", "rust_alpha", "py_beta", "c_gamma"] {
        assert!(
            first.contains(needle),
            "expected `{needle}` in graph snapshot:\n{first}"
        );
    }
}

#[test]
fn p23_corpus_self_repo_rust_is_total_and_deterministic() {
    let root = workspace_root();
    let exclude = ["**/target/**"];
    let first = snapshot_generic(&root, "rust", &["crates"], &exclude);
    let second = snapshot_generic(&root, "rust", &["crates"], &exclude);
    assert!(
        !first.is_empty(),
        "indexing the SpecSlice Rust sources should produce a graph"
    );
    assert_eq!(
        first, second,
        "two structural passes over the same Rust sources must be identical"
    );
}

#[test]
#[ignore = "heavy: indexes external corpus repos twice; run with `--ignored`"]
fn p23_corpus_external_dart_repos_are_total_and_deterministic() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let home = PathBuf::from(home);
    let candidates = [home.join("Code/Game/Morse"), home.join("Code/My/Penlly")];
    let mut indexed_any = false;
    for repo in candidates {
        if !repo.join("lib").is_dir() {
            continue; // repo absent on this host — skip, never fail.
        }
        indexed_any = true;
        let first = snapshot_dart_structure(&repo);
        let second = snapshot_dart_structure(&repo);
        assert!(
            !first.is_empty(),
            "Dart corpus {} produced an empty graph",
            repo.display()
        );
        assert_eq!(
            first,
            second,
            "Dart structural pass over {} must be deterministic",
            repo.display()
        );
    }
    eprintln!("external Dart corpus indexed: {indexed_any}");
}
