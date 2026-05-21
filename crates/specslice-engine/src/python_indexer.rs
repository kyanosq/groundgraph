//! P16 — Python language adapter (LSP-first, AST 补强).
//!
//! Python is the first language we ship where SpecSlice **never** trusts
//! a single source of truth. Reasoning:
//!
//! - LSP servers (`pyright-langserver`, `basedpyright-langserver`,
//!   `pylsp`) give the strongest structural facts (cross-file resolve,
//!   call hierarchy). When one is present we always prefer it for
//!   classes / functions / methods and for `Calls` / `References`.
//! - Even a perfect LSP server leaves SpecSlice without `import` edges
//!   and without pytest test cases, both of which the AI agents need in
//!   their context packs. So we always run a tiny AST scanner alongside
//!   LSP. The AST scanner also takes over the structural pass when no
//!   LSP is available (e.g. CI without pyright installed).
//!
//! Confidence:
//!   - Symbols / `Calls` / `References` from the LSP pass are tagged
//!     `indexer = python_lsp` so callers (graph view, dead-code, MCP)
//!     can reason about their provenance.
//!   - Symbols + imports + pytest cases from the AST pass are tagged
//!     `indexer = python_ast`. Both indexers can coexist on the same
//!     symbol id; the engine `upsert_node` dedupes by id.
//!
//! When the LSP server fails mid-run we keep whatever it managed to
//! produce and let the AST scanner fill in the rest. This is the same
//! "downgrade to partial" UX the Swift / Go adapters already follow.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use specslice_core::artifact_id::{file_id, slugify, ArtifactId};
use specslice_core::language_batch::{
    FileArtifact, ImportEdge, LanguageIndexBatch, SymbolArtifact, SymbolRange, TestArtifact,
};
use specslice_core::NodeKind;
use specslice_store::Store;

use crate::dart_indexer::ingest_language_batch_minimal;
use crate::lsp_client::LspSymbolKind;
use crate::lsp_indexer::{
    binary_on_path, run_profile, LspIndexOptions, LspIndexOutcome, LspProfile,
};
use crate::python_ast::{is_pytest_test_class, is_pytest_test_function, scan, PythonSymbol};
use crate::python_frameworks::{classify_decorators, FrameworkRole};

pub const PYTHON_INDEXER_NAME: &str = "python_lsp";
pub const PYTHON_AST_INDEXER_NAME: &str = "python_ast";
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
    /// `python_lsp` when an LSP server ran the structural pass,
    /// `python_ast` when only the AST scanner contributed, empty when
    /// both passes skipped.
    pub resolver_used: String,
    pub sidecar_skip_reason: String,
}

/// Top-level entrypoint. Runs the LSP profile when available, then
/// always overlays the AST pass for imports + pytest cases (and for
/// structural symbols when no LSP succeeded).
pub fn index_python(store: &mut Store, options: &PythonIndexOptions) -> Result<PythonIndexResult> {
    let probe = ProbeOutcome::from_options(options);

    let mut lsp_batch: Option<LanguageIndexBatch> = None;
    let mut lsp_files = 0usize;
    let mut lsp_symbols = 0usize;
    let mut skip_reason = String::new();
    let mut resolver_used = String::new();

    if let Some(cmd) = probe.command.clone() {
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
                lsp_files = stats.files;
                lsp_symbols = stats.symbols;
                if !stats.skip_reason.is_empty() {
                    skip_reason = stats.skip_reason;
                }
                ingest_language_batch_minimal(store, &batch, PYTHON_INDEXER_NAME)
                    .context("ingesting Python LSP batch")?;
                lsp_batch = Some(batch);
                resolver_used = PYTHON_INDEXER_NAME.into();
            }
            LspIndexOutcome::Skipped { reason, .. } => {
                skip_reason = reason;
            }
        }
    } else {
        skip_reason = probe.skip_reason;
    }

    let ast_outcome =
        run_ast_pass(store, options, lsp_batch.as_ref()).context("running Python AST pass")?;

    if resolver_used.is_empty() && ast_outcome.symbols + ast_outcome.tests > 0 {
        resolver_used = PYTHON_AST_INDEXER_NAME.into();
    }

    let total_files = if lsp_files > 0 {
        lsp_files
    } else {
        ast_outcome.files
    };
    let total_symbols = lsp_symbols + ast_outcome.symbols;

    Ok(PythonIndexResult {
        files: total_files,
        symbols: total_symbols,
        tests: ast_outcome.tests,
        imports: ast_outcome.imports,
        framework_entrypoints: ast_outcome.framework_entrypoints,
        resolver_used,
        sidecar_skip_reason: skip_reason,
    })
}

