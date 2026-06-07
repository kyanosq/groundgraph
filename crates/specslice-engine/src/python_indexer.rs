//! P16/P23.1 — Python language adapter (structure + heuristic).
//!
//! The in-process tree-sitter driver ([`crate::python_treesitter`]) is the
//! **sole source of truth** for Python: classes / functions / methods, pytest
//! tests, framework-decorator metadata, `src/`-layout import resolution, and
//! the medium-confidence heuristic `Calls` / `References` edges its body scan
//! produces — deterministic, fast, no external server required. Output is
//! tagged `indexer = python_treesitter`.
//!
//! Precise cross-symbol resolution is supplied out-of-band by a SCIP overlay
//! (`scip-python`; ADR-0001 R1/R2) when one is present, which authoritatively
//! supersedes the heuristic edges on the files it covers. The former in-process
//! `pyright`/`pylsp` Tier-3 sidecar was retired in favour of SCIP — only Swift
//! keeps an LSP (no mature SCIP indexer exists for it).

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_store::Store;

use crate::treesitter::{self, TsIndexOptions};

pub const PYTHON_LANGUAGE_ID: &str = "python";

/// Legacy `indexer` tag for the retired Python LSP overlay. Cleared on every
/// run so upgrading an existing store drops any stale `python_lsp` rows.
const LEGACY_PYTHON_LSP_INDEXER: &str = "python_lsp";

#[derive(Debug, Clone, Default)]
pub struct PythonIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PythonIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub imports: usize,
    /// Count of structural symbols whose decorators were classified as a
    /// framework entry point (FastAPI route, Celery task, Click/Typer command,
    /// …). 0 in pure-stdlib repos. P17.
    #[serde(default)]
    pub framework_entrypoints: usize,
    /// Medium-confidence heuristic `Calls` / `References` edges produced by the
    /// tree-sitter body scan. SCIP supersedes these on the files it covers.
    #[serde(default)]
    pub references: usize,
    /// `python_treesitter` when the structural pass produced anything, empty
    /// when no Python files were found.
    pub resolver_used: String,
}

