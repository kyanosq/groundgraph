//! Top-level orchestration for `specslice index`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::docs_indexer::{index_docs, DocsIndexOptions, DocsIndexResult, DOCS_INDEXER_NAME};
use crate::go_indexer::{index_go, GoIndexOptions, GoIndexResult, GO_LSP_COMMAND_ENV};
use crate::java_indexer::{index_java, JavaIndexOptions, JavaIndexResult, JAVA_LSP_COMMAND_ENV};
use crate::links_indexer::{index_links, LinksIndexOptions, LinksIndexResult, LINKS_INDEXER_NAME};
use crate::python_indexer::{
    index_python, PythonIndexOptions, PythonIndexResult, PYTHON_LSP_COMMAND_ENV,
};
use crate::requirements_md_indexer::{
    index_requirements_md, RequirementsMdIndexOptions, RequirementsMdIndexResult,
    DEFAULT_REQUIREMENTS_DIR, REQUIREMENTS_MD_INDEXER_NAME,
};
use crate::rust_indexer::{index_rust, RustIndexOptions, RustIndexResult, RUST_INDEXER_NAME};
use crate::swift_indexer::{
    index_swift, SwiftIndexOptions, SwiftIndexResult, SWIFT_LSP_COMMAND_ENV,
};
use crate::treesitter::{self, TsIndexOptions};
use crate::typescript_indexer::{
    index_typescript, TypescriptIndexOptions, TypescriptIndexResult, TYPESCRIPT_LSP_COMMAND_ENV,
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
    /// P23.9 — Markdown requirements (`.specslice/requirements/*.md`). `None`
    /// when the links phase was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements_md: Option<RequirementsMdIndexResult>,
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
    /// P21 — Rust adapter (tree-sitter, in-process). `None` when the
    /// adapter is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust: Option<RustIndexResult>,
    /// P22 — unified tree-sitter breadth backend results, one entry per
    /// language that produced output (in `treesitter.languages` order).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub treesitter: Vec<TreeSitterLangResult>,
    /// ADR-0001 R1 — offline SCIP overlay (`.specslice/scip/*.scip`). `None`
    /// when code indexing is skipped or `enrichment.scip = false`; present with
    /// zero counts when enabled but no `.scip` file is on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scip: Option<crate::scip_overlay::ScipOverlayResult>,
    /// ADR-0001 R2 — per-language SCIP indexer auto-invocation outcomes
    /// (`Generated` / `Skipped` / `Unsupported` / `Failed`). Empty when code
    /// indexing is skipped or `enrichment.scip = false`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scip_runs: Vec<crate::scip_runner::ScipRunOutcome>,
}

