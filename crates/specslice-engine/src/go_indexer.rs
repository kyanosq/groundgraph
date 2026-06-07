//! P11/P23.4 — Go language adapter (structure + heuristic).
//!
//! The in-process tree-sitter driver ([`crate::go_treesitter`]) is the **sole
//! source of truth** for Go: structs / interfaces / functions / methods, `go
//! test` cases (`TestXxx` / `BenchmarkXxx` / `FuzzXxx` / `ExampleXxx`),
//! resolved import paths, and the medium-confidence heuristic `Calls` /
//! `References` edges its body scan produces. Output is tagged `indexer =
//! go_treesitter`.
//!
//! Precise cross-symbol resolution is supplied out-of-band by the SCIP overlay
//! (`scip-go`; ADR-0001 R1/R2), which the engine ingests after this pass and
//! which authoritatively supersedes the heuristic edges on the files it
//! covers. The former in-process `gopls` Tier-3 sidecar was retired in favour
//! of SCIP — only Swift keeps an LSP (no mature SCIP indexer exists for it).

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_store::Store;

use crate::treesitter::{self, TsIndexOptions};

pub const GO_LANGUAGE_ID: &str = "go";

/// Legacy `indexer` tag for the retired `gopls` overlay. Cleared on every run
/// so upgrading an existing store drops any stale `go_lsp` rows it still holds.
const LEGACY_GO_LSP_INDEXER: &str = "go_lsp";

#[derive(Debug, Clone, Default)]
pub struct GoIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct GoIndexResult {
    pub files: usize,
    pub symbols: usize,
    #[serde(default)]
    pub tests: usize,
    #[serde(default)]
    pub imports: usize,
    /// Medium-confidence heuristic `Calls` / `References` edges produced by the
    /// tree-sitter body scan. SCIP supersedes these on the files it covers.
    #[serde(default)]
    pub references: usize,
    /// `go_treesitter` when the structural pass produced anything, empty when
    /// no Go files were found.
    pub resolver_used: String,
}

/// Top-level entrypoint. The tree-sitter driver produces the entire Go graph
/// (symbols + `go test` cases + resolved imports + heuristic Calls/References).
pub fn index_go(store: &mut Store, options: &GoIndexOptions) -> Result<GoIndexResult> {
    let spec = &crate::go_treesitter::GO_SPEC;
    let ts_name = treesitter::indexer_name(spec);
    store
        .clear_indexer_outputs(&ts_name)
        .context("clearing previous Go tree-sitter outputs")?;
    store
        .clear_indexer_outputs(LEGACY_GO_LSP_INDEXER)
        .context("clearing retired Go LSP outputs")?;

    let ts = treesitter::index_repo_with_spec(
        store,
        spec,
        &TsIndexOptions {
            repo_root: options.repo_root.clone(),
            code_roots: options.code_roots.clone(),
            exclude_globs: options.exclude_globs.clone(),
            resolution_paths: Vec::new(),
        },
    )
    .context("indexing Go structure via tree-sitter")?;

    Ok(GoIndexResult {
        files: ts.files,
        symbols: ts.symbols,
        tests: ts.tests,
        imports: ts.imports,
        references: ts.references,
        resolver_used: ts.resolver_used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::EdgeKind;
    use tempfile::tempdir;

    /// The driver indexes structure and emits a heuristic call edge
    /// (`Server.Greet` → `greet`) with no LSP / external toolchain.
    #[test]
    fn treesitter_pass_indexes_structure_and_heuristic_calls_without_lsp() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("main.go"),
            concat!(
                "package main\n\n",
                "type Server struct{}\n\n",
                "func (s Server) Greet(name string) string { return greet(name) }\n\n",
                "func greet(n string) string { return n }\n\n",
                "func main() { _ = Server{} }\n",
            ),
        )
        .unwrap();
        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();

        let opts = GoIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from(".")],
            exclude_globs: Vec::new(),
        };
        let result = index_go(&mut store, &opts).expect("index_go ok");

        assert_eq!(
            result.resolver_used,
            treesitter::indexer_name(&crate::go_treesitter::GO_SPEC),
            "structure comes from the tree-sitter driver: {result:?}"
        );
        assert!(result.files >= 1, "main.go indexed: {result:?}");
        assert!(
            result.symbols >= 3,
            "Server + Greet + greet + main: {result:?}"
        );
        assert!(
            result.references >= 1,
            "heuristic call resolver links Greet -> greet: {result:?}"
        );

        let calls = store.list_edges_by_kind(EdgeKind::Calls).unwrap();
        assert!(
            calls.iter().any(|e| e.to_id.as_str().ends_with("::greet")),
            "expected a heuristic Calls edge into greet, got {calls:?}"
        );
    }

    /// Re-indexing the same repo twice is a graph-level no-op (the wrapper
    /// clears its previous `go_treesitter` outputs first).
    #[test]
    fn reindexing_is_idempotent() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("main.go"),
            "package main\n\nfunc greet() string { return \"hi\" }\n\nfunc main() { _ = greet() }\n",
        )
        .unwrap();
        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let opts = GoIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from(".")],
            exclude_globs: Vec::new(),
        };

        let first = index_go(&mut store, &opts).expect("first index ok");
        let nodes_1 = store.list_all_nodes().unwrap().len();
        let edges_1 = store.list_all_edges().unwrap().len();
        let second = index_go(&mut store, &opts).expect("second index ok");
        let nodes_2 = store.list_all_nodes().unwrap().len();
        let edges_2 = store.list_all_edges().unwrap().len();

        assert_eq!(first, second, "result counts stable across re-index");
        assert_eq!(nodes_1, nodes_2, "node count stable across re-index");
        assert_eq!(edges_1, edges_2, "edge count stable across re-index");
    }
}