/// Top-level entrypoint. The tree-sitter driver produces the entire Python
/// graph (symbols + pytest tests + resolved imports + framework metadata +
/// heuristic Calls/References).
pub fn index_python(store: &mut Store, options: &PythonIndexOptions) -> Result<PythonIndexResult> {
    let spec = &crate::python_treesitter::PYTHON_SPEC;
    let ts_name = treesitter::indexer_name(spec);
    store
        .clear_indexer_outputs(&ts_name)
        .context("clearing previous Python tree-sitter outputs")?;
    store
        .clear_indexer_outputs(LEGACY_PYTHON_LSP_INDEXER)
        .context("clearing retired Python LSP outputs")?;

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
    .context("indexing Python structure via tree-sitter")?;

    // Count framework entrypoints from the metadata the structural pass stamped
    // on its nodes (FastAPI route / Celery task / Click command, …).
    let mut framework_entrypoints = 0usize;
    for node in store.list_all_nodes().context("listing nodes")? {
        if node.indexer.as_deref() != Some(ts_name.as_str()) {
            continue;
        }
        if let Some(meta) = &node.metadata_json {
            if crate::python_treesitter::metadata_is_framework_entrypoint(meta) {
                framework_entrypoints += 1;
            }
        }
    }

    Ok(PythonIndexResult {
        files: ts.files,
        symbols: ts.symbols,
        tests: ts.tests,
        imports: ts.imports,
        framework_entrypoints,
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
                "app/__init__.py",
                "from .greeter import Greeter\n__all__ = ['Greeter']\n",
            ),
            (
                "app/greeter.py",
                concat!(
                    "from .utils import banner\n",
                    "\n",
                    "class Greeter:\n",
                    "    def __init__(self, name):\n",
                    "        self.name = name\n",
                    "\n",
                    "    def greet(self):\n",
                    "        return banner(False) + ' ' + self.name\n",
                    "\n",
                    "def make_greeter(name):\n",
                    "    return Greeter(name)\n",
                ),
            ),
            ("app/utils.py", "def banner(formal):\n    return 'Hi'\n"),
            (
                "tests/test_greeter.py",
                concat!(
                    "import pytest\n",
                    "\n",
                    "from app.greeter import Greeter, make_greeter\n",
                    "\n",
                    "@pytest.fixture\n",
                    "def fix():\n",
                    "    return make_greeter('Ada')\n",
                    "\n",
                    "def test_basic(fix):\n",
                    "    assert fix.greet().endswith('Ada')\n",
                    "\n",
                    "class TestGroup:\n",
                    "    def test_inside(self):\n",
                    "        assert True\n",
                ),
            ),
        ] {
            let abs = root.join(rel);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(abs, body).unwrap();
        }
        std::fs::write(root.join(".specslice.yaml"), "repo:\n  root: .\n").unwrap();
    }

    #[test]
    fn treesitter_pass_emits_imports_pytest_tests_and_structural_symbols_without_lsp() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let db_path = tmp.path().join(".specslice/graph.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let mut store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();

        let opts = PythonIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("app"), PathBuf::from("tests")],
            exclude_globs: vec![],
        };
        let result = index_python(&mut store, &opts).expect("index_python ok");

        assert_eq!(
            result.resolver_used,
            treesitter::indexer_name(&crate::python_treesitter::PYTHON_SPEC),
            "structure comes from the tree-sitter driver: {result:?}"
        );
        assert!(result.files >= 4, "all .py files counted: {result:?}");
        assert!(
            result.symbols >= 5,
            "structural symbols counted: {result:?}"
        );
        assert!(
            result.tests >= 3,
            "test functions + classes counted: {result:?}"
        );
        assert!(result.imports >= 2, "imports counted: {result:?}");

        // Cross-check store shapes: a PythonClass for `Greeter`, a TestCase for
        // `test_basic`, and an Imports edge from the test file to greeter.py.
        let nodes = store.list_all_nodes().unwrap();
        let kinds: Vec<&NodeKind> = nodes.iter().map(|n| &n.kind).collect();
        assert!(kinds.iter().any(|k| **k == NodeKind::PythonClass));
        assert!(kinds.iter().any(|k| **k == NodeKind::PythonMethod));
        assert!(kinds.iter().any(|k| **k == NodeKind::PythonFunction));
        assert!(kinds.iter().any(|k| **k == NodeKind::TestCase));
        assert!(kinds.iter().any(|k| **k == NodeKind::TestGroup));

        let edges = store.list_all_edges().unwrap();
        let imports_target_greeter = edges.iter().any(|e| {
            e.kind == specslice_core::EdgeKind::Imports
                && e.from_id.as_str() == "file::tests/test_greeter.py"
                && e.to_id.as_str() == "file::app/greeter.py"
        });
        assert!(imports_target_greeter, "imports edge resolved across files");
    }

    #[test]
    fn framework_decorated_symbols_get_metadata_and_entry_status() {
        // Seed a tiny FastAPI / Celery / Click triple, run the tree-sitter
        // structural pass, and verify the resulting Node carries a populated
        // `metadata_json` that round-trips into a FrameworkRole recognised as
        // an entry point.
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/__init__.py"), "").unwrap();
        std::fs::write(
            root.join("app/web.py"),
            r#"
from fastapi import APIRouter
router = APIRouter()


@router.get("/items")
def list_items():
    return []


@app.task(queue="emails")
def send_email():
    return None


@click.command
def cli_run():
    return None
"#,
        )
        .unwrap();
        std::fs::write(root.join(".specslice.yaml"), "repo:\n  root: .\n").unwrap();
        let db_path = root.join(".specslice/graph.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let mut store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
        let opts = PythonIndexOptions {
            repo_root: root.to_path_buf(),
            code_roots: vec![PathBuf::from("app")],
            exclude_globs: vec![],
        };
        let result = index_python(&mut store, &opts).expect("index_python ok");
        assert_eq!(
            result.framework_entrypoints, 3,
            "expected 3 framework entrypoints (route + task + cli), got {result:?}"
        );
        let nodes = store.list_all_nodes().unwrap();
        let list_items = nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("list_items"))
            .expect("list_items node");
        let meta = list_items
            .metadata_json
            .as_deref()
            .expect("metadata_json populated for FastAPI route");
        let role: crate::python_frameworks::FrameworkRole =
            serde_json::from_str(meta).expect("metadata round-trips");
        assert_eq!(role.family(), "fastapi_route");
        assert!(role.is_framework_entrypoint());
    }

    /// Re-indexing the same repo twice must be a no-op at the graph level
    /// (P23.1 idempotency contract): identical node / edge counts after the
    /// second pass, since the wrapper clears its previous outputs first.
    #[test]
    fn reindexing_is_idempotent() {
        let tmp = tempdir().unwrap();
        write_fixture(tmp.path());
        let db_path = tmp.path().join(".specslice/graph.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let mut store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
        let opts = PythonIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from("app"), PathBuf::from("tests")],
            exclude_globs: vec![],
        };

        let first = index_python(&mut store, &opts).expect("first index ok");
        let nodes_1 = store.list_all_nodes().unwrap().len();
        let edges_1 = store.list_all_edges().unwrap().len();

        let second = index_python(&mut store, &opts).expect("second index ok");
        let nodes_2 = store.list_all_nodes().unwrap().len();
        let edges_2 = store.list_all_edges().unwrap().len();

        assert_eq!(first, second, "result counts stable across re-index");
        assert_eq!(nodes_1, nodes_2, "node count stable across re-index");
        assert_eq!(edges_1, edges_2, "edge count stable across re-index");
    }
}
