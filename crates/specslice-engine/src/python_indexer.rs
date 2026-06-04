//! P16/P23.1 — Python language adapter.
//!
//! Since the P23 收敛, Python has a **single structural source of truth**:
//! the in-process tree-sitter driver ([`crate::python_treesitter`]). It
//! owns classes / functions / methods, pytest tests, framework-decorator
//! metadata, and `src/`-layout import resolution — deterministic, fast, no
//! external server required. Output is tagged `indexer = python_treesitter`.
//!
//! The LSP server (`pyright`/`basedpyright`/`pylsp`) is an **optional
//! Tier-3 enrichment**: when one is discovered it contributes only the
//! semantic `Calls` / `References` edges it is uniquely good at, overlaid
//! onto the existing tree-sitter symbol ids (the two id schemes are
//! identical by construction). LSP edges are tagged `indexer = python_lsp`.
//! When no LSP is present the structural graph is already complete, so the
//! adapter simply records why enrichment was skipped — there is no longer
//! any "fallback" path or second structural implementation.

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_core::language_batch::LanguageIndexBatch;
use specslice_core::NodeKind;
use specslice_store::Store;

use crate::dart_indexer::ingest_language_batch_minimal;
use crate::lsp_client::LspSymbolKind;
use crate::lsp_indexer::{
    binary_on_path, run_profile, LspIndexOptions, LspIndexOutcome, LspProfile,
};
use crate::treesitter::{self, TsIndexOptions};

pub const PYTHON_INDEXER_NAME: &str = "python_lsp";
pub const PYTHON_LANGUAGE_ID: &str = "python";
pub const PYTHON_LSP_COMMAND_ENV: &str = "SPECSLICE_PYTHON_LSP_BIN";

