//! P21 — Rust adapter: thin wrapper over the generic tree-sitter driver.
//!
//! Kept as a named entry point (`index_rust`) for the engine pass, the
//! self-host integration test, and output parity with the LSP-backed
//! adapters. All real work is in [`crate::treesitter`].

use std::path::PathBuf;

use anyhow::Result;
use groundgraph_store::Store;

use crate::rust_treesitter::RUST_SPEC;
use crate::treesitter::{index_repo_with_spec, TsIndexOptions};

pub const RUST_INDEXER_NAME: &str = "rust_treesitter";
pub const RUST_LANGUAGE_ID: &str = "rust";

#[derive(Debug, Clone, Default)]
pub struct RustIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct RustIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub imports: usize,
    /// Medium-confidence heuristic `Calls` / `References` edges (P23 R1
    /// in-process call resolver).
    #[serde(default)]
    pub references: usize,
    pub resolver_used: String,
}

pub fn index_rust(store: &mut Store, options: &RustIndexOptions) -> Result<RustIndexResult> {
    let ts = index_repo_with_spec(
        store,
        &RUST_SPEC,
        &TsIndexOptions {
            repo_root: options.repo_root.clone(),
            code_roots: options.code_roots.clone(),
            exclude_globs: options.exclude_globs.clone(),
            resolution_paths: Vec::new(),
        },
    )?;
    Ok(RustIndexResult {
        files: ts.files,
        symbols: ts.symbols,
        imports: ts.imports,
        references: ts.references,
        resolver_used: ts.resolver_used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::{EdgeKind, NodeKind};
    use std::path::Path;

    fn open_temp_store(root: &Path) -> Store {
        let db = root.join("graph.db");
        let mut store = Store::open(&db).unwrap();
        store.migrate().unwrap();
        store
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn indexes_a_rust_crate_fixture_end_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "pub mod util;\n\
             use std::collections::HashMap;\n\
             use crate::util::Thing;\n\
             pub struct Greeter { name: String }\n\
             impl Greeter {\n  pub fn greet(&self) -> String { self.name.clone() }\n}\n\
             pub fn helper() {}\n",
        );
        write(tmp.path(), "src/util.rs", "pub struct Thing;\n");
        let mut store = open_temp_store(tmp.path());

        let opts = RustIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        };
        let result = index_rust(&mut store, &opts).unwrap();

        assert_eq!(result.files, 2);
        assert!(result.symbols >= 3, "struct+method+fn, got {result:?}");
        // Only the in-repo `use crate::util::Thing` resolves; `use std::…` is
        // an external import and must be dropped, not kept as a dangling edge.
        assert_eq!(
            result.imports, 1,
            "only the in-repo use resolves, got {result:?}"
        );
        assert_eq!(result.resolver_used, RUST_INDEXER_NAME);

        let nodes = store.list_all_nodes().unwrap();
        assert!(nodes
            .iter()
            .any(|n| n.kind == NodeKind::RustStruct
                && n.id.to_string() == "rust::src/lib.rs::Greeter"));
        assert!(nodes.iter().any(|n| n.kind == NodeKind::RustMethod
            && n.id.to_string() == "rust::src/lib.rs::Greeter::greet"));
        assert!(nodes
            .iter()
            .any(|n| n.kind == NodeKind::RustFunction
                && n.id.to_string() == "rust::src/lib.rs::helper"));

        // The resolved import points at the real sibling module file …
        let imports = store
            .list_edges_by_kind(groundgraph_core::EdgeKind::Imports)
            .unwrap();
        assert!(
            imports
                .iter()
                .any(|e| e.from_id.to_string() == "file::src/lib.rs"
                    && e.to_id.to_string() == "file::src/util.rs"),
            "expected lib.rs → util.rs import edge, got {imports:?}"
        );
        // … and no dangling std/external target survives.
        assert!(
            imports.iter().all(|e| !e.to_id.to_string().contains("std")),
            "external std import must be dropped, got {imports:?}"
        );
    }

    fn calls_edges(store: &Store) -> Vec<(String, String)> {
        store
            .list_edges_by_kind(EdgeKind::Calls)
            .unwrap()
            .iter()
            .map(|e| (e.from_id.to_string(), e.to_id.to_string()))
            .collect()
    }

    #[test]
    fn emits_same_file_call_edges() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "pub fn a() { b(); }\npub fn b() {}\n",
        );
        let mut store = open_temp_store(tmp.path());
        let opts = RustIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        };
        let result = index_rust(&mut store, &opts).unwrap();
        assert!(
            result.references >= 1,
            "expected at least one heuristic call edge, got {result:?}"
        );
        let calls = calls_edges(&store);
        assert!(
            calls.contains(&(
                "rust::src/lib.rs::a".to_string(),
                "rust::src/lib.rs::b".to_string()
            )),
            "expected a → b call edge, got {calls:?}"
        );
    }

    #[test]
    fn emits_cross_file_call_edges_via_use() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "pub mod util;\nuse crate::util::helper;\npub fn run() { helper(); }\n",
        );
        write(tmp.path(), "src/util.rs", "pub fn helper() {}\n");
        let mut store = open_temp_store(tmp.path());
        let opts = RustIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        };
        index_rust(&mut store, &opts).unwrap();
        let calls = calls_edges(&store);
        assert!(
            calls.contains(&(
                "rust::src/lib.rs::run".to_string(),
                "rust::src/util.rs::helper".to_string()
            )),
            "expected cross-file run → util::helper call edge, got {calls:?}"
        );
    }

    #[test]
    fn local_scoped_assoc_calls_link() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "pub struct Foo;\nimpl Foo { pub fn new() -> Self { Foo } }\n\
             pub fn run() { let _ = Foo::new(); }\n",
        );
        let mut store = open_temp_store(tmp.path());
        let opts = RustIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        };
        index_rust(&mut store, &opts).unwrap();
        let calls = calls_edges(&store);
        assert!(
            calls.contains(&(
                "rust::src/lib.rs::run".to_string(),
                "rust::src/lib.rs::Foo::new".to_string()
            )),
            "local Foo::new() should link, got {calls:?}"
        );
    }

    #[test]
    fn external_scoped_calls_do_not_mislink_to_local_symbols() {
        // `HashMap::new()` must never create a `run → Foo::new` edge just
        // because a local `Foo::new` exists. The head of a `Type::assoc`
        // path has to be a *local* type for the edge to be emitted.
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "use std::collections::HashMap;\npub struct Foo;\n\
             impl Foo { pub fn new() -> Self { Foo } }\n\
             pub fn run() { let _m: HashMap<u8, u8> = HashMap::new(); }\n",
        );
        let mut store = open_temp_store(tmp.path());
        let opts = RustIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        };
        index_rust(&mut store, &opts).unwrap();
        let calls = calls_edges(&store);
        assert!(
            !calls.contains(&(
                "rust::src/lib.rs::run".to_string(),
                "rust::src/lib.rs::Foo::new".to_string()
            )),
            "external HashMap::new must not mislink to local Foo::new, got {calls:?}"
        );
    }

    #[test]
    fn reindex_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/main.rs",
            "fn main() {}\nstruct A;\nimpl A { fn go(&self) {} }\n",
        );
        let mut store = open_temp_store(tmp.path());
        let opts = RustIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        };

        let first = index_rust(&mut store, &opts).unwrap();
        let nodes_first = store.list_all_nodes().unwrap().len();
        let second = index_rust(&mut store, &opts).unwrap();
        let nodes_second = store.list_all_nodes().unwrap().len();

        assert_eq!(first, second, "result counts must be stable across runs");
        assert_eq!(
            nodes_first, nodes_second,
            "re-index must not duplicate nodes"
        );
    }

    #[test]
    fn target_dir_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "src/lib.rs", "pub fn keep() {}\n");
        write(
            tmp.path(),
            "target/debug/build/generated.rs",
            "pub fn dropme() {}\n",
        );
        let mut store = open_temp_store(tmp.path());
        let opts = RustIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from(".")],
            exclude_globs: vec![],
        };
        index_rust(&mut store, &opts).unwrap();
        let nodes = store.list_all_nodes().unwrap();
        assert!(nodes.iter().any(|n| n.name.as_deref() == Some("keep")));
        assert!(
            !nodes.iter().any(|n| n.name.as_deref() == Some("dropme")),
            "files under target/ must be skipped"
        );
    }
}