/// True when *any* Python adapter (LSP probe or AST fallback) can do
/// useful work — used by CLI smoke output.
pub fn python_lsp_available(options: &PythonIndexOptions) -> bool {
    ProbeOutcome::from_options(options).command.is_some()
}

#[derive(Debug, Default)]
struct AstOutcome {
    files: usize,
    symbols: usize,
    tests: usize,
    imports: usize,
    /// Count of symbols whose decorators were classified as a
    /// framework entry point (FastAPI route, Celery task, Click
    /// command, …). Used by the CLI to surface "X framework
    /// entrypoints detected" so operators can validate P17 made it
    /// past the AST scanner. Counted *additionally* to `symbols`,
    /// not as a subset, so it can grow without affecting the
    /// symbol total.
    framework_entrypoints: usize,
}

fn run_ast_pass(
    store: &mut Store,
    options: &PythonIndexOptions,
    lsp_batch: Option<&LanguageIndexBatch>,
) -> Result<AstOutcome> {
    let py_files = discover_python_files(
        &options.repo_root,
        &options.code_roots,
        &options.exclude_globs,
    )?;
    if py_files.is_empty() {
        return Ok(AstOutcome::default());
    }

    // Project layouts vary: a flat `app/foo.py` repo resolves
    // `import app.foo` directly, but a `src/`-style layout
    // (`backend/app/foo.py`) needs us to know that `backend/` is a
    // *source root*. We infer those roots from where the
    // `__init__.py` chain starts: any directory that is *not* itself
    // a package but whose child is becomes a source root. Validated
    // against atagent (`backend/app/...`) where the un-fixed resolver
    // missed ~85% of `from app.X import ...` lines.
    let src_roots = discover_python_src_roots(&py_files);

    let mut outcome = AstOutcome::default();
    let mut batch = LanguageIndexBatch {
        language: PYTHON_LANGUAGE_ID.into(),
        ..Default::default()
    };

    let lsp_symbol_ids: std::collections::BTreeSet<String> = lsp_batch
        .map(|b| b.symbols.iter().map(|s| s.id.to_string()).collect())
        .unwrap_or_default();

    for file in &py_files {
        let source = std::fs::read_to_string(&file.absolute)
            .with_context(|| format!("reading {}", file.absolute.display()))?;
        let scan = scan(&source);
        outcome.files += 1;

        // File node is always recorded by the AST pass so even minimal
        // workspaces (no LSP, no symbols) still anchor edges to a file.
        if lsp_batch.is_none() {
            let hash = format!("{:x}", sha2::Sha256::digest(source.as_bytes()));
            batch.files.push(FileArtifact {
                id: file_id(&file.relative),
                path: file.relative.clone(),
                language: PYTHON_LANGUAGE_ID.into(),
                content_hash: hash,
            });
        }

        // Index pytest tests / groups so the engine can wire them to
        // `EdgeKind::Contains` and surface them under `--include-tests`.
        for sym in &scan.symbols {
            if is_pytest_test_class(sym) {
                push_test_group(&mut batch, &file.relative, sym);
                outcome.tests += 1;
            } else if is_pytest_test_function(sym) {
                push_test_case(&mut batch, &file.relative, sym);
                outcome.tests += 1;
            }
        }

        for import in &scan.imports {
            if let Some(target) =
                resolve_python_import(&py_files, &src_roots, &file.relative, &import.module_path)
            {
                batch.imports.push(ImportEdge {
                    from_file: file_id(&file.relative),
                    to_path: target,
                });
                outcome.imports += 1;
            }
        }

        // Structural symbols only when the LSP pass did not already
        // emit them. We compare by the same id the LSP profile would
        // produce so duplicates collapse on the upsert.
        for sym in &scan.symbols {
            if is_pytest_test_class(sym) || is_pytest_test_function(sym) {
                continue;
            }
            if lsp_batch.is_some() {
                let candidate = python_symbol_id(&file.relative, sym);
                if lsp_symbol_ids.contains(candidate.as_str()) {
                    continue;
                }
            }
            let framework_role = classify_decorators(&sym.decorators);
            let metadata_json = framework_role
                .as_ref()
                .and_then(|role| serde_json::to_string(role).ok());
            if framework_role
                .as_ref()
                .is_some_and(FrameworkRole::is_framework_entrypoint)
            {
                outcome.framework_entrypoints += 1;
            }
            push_structural_symbol(&mut batch, &file.relative, sym, metadata_json);
            outcome.symbols += 1;
        }
    }

    if !batch.files.is_empty()
        || !batch.symbols.is_empty()
        || !batch.tests.is_empty()
        || !batch.imports.is_empty()
    {
        ingest_language_batch_minimal(store, &batch, PYTHON_AST_INDEXER_NAME)
            .context("ingesting Python AST batch")?;
    }

    Ok(outcome)
}