#[derive(Debug, Clone, Default)]
pub struct PythonIndexOptions {
    pub repo_root: PathBuf,
    pub code_roots: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    /// Operator-supplied LSP binary. When set, venv discovery is
    /// skipped entirely (the operator is in charge).
    pub lsp_command: Option<String>,
    /// Disable venv auto-detection for deterministic tests.
    pub disable_venv_discovery: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PythonIndexResult {
    pub files: usize,
    pub symbols: usize,
    pub tests: usize,
    pub imports: usize,
    /// Count of structural symbols whose decorators were classified
    /// as a framework entry point (FastAPI route, Celery task,
    /// Click/Typer command, …). 0 in pure-stdlib repos. P17.
    #[serde(default)]
    pub framework_entrypoints: usize,
    /// Number of `Calls` / `References` edges contributed by the optional
    /// Tier-3 LSP enrichment pass (0 when no LSP was available).
    #[serde(default)]
    pub references: usize,
    /// `python_treesitter` when the structural pass produced anything,
    /// empty when no Python files were found.
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

/// Top-level entrypoint. The tree-sitter driver produces the entire
/// structural graph (symbols + pytest tests + resolved imports + framework
/// metadata); an optional LSP pass then overlays `Calls` / `References`.
pub fn index_python(store: &mut Store, options: &PythonIndexOptions) -> Result<PythonIndexResult> {
    let spec = &crate::python_treesitter::PYTHON_SPEC;
    let ts_name = treesitter::indexer_name(spec);
    store
        .clear_indexer_outputs(&ts_name)
        .context("clearing previous Python tree-sitter outputs")?;
    store
        .clear_indexer_outputs(PYTHON_INDEXER_NAME)
        .context("clearing previous Python LSP outputs")?;

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

    // Count framework entrypoints + collect the id set of structural nodes
    // (so the optional LSP pass can attach edges without dangling targets).
    let mut framework_entrypoints = 0usize;
    let mut known_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for node in store.list_all_nodes().context("listing nodes")? {
        if node.indexer.as_deref() != Some(ts_name.as_str()) {
            continue;
        }
        known_ids.insert(node.id.to_string());
        if let Some(meta) = &node.metadata_json {
            if crate::python_treesitter::metadata_is_framework_entrypoint(meta) {
                framework_entrypoints += 1;
            }
        }
    }

    // Tier 3 (optional): LSP `Calls` / `References` enrichment, overlaid
    // onto the existing tree-sitter symbol ids (identical id scheme).
    let probe = ProbeOutcome::from_options(options);
    let mut references = 0usize;
    let skip_reason = match probe.command.clone() {
        Some(cmd) => {
            let profile = python_profile();
            let lsp_options = LspIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: options.code_roots.clone(),
                exclude_globs: options.exclude_globs.clone(),
                lsp_command: Some(cmd),
            };
            match run_profile(&profile, &lsp_options)? {
                LspIndexOutcome::Indexed(boxed) => {
                    let crate::lsp_indexer::LspIndexedBatch { batch, stats } = *boxed;
                    let refs: Vec<_> = batch
                        .references
                        .into_iter()
                        .filter(|r| {
                            known_ids.contains(r.from_symbol_id.as_str())
                                && known_ids.contains(r.to_symbol_id.as_str())
                        })
                        .collect();
                    references = refs.len();
                    if !refs.is_empty() {
                        let refs_batch = LanguageIndexBatch {
                            language: PYTHON_LANGUAGE_ID.into(),
                            references: refs,
                            ..Default::default()
                        };
                        ingest_language_batch_minimal(store, &refs_batch, PYTHON_INDEXER_NAME)
                            .context("ingesting Python LSP reference edges")?;
                    }
                    stats.skip_reason
                }
                LspIndexOutcome::Skipped { reason, .. } => reason,
            }
        }
        None => probe.skip_reason,
    };

    Ok(PythonIndexResult {
        files: ts.files,
        symbols: ts.symbols,
        tests: ts.tests,
        imports: ts.imports,
        framework_entrypoints,
        references,
        resolver_used: ts.resolver_used,
        sidecar_skip_reason: skip_reason,
    })
}

/// True when an optional Python LSP enrichment server is discoverable.
/// Structural indexing no longer depends on it — this only gates the
/// Tier-3 `Calls` / `References` overlay — but the CLI still surfaces it.
pub fn python_lsp_available(options: &PythonIndexOptions) -> bool {
    ProbeOutcome::from_options(options).command.is_some()
}

fn python_profile() -> LspProfile {
    LspProfile {
        language: PYTHON_LANGUAGE_ID,
        language_id: PYTHON_LANGUAGE_ID,
        file_extensions: &["py"],
        skip_dirs: &[
            ".venv",
            "venv",
            "__pycache__",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
            ".tox",
            ".eggs",
            ".git",
            "node_modules",
            "build",
            "dist",
            "site-packages",
        ],
        skip_suffixes: &[],
        default_command: "pyright-langserver",
        // pyright / basedpyright / pylsp all accept `--stdio`. We set
        // the flag so operators do not need to remember it.
        default_args: &["--stdio"],
        command_env_var: PYTHON_LSP_COMMAND_ENV,
        map_kind: python_map_kind,
        qualify: python_qualify,
    }
}

fn python_map_kind(kind: LspSymbolKind, _parent: Option<NodeKind>) -> Option<NodeKind> {
    match kind {
        LspSymbolKind::Module => Some(NodeKind::PythonModule),
        LspSymbolKind::Class => Some(NodeKind::PythonClass),
        LspSymbolKind::Method => Some(NodeKind::PythonMethod),
        LspSymbolKind::Function => Some(NodeKind::PythonFunction),
        _ => None,
    }
}

fn python_qualify(file_rel: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}.{name}"),
        None => format!("{file_rel}::{name}"),
    }
}

#[derive(Debug, Default)]
struct ProbeOutcome {
    command: Option<String>,
    skip_reason: String,
}

