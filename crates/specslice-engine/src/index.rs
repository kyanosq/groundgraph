//! Top-level orchestration for `specslice index`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::docs_indexer::{index_docs, DocsIndexOptions, DocsIndexResult, DOCS_INDEXER_NAME};
use crate::go_indexer::{
    index_go, GoIndexOptions, GoIndexResult, GO_INDEXER_NAME, GO_LSP_COMMAND_ENV,
};
use crate::java_indexer::{
    index_java, JavaIndexOptions, JavaIndexResult, JAVA_AST_INDEXER_NAME, JAVA_INDEXER_NAME,
    JAVA_LSP_COMMAND_ENV,
};
use crate::links_indexer::{index_links, LinksIndexOptions, LinksIndexResult, LINKS_INDEXER_NAME};
use crate::python_indexer::{
    index_python, PythonIndexOptions, PythonIndexResult, PYTHON_AST_INDEXER_NAME,
    PYTHON_INDEXER_NAME, PYTHON_LSP_COMMAND_ENV,
};
use crate::swift_indexer::{
    index_swift, SwiftIndexOptions, SwiftIndexResult, SWIFT_INDEXER_NAME, SWIFT_LSP_COMMAND_ENV,
};
use crate::typescript_indexer::{
    index_typescript, TypescriptIndexOptions, TypescriptIndexResult, TYPESCRIPT_AST_INDEXER_NAME,
    TYPESCRIPT_INDEXER_NAME, TYPESCRIPT_LSP_COMMAND_ENV,
};

#[derive(Debug, Clone)]
pub struct IndexOptions {
    pub repo_root: PathBuf,
    pub include_docs: bool,
    pub include_code: bool,
    pub include_links: bool,
}