/// Per-language outcome of the unified tree-sitter pass.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TreeSitterLangResult {
    pub language: String,
    pub files: usize,
    pub symbols: usize,
    pub imports: usize,
    pub resolver_used: String,
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
        // Always clear prior Dart rows so toggling Dart off (empty `code.paths`)
        // purges stale structure instead of leaving orphans behind.
        store
            .clear_indexer_outputs(crate::dart_indexer::DART_INDEXER_NAME)
            .context("clearing previous dart index outputs")?;
        store
            .clear_indexer_outputs(crate::dart_indexer::RESOLVER_DART_ANALYZER)
            .context("clearing previous dart analyzer index outputs")?;
        // The Dart structural pass only runs when a code root is configured. An
        // empty `code.paths` is an *explicit exclusion* (Dart absent from the
        // unified `languages:` list, or `code.paths: []`) and must scan nothing
        // — never fall back to the whole repo. Legacy configs keep the default
        // `[lib, test]` roots, so their behaviour is unchanged.
        if !config.code.paths.is_empty() {
            let code = crate::dart_indexer::index_dart(
                &mut store,
                &crate::dart_indexer::DartIndexOptions {
                    repo_root: options.repo_root.clone(),
                    code_roots: config.code.paths.iter().map(PathBuf::from).collect(),
                    exclude_globs: config.code.exclude.clone(),
                    disable_analyzer: !config.enrichment.analyzer,
                },
            )
            .context("indexing Dart sources")?;
            result.code = Some(code);
        }

        // P11/P23.5 — opt-in Swift / Go adapters. Both are gated behind the
        // `swift.enabled` / `go.enabled` keys so existing Dart-only
        // workspaces keep their current behaviour. Structure comes from the
        // tree-sitter driver; an optional LSP server overlays only
        // `Calls` / `References`. The adapters clear their own prior
        // `*_treesitter` + `*_lsp` rows and honour
        // `SPECSLICE_SWIFT_LSP_BIN` / `SPECSLICE_GO_LSP_BIN` overrides.
        if config.swift.enabled {
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

        // P11/P23.4 — Go adapter. The tree-sitter driver owns structure; an
        // optional `gopls` server overlays only `Calls` / `References`.
        // `index_go` clears its own prior `go_treesitter` + `go_lsp` rows.
        if config.go.enabled {
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

        // P16/P23.1 — Python adapter. The in-process tree-sitter driver
        // owns the structural graph; an optional LSP server overlays only
        // `Calls` / `References`. `index_python` clears its own prior
        // `python_treesitter` + `python_lsp` rows, so no pre-clear here.
        if config.python.enabled {
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

        // P20/P23.2 — TypeScript adapter. The tree-sitter driver (`.ts` +
        // `.tsx`) owns structure; an optional LSP server overlays only
        // `Calls` / `References`. `index_typescript` clears its own prior
        // `typescript_treesitter` + `typescript_lsp` rows.
        if config.typescript.enabled {
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

        // P20/P23.3 — Java adapter. The tree-sitter driver owns structure;
        // an optional `jdtls` server overlays only `Calls` / `References`.
        // `index_java` clears its own prior `java_treesitter` + `java_lsp`
        // rows.
        if config.java.enabled {
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

        // P21 — Rust adapter. No LSP tier: the tree-sitter grammar is
        // linked in, so this is always deterministic and fast. Gated by
        // `rust.enabled` to keep non-Rust workspaces untouched.
        if config.rust.enabled {
            store
                .clear_indexer_outputs(RUST_INDEXER_NAME)
                .context("clearing previous Rust tree-sitter outputs")?;
            let rust_paths = config.rust.paths_or(&["crates", "src"]);
            let rust_options = RustIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: rust_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.rust.exclude.clone(),
            };
            let rust = index_rust(&mut store, &rust_options).context("indexing Rust sources")?;
            result.rust = Some(rust);
        }

        // P22 — unified tree-sitter breadth backend. One pass drives the
        // in-process generic driver for every configured language. Rust
        // is skipped here when the legacy `rust:` switch already ran it,
        // so the two paths never double-index.
        if config.treesitter.enabled {
            let roots: Vec<PathBuf> = config
                .treesitter
                .roots()
                .iter()
                .map(PathBuf::from)
                .collect();
            let mut seen: std::collections::BTreeSet<&'static str> =
                std::collections::BTreeSet::new();
            for lang in &config.treesitter.languages {
                let Some(spec) = treesitter::spec_for_language(lang) else {
                    continue; // unknown language id: skip, never abort.
                };
                if !seen.insert(spec.language_id) {
                    continue; // duplicate entry / alias.
                }
                if spec.language_id == "rust" && config.rust.enabled {
                    continue; // legacy rust switch owns Rust this run.
                }
                if spec.language_id == "python" && config.python.enabled {
                    continue; // Python adapter (tree-sitter + optional LSP) owns Python.
                }
                if spec.language_id == "typescript" && config.typescript.enabled {
                    continue; // TypeScript adapter (tree-sitter + optional LSP) owns TS/TSX.
                }
                if spec.language_id == "java" && config.java.enabled {
                    continue; // Java adapter (tree-sitter + optional LSP) owns Java.
                }
                if spec.language_id == "go" && config.go.enabled {
                    continue; // Go adapter (tree-sitter + optional LSP) owns Go.
                }
                if spec.language_id == "swift" && config.swift.enabled {
                    continue; // Swift adapter (tree-sitter + optional LSP) owns Swift.
                }
                let name = treesitter::indexer_name(spec);
                store
                    .clear_indexer_outputs(&name)
                    .with_context(|| format!("clearing previous {name} outputs"))?;
                let ts = treesitter::index_repo_with_spec(
                    &mut store,
                    spec,
                    &TsIndexOptions {
                        repo_root: options.repo_root.clone(),
                        code_roots: roots.clone(),
                        exclude_globs: config.treesitter.exclude.clone(),
                        resolution_paths: Vec::new(),
                    },
                )
                .with_context(|| format!("indexing {} sources", spec.language_id))?;
                result.treesitter.push(TreeSitterLangResult {
                    language: spec.language_id.to_string(),
                    files: ts.files,
                    symbols: ts.symbols,
                    imports: ts.imports,
                    resolver_used: ts.resolver_used,
                });
            }
        }

        // ADR-0001 R1/R2 — SCIP is the single precision layer. First
        // auto-invoke each indexed language's installed SCIP indexer
        // (`rust-analyzer scip`, `scip-go`, …) to regenerate
        // `.specslice/scip/<lang>.scip` — a one-shot batch, silently skipped
        // when the binary is absent — then overlay every `.scip` as
        // high-confidence `Calls`/`References` edges. Runs *last*, after every
        // structural pass, so the precise edges bind to symbols that already
        // exist. A no-op when no indexer is installed and no `.scip` is present.
        if config.enrichment.scip {
            let languages = indexed_languages(&config);
            result.scip_runs = crate::scip_runner::run_indexers(&options.repo_root, &languages);
            let scip = crate::scip_overlay::ingest_scip_overlay(&mut store, &options.repo_root)
                .context("ingesting SCIP overlay")?;
            result.scip = Some(scip);
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

        // P23.9 — Markdown requirements. Runs after the manifest so both
        // sources contribute requirement→artifact edges into the same graph;
        // `links.yaml` stays supported but `.specslice/requirements/*.md` is
        // the recommended, human-friendly format.
        store
            .clear_indexer_outputs(REQUIREMENTS_MD_INDEXER_NAME)
            .context("clearing previous requirements markdown outputs")?;
        let requirements_md = index_requirements_md(
            &mut store,
            &RequirementsMdIndexOptions {
                repo_root: options.repo_root.clone(),
                requirements_dir: PathBuf::from(DEFAULT_REQUIREMENTS_DIR),
            },
        )
        .context("indexing markdown requirements")?;
        result.requirements_md = Some(requirements_md);
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
    // P23.7 — fold the unified `languages:` selector onto the legacy
    // per-language switches so the index pass below has a single source.
    Ok(cfg.normalized())
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    let raw = Path::new(&config.storage.path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    }
}

/// Canonical ids of every language this run indexed (post-`normalized`), used
/// to decide which SCIP indexers to auto-invoke. Mirrors the enable-gates of
/// the structural passes above; `run_indexers` dedupes and skips ids without
/// an auto-invoke spec (e.g. `swift`, `java`, `c`, `cpp`).
fn indexed_languages(config: &EngineConfig) -> Vec<String> {
    let mut langs: Vec<String> = Vec::new();
    if !config.code.paths.is_empty() {
        langs.push("dart".to_string());
    }
    if config.swift.enabled {
        langs.push("swift".to_string());
    }
    if config.go.enabled {
        langs.push("go".to_string());
    }
    if config.python.enabled {
        langs.push("python".to_string());
    }
    if config.typescript.enabled {
        langs.push("typescript".to_string());
    }
    if config.java.enabled {
        langs.push("java".to_string());
    }
    if config.rust.enabled {
        langs.push("rust".to_string());
    }
    for lang in &config.treesitter.languages {
        if let Some(canon) = crate::config::canonical_language_id(lang) {
            langs.push(canon.to_string());
        }
    }
    langs
}