impl ProbeOutcome {
    /// Resolve the Python LSP binary following the user-documented
    /// discovery order:
    ///
    /// 1. `SPECSLICE_PYTHON_LSP_BIN` env var (authoritative)
    /// 2. operator override in `.specslice.yaml` (authoritative)
    /// 3. project-local `.venv/bin/{basedpyright-langserver,
    ///    pyright-langserver, pylsp}`
    /// 4. PATH fallbacks for the same three binaries
    /// 5. nothing found → caller falls back to the pure AST scan
    ///
    /// Steps 1 and 2 are *authoritative*: when an operator explicitly
    /// names a binary we never silently substitute another one. Steps
    /// 3+ are best-effort discovery and may all be skipped without
    /// affecting correctness.
    ///
    /// Each candidate is then **smoke-launched** by [`runs_ok`] — the
    /// previous probe only checked `is_file()`, which let through a
    /// classic broken-interpreter shebang (e.g. `pylsp` shipped by a
    /// removed conda env).
    fn from_options(options: &PythonIndexOptions) -> Self {
        // Helper: wrap a resolved command + authoritative source label
        // ("env" / "config") so the caller-side authoritative branches
        // produce a useful skip_reason if the smoke launch fails.
        let accept_authoritative = |cmd: String, label: &str| -> Self {
            if runs_ok(&cmd) {
                Self {
                    command: Some(cmd),
                    skip_reason: String::new(),
                }
            } else {
                Self {
                    command: None,
                    skip_reason: format!(
                        "{label}=`{cmd}` 启动失败 — 二进制存在但 `--help` 未在 1.5s 内成功退出（典型情况：shebang 指向已删除的解释器）。已退化为 AST fallback"
                    ),
                }
            }
        };
        if let Ok(env_cmd) = std::env::var(PYTHON_LSP_COMMAND_ENV) {
            if !binary_on_path(&env_cmd) {
                return Self {
                    command: None,
                    skip_reason: format!(
                        "{PYTHON_LSP_COMMAND_ENV}=`{env_cmd}` 未找到对应可执行文件，已退化为 AST fallback"
                    ),
                };
            }
            return accept_authoritative(env_cmd, PYTHON_LSP_COMMAND_ENV);
        }
        if let Some(cmd) = options.lsp_command.as_deref() {
            if !binary_on_path(cmd) {
                return Self {
                    command: None,
                    skip_reason: format!(
                        "`python.lsp_command = {cmd}` 未找到对应可执行文件，已退化为 AST fallback"
                    ),
                };
            }
            return accept_authoritative(cmd.to_string(), "python.lsp_command");
        }
        // Best-effort discovery: skip silently if the candidate fails
        // the smoke launch and try the next entry. Operators get a
        // useful summary if nothing works at the end.
        if !options.disable_venv_discovery {
            for relative in [
                ".venv/bin/basedpyright-langserver",
                ".venv/bin/pyright-langserver",
                ".venv/bin/pylsp",
            ] {
                let candidate = options.repo_root.join(relative);
                if !candidate.is_file() {
                    continue;
                }
                let cmd = candidate.to_string_lossy().into_owned();
                if runs_ok(&cmd) {
                    return Self {
                        command: Some(cmd),
                        skip_reason: String::new(),
                    };
                }
            }
        }
        for fallback in ["basedpyright-langserver", "pyright-langserver", "pylsp"] {
            if !binary_on_path(fallback) {
                continue;
            }
            if runs_ok(fallback) {
                return Self {
                    command: Some(fallback.to_string()),
                    skip_reason: String::new(),
                };
            }
        }
        Self {
            command: None,
            skip_reason:
                "未在 PATH / .venv 中找到可启动的 pyright/basedpyright/pylsp（要么不存在，要么 `--help` 启动失败），已退化为 AST fallback".into(),
        }
    }
}

/// Smoke-launch a candidate Python LSP binary. Returns `true` only if
/// `<cmd> --help` spawns and reaches a terminal state within 1.5s.
/// Any non-success outcome (spawn error, shebang failure, timeout
/// followed by a kill) returns `false` and lets the caller move on to
/// the next candidate.
///
/// Thin shim that delegates the actual smoke launch to the shared
/// [`crate::lsp_probe`] module. Kept as a function — rather than
/// inlining — so the rest of the Python adapter (and its regression
/// tests) keep the same callsite shape.
fn runs_ok(cmd: &str) -> bool {
    crate::lsp_probe::probe_lsp_command(
        cmd,
        crate::lsp_probe::DEFAULT_SMOKE_ARGS,
        crate::lsp_probe::DEFAULT_TIMEOUT,
    )
    .is_runnable()
}