fn push_structural_symbol(
    batch: &mut LanguageIndexBatch,
    file_rel: &str,
    sym: &PythonSymbol,
    metadata_json: Option<String>,
) {
    let id = python_symbol_id(file_rel, sym);
    let qualified = python_qualify(file_rel, None, &sym.qualified_name);
    let parent_id = sym
        .parent_qualified_name
        .as_deref()
        .map(|parent| python_qualified_id(file_rel, parent));
    batch.symbols.push(SymbolArtifact {
        id: id.clone(),
        kind: sym.kind,
        path: file_rel.into(),
        name: sym.name.clone(),
        qualified_name: qualified.clone(),
        start_line: sym.start_line,
        end_line: sym.end_line,
        parent_symbol_id: parent_id.clone(),
        metadata_json,
    });
    batch.symbol_ranges.push(SymbolRange {
        file_path: file_rel.into(),
        symbol_id: id,
        start_line: sym.start_line,
        end_line: sym.end_line,
        symbol_kind: sym.kind,
        qualified_name: qualified,
        parent_symbol_id: parent_id,
    });
}

fn push_test_group(batch: &mut LanguageIndexBatch, file_rel: &str, sym: &PythonSymbol) {
    let id = python_qualified_id(file_rel, &sym.qualified_name);
    batch.tests.push(TestArtifact {
        id,
        kind: NodeKind::TestGroup,
        path: file_rel.into(),
        name: sym.name.clone(),
        start_line: sym.start_line,
        end_line: sym.end_line,
        parent_symbol_id: None,
    });
}

fn push_test_case(batch: &mut LanguageIndexBatch, file_rel: &str, sym: &PythonSymbol) {
    let id = python_qualified_id(file_rel, &sym.qualified_name);
    let parent_id = sym
        .parent_qualified_name
        .as_deref()
        .filter(|parent| {
            // Only treat the parent class as a TestGroup ancestor when
            // it actually looks like one (`Test*`). Otherwise the test
            // case is a module-level function and anchors to the file.
            parent
                .rsplit('.')
                .next()
                .map(|tail| tail.starts_with("Test"))
                .unwrap_or(false)
        })
        .map(|parent| python_qualified_id(file_rel, parent));
    batch.tests.push(TestArtifact {
        id,
        kind: NodeKind::TestCase,
        path: file_rel.into(),
        name: pytest_display_name(file_rel, sym),
        start_line: sym.start_line,
        end_line: sym.end_line,
        parent_symbol_id: parent_id,
    });
}