impl IndexOptions {
    pub fn all(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            include_docs: true,
            include_code: true,
            include_links: true,
        }
    }

    pub fn docs_only(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            include_docs: true,
            include_code: false,
            include_links: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IndexResult {
    pub docs: Option<DocsIndexResult>,
    pub code: Option<crate::dart_indexer::DartIndexResult>,
    pub links: Option<LinksIndexResult>,
    /// P11 — when the Swift adapter is enabled in `.specslice.yaml`,
    /// this holds the stats from the LSP-driven indexer. `None` when
    /// the adapter is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swift: Option<SwiftIndexResult>,
    /// P11 — Go adapter counterpart. Same semantics as `swift`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go: Option<GoIndexResult>,
    /// P16 — Python adapter (LSP-first, AST 补强). `None` when the
    /// adapter is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonIndexResult>,
    /// P20 — TypeScript adapter (LSP-first, AST 补强). `None` when
    /// the adapter is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typescript: Option<TypescriptIndexResult>,
    /// P20 — Java adapter (LSP-first, AST 补强). `None` when the
    /// adapter is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java: Option<JavaIndexResult>,
}

pub fn index_repository(options: IndexOptions) -> Result<IndexResult> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let mut store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    store
        .migrate()
        .with_context(|| format!("running migrations on {}", db_path.display()))?;

    let mut result = IndexResult::default();

    if options.include_docs {
        store
            .clear_indexer_outputs(DOCS_INDEXER_NAME)
            .context("clearing previous docs index outputs")?;
        let docs_options = DocsIndexOptions {
            repo_root: options.repo_root.clone(),
            doc_roots: config.docs.paths.iter().map(PathBuf::from).collect(),
            include_globs: config.docs.include.clone(),
        };
        let docs = index_docs(&mut store, &docs_options).context("indexing docs")?;
        result.docs = Some(docs);
    }

    if options.include_code {
        store
            .clear_indexer_outputs(crate::dart_indexer::DART_INDEXER_NAME)
            .context("clearing previous dart index outputs")?;
        store
            .clear_indexer_outputs(crate::dart_indexer::RESOLVER_DART_ANALYZER)
            .context("clearing previous dart analyzer index outputs")?;
        let code = crate::dart_indexer::index_dart(
            &mut store,
            &crate::dart_indexer::DartIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: config.code.paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.code.exclude.clone(),
            },
        )
        .context("indexing Dart sources")?;
        result.code = Some(code);

        // P11 — opt-in Swift / Go adapters. Both are gated behind the
        // `swift.enabled` / `go.enabled` keys so existing Dart-only
        // workspaces keep their current behaviour. The adapters also
        // honour `SPECSLICE_SWIFT_LSP_BIN` / `SPECSLICE_GO_LSP_BIN`
        // env vars for ad-hoc binary overrides.
        if config.swift.enabled {
            store
                .clear_indexer_outputs(SWIFT_INDEXER_NAME)
                .context("clearing previous Swift LSP outputs")?;
            let swift_paths = config.swift.paths_or(&["Sources", "Tests"]);
            let swift_options = SwiftIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: swift_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.swift.exclude.clone(),
                lsp_command: std::env::var(SWIFT_LSP_COMMAND_ENV)
                    .ok()
                    .or_else(|| config.swift.lsp_command.clone()),
            };
            let swift =
                index_swift(&mut store, &swift_options).context("indexing Swift sources")?;
            result.swift = Some(swift);
        }

        if config.go.enabled {
            store
                .clear_indexer_outputs(GO_INDEXER_NAME)
                .context("clearing previous Go LSP outputs")?;
            let go_paths = config.go.paths_or(&["."]);
            let go_options = GoIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: go_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.go.exclude.clone(),
                lsp_command: std::env::var(GO_LSP_COMMAND_ENV)
                    .ok()
                    .or_else(|| config.go.lsp_command.clone()),
            };
            let go = index_go(&mut store, &go_options).context("indexing Go sources")?;
            result.go = Some(go);
        }

        // P16 — Python adapter (LSP first, AST always). Both
        // contributors share a `clear_indexer_outputs` reset so we do
        // not leave stale rows from a previous resolver.
        if config.python.enabled {
            store
                .clear_indexer_outputs(PYTHON_INDEXER_NAME)
                .context("clearing previous Python LSP outputs")?;
            store
                .clear_indexer_outputs(PYTHON_AST_INDEXER_NAME)
                .context("clearing previous Python AST outputs")?;
            let python_paths = config.python.paths_or(&["."]);
            let python_options = PythonIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: python_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.python.exclude.clone(),
                lsp_command: std::env::var(PYTHON_LSP_COMMAND_ENV)
                    .ok()
                    .or_else(|| config.python.lsp_command.clone()),
                disable_venv_discovery: false,
            };
            let python =
                index_python(&mut store, &python_options).context("indexing Python sources")?;
            result.python = Some(python);
        }

        // P20 — TypeScript adapter (LSP first, AST always).
        if config.typescript.enabled {
            store
                .clear_indexer_outputs(TYPESCRIPT_INDEXER_NAME)
                .context("clearing previous TypeScript LSP outputs")?;
            store
                .clear_indexer_outputs(TYPESCRIPT_AST_INDEXER_NAME)
                .context("clearing previous TypeScript AST outputs")?;
            let ts_paths = config.typescript.paths_or(&["src", "tests", "test"]);
            let ts_options = TypescriptIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: ts_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.typescript.exclude.clone(),
                lsp_command: std::env::var(TYPESCRIPT_LSP_COMMAND_ENV)
                    .ok()
                    .or_else(|| config.typescript.lsp_command.clone()),
            };
            let ts =
                index_typescript(&mut store, &ts_options).context("indexing TypeScript sources")?;
            result.typescript = Some(ts);
        }

        // P20 — Java adapter (LSP first, AST always).
        if config.java.enabled {
            store
                .clear_indexer_outputs(JAVA_INDEXER_NAME)
                .context("clearing previous Java LSP outputs")?;
            store
                .clear_indexer_outputs(JAVA_AST_INDEXER_NAME)
                .context("clearing previous Java AST outputs")?;
            let java_paths = config.java.paths_or(&["src"]);
            let java_options = JavaIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: java_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.java.exclude.clone(),
                lsp_command: std::env::var(JAVA_LSP_COMMAND_ENV)
                    .ok()
                    .or_else(|| config.java.lsp_command.clone()),
            };
            let java = index_java(&mut store, &java_options).context("indexing Java sources")?;
            result.java = Some(java);
        }
    }

    if options.include_links {
        store
            .clear_indexer_outputs(LINKS_INDEXER_NAME)
            .context("clearing previous links index outputs")?;
        let links = index_links(
            &mut store,
            &LinksIndexOptions {
                repo_root: options.repo_root.clone(),
                manifest_path: PathBuf::from(&config.links.path),
            },
        )
        .context("indexing external links manifest")?;
        result.links = Some(links);
    }

    Ok(result)
}

fn load_config(repo_root: &Path) -> Result<EngineConfig> {
    let path = repo_root.join(DEFAULT_CONFIG_FILE_NAME);
    if !path.exists() {
        anyhow::bail!(
            "no SpecSlice workspace at {}: run `specslice init` first",
            repo_root.display()
        );
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let cfg: EngineConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("parsing config {}", path.display()))?;
    Ok(cfg)
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = Path::new(&config.storage.path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    }
}