#[cfg(test)]
mod tests {
    use super::*;
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
            lsp_command: Some("specslice_nonexistent_python_lsp_999".into()),
            disable_venv_discovery: true,
        };
        let result = index_python(&mut store, &opts).expect("index_python ok");

        assert_eq!(
            result.resolver_used,
            treesitter::indexer_name(&crate::python_treesitter::PYTHON_SPEC),
            "structure now comes from the tree-sitter driver: {result:?}"
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
        assert!(
            !result.sidecar_skip_reason.is_empty(),
            "skip reason recorded when no LSP available"
        );

        // Cross-check that the store actually received the right
        // shapes: a PythonClass for `Greeter`, a TestCase for
        // `test_basic`, and an Imports edge from the test file to
        // `app/greeter.py`.
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
        // We seed a tiny FastAPI / Celery / Click triple, run the
        // tree-sitter structural pass, and verify the resulting Node has
        // a populated `metadata_json` field that round-trips through
        // serde into a FrameworkRole the engine recognises as an entry.
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
            lsp_command: Some("specslice_nonexistent_python_lsp_999".into()),
            disable_venv_discovery: true,
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

    #[test]
    fn python_qualify_uses_dot_for_methods() {
        assert_eq!(python_qualify("app/foo.py", None, "Bar"), "app/foo.py::Bar");
        assert_eq!(
            python_qualify("app/foo.py", Some("app/foo.py::Bar"), "baz"),
            "app/foo.py::Bar.baz"
        );
    }

    #[test]
    fn python_lsp_available_respects_options_override() {
        let tmp = tempdir().unwrap();
        let opts = PythonIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from(".")],
            exclude_globs: vec![],
            lsp_command: Some("specslice_nonexistent_python_lsp_999".into()),
            disable_venv_discovery: true,
        };
        assert!(!python_lsp_available(&opts));
    }

    /// Regression — a binary whose shebang points at a missing
    /// interpreter (the exact failure mode on the reviewer's box:
    /// `pylsp` exists on PATH but `bad interpreter:
    /// /Users/.../anaconda3/bin/python: no such file or directory`)
    /// must be rejected. The previous probe only checked file
    /// existence, so it claimed the binary was usable and the
    /// downstream `index_python` silently fell back to AST — the
    /// opt-in smoke test then failed its `resolver_used == python_lsp`
    /// assertion.
    #[test]
    #[cfg(unix)]
    fn python_lsp_available_rejects_binary_with_broken_shebang() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().unwrap();
        let bin = tmp.path().join("bad_pylsp");
        std::fs::write(
            &bin,
            "#!/specslice/nonexistent/python\nprint('unreachable')\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();

        let opts = PythonIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from(".")],
            exclude_globs: vec![],
            lsp_command: Some(bin.to_string_lossy().into_owned()),
            disable_venv_discovery: true,
        };
        assert!(
            !python_lsp_available(&opts),
            "broken-shebang binary should not register as available"
        );
    }

    /// A binary that runs and prints help text (positive control) must
    /// stay registered as available. We use the system `echo` so this
    /// test works on any CI box without depending on `pylsp` being
    /// installed.
    #[test]
    #[cfg(unix)]
    fn python_lsp_available_accepts_executable_that_runs() {
        let tmp = tempdir().unwrap();
        let bin = tmp.path().join("fake_pylsp");
        std::fs::write(&bin, "#!/bin/sh\necho 'usage: fake_pylsp [...]'\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();

        let opts = PythonIndexOptions {
            repo_root: tmp.path().to_path_buf(),
            code_roots: vec![PathBuf::from(".")],
            exclude_globs: vec![],
            lsp_command: Some(bin.to_string_lossy().into_owned()),
            disable_venv_discovery: true,
        };
        assert!(python_lsp_available(&opts));
    }

    /// Re-indexing the same repo twice must be a no-op at the graph
    /// level (P23.1 idempotency contract). We assert identical node /
    /// edge counts after the second pass — the wrapper clears its
    /// previous `python_treesitter` + `python_lsp` outputs first.
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
            lsp_command: Some("specslice_nonexistent_python_lsp_999".into()),
            disable_venv_discovery: true,
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