fn pytest_display_name(file_rel: &str, sym: &PythonSymbol) -> String {
    // Pytest's textual id is `path::Class::method` or `path::function`.
    // Use the same shape so search / context_pack are user-recognisable.
    let _ = slugify; // ensure import retained even if future refactors drop usage
    if let Some(parent) = &sym.parent_qualified_name {
        format!("{file_rel}::{parent}::{name}", name = sym.name)
    } else {
        format!("{file_rel}::{}", sym.name)
    }
}

fn python_symbol_id(file_rel: &str, sym: &PythonSymbol) -> ArtifactId {
    python_qualified_id(file_rel, &sym.qualified_name)
}

fn python_qualified_id(file_rel: &str, qualified_in_module: &str) -> ArtifactId {
    let qualified = python_qualify(file_rel, None, qualified_in_module);
    ArtifactId::new(format!("{PYTHON_LANGUAGE_ID}::{qualified}"))
}

#[derive(Debug, Clone)]
struct DiscoveredPyFile {
    relative: String,
    absolute: PathBuf,
}

fn discover_python_files(
    repo_root: &Path,
    code_roots: &[PathBuf],
    exclude_globs: &[String],
) -> Result<Vec<DiscoveredPyFile>> {
    let mut out: Vec<DiscoveredPyFile> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let roots: Vec<PathBuf> = if code_roots.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        code_roots.to_vec()
    };
    for root in &roots {
        let abs = repo_root.join(root);
        if !abs.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&abs)
            .into_iter()
            .filter_entry(|e| !is_python_skip_dir(e))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            if ext != "py" {
                continue;
            }
            let rel = path
                .strip_prefix(repo_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if exclude_globs
                .iter()
                .any(|g| crate::lsp_indexer::simple_glob_match(g, &rel))
            {
                continue;
            }
            if !seen.insert(rel.clone()) {
                continue;
            }
            out.push(DiscoveredPyFile {
                relative: rel,
                absolute: repo_root.join(path.strip_prefix(repo_root).unwrap_or(path)),
            });
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

fn is_python_skip_dir(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    matches!(
        name,
        ".venv"
            | "venv"
            | "__pycache__"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".ruff_cache"
            | ".tox"
            | ".eggs"
            | ".git"
            | "node_modules"
            | "build"
            | "dist"
            | "site-packages"
    )
}

fn resolve_python_import(
    files: &[DiscoveredPyFile],
    src_roots: &[String],
    from_file: &str,
    module_path: &str,
) -> Option<String> {
    let module = module_path.trim();
    if module.is_empty() {
        return None;
    }
    if let Some(stripped) = module.strip_prefix('.') {
        // Relative import. Count leading dots, then resolve against
        // the importer's package. Each extra dot is one parent above.
        let mut dots = 1usize;
        let mut tail = stripped;
        while let Some(rest) = tail.strip_prefix('.') {
            dots += 1;
            tail = rest;
        }
        let from_parts: Vec<&str> = from_file.split('/').collect();
        // The file lives at `pkg/sub/file.py`; its package is
        // `pkg/sub`. A single dot resolves siblings of `file.py`.
        let pkg_len = from_parts.len().saturating_sub(1);
        if dots > pkg_len {
            return None;
        }
        let mut base: Vec<&str> = from_parts[..pkg_len.saturating_sub(dots - 1)].to_vec();
        if !tail.is_empty() {
            base.extend(tail.split('.'));
        }
        let candidate = base.join("/");
        return resolve_python_candidate(files, &candidate);
    }
    let base = module.replace('.', "/");
    // 1) Flat-layout match: `module` already encodes the on-disk path.
    if let Some(hit) = resolve_python_candidate(files, &base) {
        return Some(hit);
    }
    // 2) src-layout match: prepend each discovered source root.
    for root in src_roots {
        let candidate = if root.is_empty() {
            base.clone()
        } else {
            format!("{root}/{base}")
        };
        if candidate == base {
            continue; // already tried in step 1
        }
        if let Some(hit) = resolve_python_candidate(files, &candidate) {
            return Some(hit);
        }
    }
    None
}

fn resolve_python_candidate(files: &[DiscoveredPyFile], base: &str) -> Option<String> {
    let module_file = format!("{base}.py");
    let package_init = format!("{base}/__init__.py");
    for file in files {
        if file.relative == module_file || file.relative == package_init {
            return Some(file.relative.clone());
        }
    }
    None
}

/// Walk every `__init__.py` we discovered upwards until the parent
/// directory stops being a Python package (no `__init__.py`). That
/// parent is a *source root*: imports resolve against it as if it
/// were on `sys.path`. The repo root is included implicitly as the
/// empty-string root so flat layouts keep working. Returned roots
/// are de-duplicated and sorted by depth (deepest first) so we try
/// the most specific match before falling back.
fn discover_python_src_roots(files: &[DiscoveredPyFile]) -> Vec<String> {
    let mut init_dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for file in files {
        let trimmed = file.relative.strip_suffix("/__init__.py").or_else(|| {
            if file.relative == "__init__.py" {
                Some("")
            } else {
                None
            }
        });
        if let Some(dir) = trimmed {
            init_dirs.insert(dir.to_string());
        }
    }
    let mut roots: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Always include the empty root so unprefixed imports resolve in
    // flat repos.
    roots.insert(String::new());
    for dir in &init_dirs {
        let mut cur = dir.clone();
        loop {
            let parent = match cur.rfind('/') {
                Some(idx) => cur[..idx].to_string(),
                None => String::new(),
            };
            if !init_dirs.contains(&parent) {
                roots.insert(parent);
                break;
            }
            cur = parent;
        }
    }
    // Sort deepest-first so more specific roots win when multiple
    // resolutions are possible. Empty root naturally sorts last.
    let mut out: Vec<String> = roots.into_iter().collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.matches('/').count() + usize::from(!r.is_empty())));
    out
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
    fn from_options(options: &PythonIndexOptions) -> Self {
        if let Ok(env_cmd) = std::env::var(PYTHON_LSP_COMMAND_ENV) {
            if binary_on_path(&env_cmd) {
                return Self {
                    command: Some(env_cmd),
                    skip_reason: String::new(),
                };
            }
            return Self {
                command: None,
                skip_reason: format!(
                    "{PYTHON_LSP_COMMAND_ENV}=`{env_cmd}` 未找到对应可执行文件，已退化为 AST fallback"
                ),
            };
        }
        if let Some(cmd) = options.lsp_command.as_deref() {
            if binary_on_path(cmd) {
                return Self {
                    command: Some(cmd.to_string()),
                    skip_reason: String::new(),
                };
            }
            return Self {
                command: None,
                skip_reason: format!(
                    "`python.lsp_command = {cmd}` 未找到对应可执行文件，已退化为 AST fallback"
                ),
            };
        }
        if !options.disable_venv_discovery {
            for relative in [
                ".venv/bin/basedpyright-langserver",
                ".venv/bin/pyright-langserver",
                ".venv/bin/pylsp",
            ] {
                let candidate = options.repo_root.join(relative);
                if candidate.is_file() {
                    return Self {
                        command: Some(candidate.to_string_lossy().into_owned()),
                        skip_reason: String::new(),
                    };
                }
            }
        }
        for fallback in ["basedpyright-langserver", "pyright-langserver", "pylsp"] {
            if binary_on_path(fallback) {
                return Self {
                    command: Some(fallback.to_string()),
                    skip_reason: String::new(),
                };
            }
        }
        Self {
            command: None,
            skip_reason:
                "未在 PATH / .venv 中找到 pyright/basedpyright/pylsp，已退化为 AST fallback".into(),
        }
    }
}

