//! P20/P23.3 — Java language adapter (structure + heuristic).
//!
//! The in-process tree-sitter driver ([`crate::java_treesitter`]) is the
//! **sole source of truth** for Java: classes / interfaces / enums / records,
//! methods + constructors, JUnit `@Test` cases, `import x.y.Z;` resolved to
//! repo-relative file ids, and the medium-confidence heuristic `Calls` /
//! `References` edges its body scan produces. Output is tagged `indexer =
//! java_treesitter`.
//!
//! Precise cross-symbol resolution is supplied out-of-band by a SCIP overlay
//! (`scip-java`; ADR-0001 R1/R2) when one is present, which authoritatively
//! supersedes the heuristic edges on the files it covers. The former in-process
//! `jdtls` Tier-3 sidecar was retired in favour of SCIP — only Swift keeps an
//! LSP (no mature SCIP indexer exists for it).

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_store::Store;

use crate::treesitter::{self, TsIndexOptions};

pub const JAVA_LANGUAGE_ID: &str = "java";

/// Legacy `indexer` tag for the retired `jdtls` overlay. Cleared on every run
/// so upgrading an existing store drops any stale `java_lsp` rows it holds.
const LEGACY_JAVA_LSP_INDEXER: &str = "java_lsp";

#[derive(Debug, Clone, Default)]
pub struct JavaIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct JavaIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub imports: usize,
    /// Medium-confidence heuristic `Calls` / `References` edges produced by the
    /// tree-sitter body scan. SCIP supersedes these on the files it covers.
    #[serde(default)]
    pub references: usize,
    /// `java_treesitter` when the structural pass produced anything, empty when
    /// no Java files were found.
    pub resolver_used: String,
}

/// Top-level entrypoint. The tree-sitter driver produces the entire Java graph
/// (symbols + JUnit tests + resolved imports + heuristic Calls/References).
pub fn index_java(store: &mut Store, options: &JavaIndexOptions) -> Result<JavaIndexResult> {
    let spec = &crate::java_treesitter::JAVA_SPEC;
    let ts_name = treesitter::indexer_name(spec);
    store
        .clear_indexer_outputs(&ts_name)
        .context("clearing previous Java tree-sitter outputs")?;
    store
        .clear_indexer_outputs(LEGACY_JAVA_LSP_INDEXER)
        .context("clearing retired Java LSP outputs")?;

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
    .context("indexing Java structure via tree-sitter")?;

    Ok(JavaIndexResult {
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
    use specslice_core::NodeKind;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_fixture(root: &Path) {
        for (rel, body) in [
            (
                "src/main/java/com/example/Greeter.java",
                "package com.example;\n\
                 public class Greeter {\n  \
                   public String greet(String name) { return \"hi \" + name; }\n\
                 }\n",
            ),
            (
                "src/test/java/com/example/GreeterTest.java",
                "package com.example;\n\
                 import org.junit.jupiter.api.Test;\n\
                 import com.example.Greeter;\n\
                 class GreeterTest {\n  \
                   @Test\n  \
                   void greetsByName() {}\n\
                 }\n",
            ),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
    }

    fn open_temp_store(root: &Path) -> (Store, PathBuf) {
        let db = root.join("graph.db");
        let mut store = Store::open(&db).unwrap();
        store.migrate().unwrap();
        (store, db)
    }

    #[test]
    fn treesitter_pass_runs_against_java_hello_fixture_without_lsp() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let (mut store, _db) = open_temp_store(tmp.path());

        let opts = JavaIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        };
        let result = index_java(&mut store, &opts).unwrap();
        assert_eq!(
            result.resolver_used,
            treesitter::indexer_name(&crate::java_treesitter::JAVA_SPEC),
            "structure comes from the tree-sitter driver: {result:?}"
        );
        assert!(result.files >= 2, "both Java files indexed: {result:?}");
        assert!(result.symbols >= 2, "class + method counted: {result:?}");
        assert!(result.tests >= 1, "JUnit @Test recovered: {result:?}");

        let nodes = store.list_all_nodes().unwrap();
        assert!(
            nodes
                .iter()
                .any(|n| n.kind == NodeKind::JavaClass && n.name.as_deref() == Some("Greeter")),
            "Greeter class present; got {:?}",
            nodes
                .iter()
                .map(|n| (n.kind, n.name.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            nodes
                .iter()
                .any(|n| n.kind == NodeKind::TestCase && n.name.as_deref() == Some("greetsByName")),
            "JUnit test case present"
        );

        // The intra-repo import resolves file → file.
        let edges = store.list_all_edges().unwrap();
        assert!(
            edges.iter().any(|e| {
                e.kind == specslice_core::EdgeKind::Imports
                    && e.from_id.as_str() == "file::src/test/java/com/example/GreeterTest.java"
                    && e.to_id.as_str() == "file::src/main/java/com/example/Greeter.java"
            }),
            "GreeterTest should import Greeter across the source tree"
        );
    }

    #[test]
    fn reindexing_is_idempotent() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let (mut store, _db) = open_temp_store(tmp.path());
        let opts = JavaIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("src")],
            exclude_globs: vec![],
        };
        let first = index_java(&mut store, &opts).expect("first index ok");
        let nodes_1 = store.list_all_nodes().unwrap().len();
        let edges_1 = store.list_all_edges().unwrap().len();
        let second = index_java(&mut store, &opts).expect("second index ok");
        let nodes_2 = store.list_all_nodes().unwrap().len();
        let edges_2 = store.list_all_edges().unwrap().len();
        assert_eq!(first, second, "result counts stable across re-index");
        assert_eq!(nodes_1, nodes_2, "node count stable across re-index");
        assert_eq!(edges_1, edges_2, "edge count stable across re-index");
    }
}