// `sha2::Sha256` is reachable via the existing engine dep tree; we use
// it directly here to hash file contents for the AST-only batch.
use sha2::Digest;

#[cfg(test)]
mod tests {
    use super::*;
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
    fn ast_pass_emits_imports_pytest_tests_and_structural_symbols_without_lsp() {
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

        assert_eq!(result.resolver_used, PYTHON_AST_INDEXER_NAME);
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
        // We seed a tiny FastAPI / Celery / Click triple, run the AST
        // pass, and verify the resulting Node has a populated
        // `metadata_json` field that round-trips through serde into
        // a FrameworkRole the engine recognises as an entry point.
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

    #[test]
    fn resolve_python_import_handles_packages_and_relative_imports() {
        let files = vec![
            DiscoveredPyFile {
                relative: "app/__init__.py".into(),
                absolute: PathBuf::from("/tmp/app/__init__.py"),
            },
            DiscoveredPyFile {
                relative: "app/greeter.py".into(),
                absolute: PathBuf::from("/tmp/app/greeter.py"),
            },
            DiscoveredPyFile {
                relative: "app/utils.py".into(),
                absolute: PathBuf::from("/tmp/app/utils.py"),
            },
        ];
        let src_roots = discover_python_src_roots(&files);
        assert_eq!(
            resolve_python_import(&files, &src_roots, "tests/test_greeter.py", "app.greeter"),
            Some("app/greeter.py".into())
        );
        assert_eq!(
            resolve_python_import(&files, &src_roots, "tests/test_greeter.py", "app"),
            Some("app/__init__.py".into())
        );
        assert_eq!(
            resolve_python_import(&files, &src_roots, "app/greeter.py", ".utils"),
            Some("app/utils.py".into())
        );
        // Non-resolvable imports drop quietly so we never inject fake
        // file nodes for `import os` etc.
        assert_eq!(
            resolve_python_import(&files, &src_roots, "app/greeter.py", "os"),
            None
        );
    }

    #[test]
    fn resolve_python_import_handles_src_layout_via_discovered_roots() {
        // Mirrors atagent's `backend/app/...` layout. `backend/` is
        // not itself a package (no `__init__.py` directly under it)
        // but every dir below it is — making `backend/` the source
        // root that `from app.core.config import ...` resolves against.
        let files = vec![
            DiscoveredPyFile {
                relative: "backend/app/__init__.py".into(),
                absolute: PathBuf::from("/tmp/backend/app/__init__.py"),
            },
            DiscoveredPyFile {
                relative: "backend/app/main.py".into(),
                absolute: PathBuf::from("/tmp/backend/app/main.py"),
            },
            DiscoveredPyFile {
                relative: "backend/app/core/__init__.py".into(),
                absolute: PathBuf::from("/tmp/backend/app/core/__init__.py"),
            },
            DiscoveredPyFile {
                relative: "backend/app/core/config.py".into(),
                absolute: PathBuf::from("/tmp/backend/app/core/config.py"),
            },
            DiscoveredPyFile {
                relative: "backend/tests/test_config.py".into(),
                absolute: PathBuf::from("/tmp/backend/tests/test_config.py"),
            },
        ];
        let src_roots = discover_python_src_roots(&files);
        assert!(
            src_roots.contains(&"backend".to_string()),
            "expected `backend` in src roots, got {src_roots:?}"
        );
        assert_eq!(
            resolve_python_import(
                &files,
                &src_roots,
                "backend/tests/test_config.py",
                "app.core.config"
            ),
            Some("backend/app/core/config.py".into())
        );
        assert_eq!(
            resolve_python_import(&files, &src_roots, "backend/app/main.py", "app.core.config"),
            Some("backend/app/core/config.py".into())
        );
    }

    #[test]
    fn discover_python_src_roots_includes_repo_root_for_flat_layout() {
        let files = vec![DiscoveredPyFile {
            relative: "app/foo.py".into(),
            absolute: PathBuf::from("/tmp/app/foo.py"),
        }];
        let roots = discover_python_src_roots(&files);
        assert!(
            roots.contains(&String::new()),
            "expected `\"\"` in {roots:?}"
        );
    }
}
