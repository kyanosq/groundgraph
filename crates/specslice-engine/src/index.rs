//! Top-level orchestration for `specslice index`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::docs_indexer::{index_docs, DocsIndexOptions, DocsIndexResult, DOCS_INDEXER_NAME};
use crate::go_indexer::{index_go, GoIndexOptions, GoIndexResult};
use crate::java_indexer::{index_java, JavaIndexOptions, JavaIndexResult};
use crate::links_indexer::{index_links, LinksIndexOptions, LinksIndexResult, LINKS_INDEXER_NAME};
use crate::python_indexer::{index_python, PythonIndexOptions, PythonIndexResult};
use crate::requirements_md_indexer::{
    index_requirements_md, RequirementsMdIndexOptions, RequirementsMdIndexResult,
    DEFAULT_REQUIREMENTS_DIR, REQUIREMENTS_MD_INDEXER_NAME,
};
use crate::rust_indexer::{index_rust, RustIndexOptions, RustIndexResult, RUST_INDEXER_NAME};
use crate::swift_indexer::{
    index_swift, SwiftIndexOptions, SwiftIndexResult, SWIFT_LSP_COMMAND_ENV,
};
use crate::treesitter::{self, TsIndexOptions};
use crate::typescript_indexer::{index_typescript, TypescriptIndexOptions, TypescriptIndexResult};

/// #187: opt-in to executing commands that came from the *repo's*
/// `.specslice.yaml`. Off by default so pointing `specslice index` at an
/// untrusted clone can never run an attacker-chosen binary.
pub const TRUST_CONFIG_COMMANDS_ENV: &str = "SPECSLICE_TRUST_CONFIG_COMMANDS";

/// Decide whether a command string that originated in the repo-controlled
/// config should be honoured (#187).
///
/// `.specslice.yaml` is part of the *target* repository, so a poisoned
/// `swift.lsp_command: /tmp/payload.sh` would otherwise run on `index`. We
/// honour such a value only when the operator explicitly trusts the workspace
/// (`trusted == true`); otherwise we drop it and return a human-readable notice
/// so the skip is visible rather than silent. Operator-set env vars
/// (`SPECSLICE_SWIFT_LSP_BIN`, `SPECSLICE_SCIP_*_BIN`, …) bypass this gate by
/// construction — they are not repo-controlled.
///
/// Pure (the `trusted` flag is read from the environment by the caller) so the
/// policy is unit-testable without mutating process env.
pub(crate) fn resolve_config_command(
    trusted: bool,
    field: &str,
    value: Option<&str>,
) -> (Option<String>, Option<String>) {
    match value {
        None => (None, None),
        Some(v) if trusted => (Some(v.to_string()), None),
        Some(v) => (
            None,
            Some(format!(
                "ignoring `{field}: {v}` from .specslice.yaml — repo-provided commands are not \
                 executed by default; set {TRUST_CONFIG_COMMANDS_ENV}=1 to trust this workspace, \
                 or pass the command via its operator env var instead"
            )),
        ),
    }
}

/// Env-reading wrapper over [`resolve_config_command`]; prints the "ignored"
/// notice to stderr and returns the command to honour (if any).
fn config_command(field: &str, value: Option<&str>) -> Option<String> {
    let trusted = std::env::var_os(TRUST_CONFIG_COMMANDS_ENV).is_some();
    let (cmd, notice) = resolve_config_command(trusted, field, value);
    if let Some(notice) = notice {
        eprintln!("specslice: {notice}");
    }
    cmd
}

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
    /// Fulltext content layer (FTS5/BM25) rebuild stats — what makes
    /// comment-only / doc-body-only phrases searchable. Runs on every
    /// `index`, mirroring the final node set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulltext: Option<crate::fulltext_indexer::FulltextIndexResult>,
}

/// Per-language outcome of the unified tree-sitter pass.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TreeSitterLangResult {
    pub language: String,
    pub files: usize,
    pub symbols: usize,
    pub imports: usize,
    /// Files skipped because parsing exceeded the per-file budget
    /// (tree-sitter error recovery on fixture corpora).
    #[serde(default)]
    pub parse_timeouts: usize,
    /// Files skipped because they exceeded the per-file size budget.
    #[serde(default)]
    pub skipped_oversized: usize,
    pub resolver_used: String,
}

/// Wall-clock phase reporter, enabled by `SPECSLICE_TIMING=1`. Prints each
/// index phase's elapsed time to stderr so a slow run can be attributed to
/// parse / ingest / scip / fulltext without a sampling profiler.
struct PhaseTimer {
    enabled: bool,
    last: std::time::Instant,
}

impl PhaseTimer {
    fn new() -> Self {
        Self {
            enabled: std::env::var_os("SPECSLICE_TIMING").is_some(),
            last: std::time::Instant::now(),
        }
    }
    fn mark(&mut self, phase: &str) {
        if self.enabled {
            eprintln!(
                "[timing] {phase}: {:.2}s",
                self.last.elapsed().as_secs_f64()
            );
        }
        self.last = std::time::Instant::now();
    }
}

pub fn index_repository(options: IndexOptions) -> Result<IndexResult> {
    let mut timer = PhaseTimer::new();
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let mut store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    store
        .migrate()
        .with_context(|| format!("running migrations on {}", db_path.display()))?;

    // One bulk transaction for the whole ingest. Autocommit mode turns the
    // 10^5+ upserts of a large repo into as many WAL commits; the django
    // profile was dominated by pread/pwrite/fsync, not CPU. A single commit
    // amortizes that to one WAL append + one checkpoint. Early `?` returns
    // drop the connection, which rolls the half-built generation back — the
    // previous complete index stays untouched.
    //
    // Deliberately all-or-nothing (issues2.md #34): per-indexer commits
    // would leave a *mixed-generation* graph on failure (new dart rows,
    // old java rows), and the cross-indexer linking passes that run later
    // (schema routes → callables, docs → symbols) would build edges
    // against nodes from different generations. A failed run costing a
    // re-index beats a silently inconsistent graph — the index is a
    // rebuildable cache, never the source of truth.
    store.begin_bulk().context("opening bulk write session")?;

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
        timer.mark("docs");
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
            timer.mark("dart");
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
                // Operator env var is trusted; a command from the repo's
                // `.specslice.yaml` is gated behind `SPECSLICE_TRUST_CONFIG_COMMANDS` (#187).
                lsp_command: std::env::var(SWIFT_LSP_COMMAND_ENV).ok().or_else(|| {
                    config_command("swift.lsp_command", config.swift.lsp_command.as_deref())
                }),
            };
            let swift =
                index_swift(&mut store, &swift_options).context("indexing Swift sources")?;
            result.swift = Some(swift);
            timer.mark("swift");
        }

        // P11/P23.4 — Go adapter. The tree-sitter driver owns structure +
        // heuristic `Calls`/`References`; precise edges come from the SCIP
        // overlay (`scip-go`). `index_go` clears its own prior `go_treesitter`
        // rows (and any retired `go_lsp` rows).
        if config.go.enabled {
            let go_paths = config.go.paths_or(&["."]);
            let go_options = GoIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: go_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.go.exclude.clone(),
            };
            let go = index_go(&mut store, &go_options).context("indexing Go sources")?;
            result.go = Some(go);
            timer.mark("go");
        }

        // P16/P23.1 — Python adapter. The in-process tree-sitter driver owns
        // the structural graph + heuristic `Calls`/`References`; precise edges
        // come from a SCIP overlay (`scip-python`) when present. `index_python`
        // clears its own prior `python_treesitter` rows (and retired
        // `python_lsp` rows), so no pre-clear here.
        if config.python.enabled {
            let python_paths = config.python.paths_or(&["."]);
            let python_options = PythonIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: python_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.python.exclude.clone(),
            };
            let python =
                index_python(&mut store, &python_options).context("indexing Python sources")?;
            result.python = Some(python);
            timer.mark("python_adapter");
        }

        // P20/P23.2 — TypeScript adapter. The tree-sitter driver (`.ts` +
        // `.tsx`/`.js`/`.vue`) owns structure + heuristic `Calls`/`References`;
        // precise edges come from the SCIP overlay (`scip-typescript`).
        // `index_typescript` clears its own prior `typescript_treesitter` rows
        // (and any retired `typescript_lsp` rows).
        if config.typescript.enabled {
            let ts_paths = config.typescript.paths_or(&["src", "tests", "test"]);
            let ts_options = TypescriptIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: ts_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.typescript.exclude.clone(),
            };
            let ts =
                index_typescript(&mut store, &ts_options).context("indexing TypeScript sources")?;
            result.typescript = Some(ts);
            timer.mark("typescript_adapter");
        }

        // P20/P23.3 — Java adapter. The tree-sitter driver owns structure +
        // heuristic `Calls`/`References`; precise edges come from a SCIP
        // overlay (`scip-java`) when present. `index_java` clears its own prior
        // `java_treesitter` rows (and any retired `java_lsp` rows).
        if config.java.enabled {
            let java_paths = config.java.paths_or(&["src"]);
            let java_options = JavaIndexOptions {
                repo_root: options.repo_root.clone(),
                code_roots: java_paths.iter().map(PathBuf::from).collect(),
                exclude_globs: config.java.exclude.clone(),
            };
            let java = index_java(&mut store, &java_options).context("indexing Java sources")?;
            result.java = Some(java);
            timer.mark("java");
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
            timer.mark("rust_adapter");
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
                    parse_timeouts: ts.parse_timeouts,
                    skipped_oversized: ts.skipped_oversized,
                    resolver_used: ts.resolver_used,
                });
                timer.mark(&format!("treesitter:{}", spec.language_id));
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
            // Only auto-invoke a language's SCIP indexer when that language
            // actually contributed files this run. A repo selecting several
            // `treesitter.languages` but holding only Rust would otherwise spawn
            // `scip-go` / `scip-python` / `scip-typescript` on an empty tree —
            // wasted subprocesses that emit empty `.scip` and noisy "generated"
            // lines. Filtering by real file counts keeps `index` fast and its
            // output honest.
            let languages: Vec<String> = indexed_languages(&config)
                .into_iter()
                .filter(|lang| language_file_count(&result, lang) > 0)
                .collect();
            result.scip_runs = crate::scip_runner::run_indexers(&options.repo_root, &languages);
            let scip = crate::scip_overlay::ingest_scip_overlay(&mut store, &options.repo_root)
                .context("ingesting SCIP overlay")?;
            result.scip = Some(scip);
            timer.mark("scip");
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
        timer.mark("links");

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
        timer.mark("requirements_md");
    }

    // Content layer LAST: it mirrors whatever node set the passes above just
    // produced (docs sections, code symbols, requirements), reading each
    // source file once and rebuilding `node_fts` wholesale. This is what lets
    // `search` rank comment-only / doc-body-only phrases (BM25) instead of
    // being blind to anything outside ids/names/paths.
    let fulltext = crate::fulltext_indexer::rebuild_fulltext_index(&mut store, &options.repo_root)
        .context("rebuilding fulltext content layer")?;
    result.fulltext = Some(fulltext);
    timer.mark("fulltext");

    store
        .commit_bulk()
        .context("committing bulk write session")?;
    timer.mark("commit");

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
    let cfg: EngineConfig = serde_yml::from_str(&contents)
        .with_context(|| format!("parsing config {}", path.display()))?;
    // #72 — warn (don't fail) when the file was written by a newer specslice
    // whose config keys this build may misinterpret.
    if let Some(notice) = cfg.schema_version_notice() {
        eprintln!("specslice: {notice}");
    }
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
    // Dart is special: its bespoke analyzer sidecar (`dart_analyzer`) is the
    // authoritative precision source — it emits Dart-domain semantic edges
    // (Riverpod / Hive / navigation / IAP) that generic SCIP cannot, *and*
    // supplies high-confidence Calls/References. Auto-invoking `scip_dart`
    // alongside it would make the overlay suppress the sidecar's
    // Calls/References in favour of scip's generic ones. So `scip_dart` only
    // fills the gap when the sidecar is disabled (`enrichment.analyzer=false`),
    // upgrading the `dart_lightweight` heuristic to SCIP precision.
    if !config.code.paths.is_empty() && !config.enrichment.analyzer {
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

/// Canonical ids of languages that have enough first-party source to be
/// auto-elected by `init` but that the current config does **not** index — the
/// silent blind spots a stale config leaves behind (e.g. a repo `init`-ed when
/// it was pure Python that later grew a `web/` TypeScript front-end). Returns a
/// sorted, deduplicated list; empty when the config already covers everything
/// detection would elect. Threshold matches `init`'s own elector (≥3 files /
/// ≥25% of sources / a language manifest), so stray scripts never trip it.
pub fn unindexed_present_languages(repo_root: &Path) -> Result<Vec<String>> {
    let config = load_config(repo_root)?;
    Ok(unindexed_languages_against_config(repo_root, &config))
}

/// Pure set-difference half of [`unindexed_present_languages`], split out so it
/// can be unit-tested without a real `.specslice.yaml` on disk.
fn unindexed_languages_against_config(repo_root: &Path, config: &EngineConfig) -> Vec<String> {
    let detected: std::collections::BTreeSet<String> =
        crate::init::detect_language_selections(repo_root)
            .into_iter()
            .map(|sel| sel.id)
            .collect();
    let mut covered: std::collections::BTreeSet<String> =
        indexed_languages(config).into_iter().collect();
    // Dart is indexed by its bespoke analyzer sidecar, which the SCIP-oriented
    // `indexed_languages` intentionally omits when the analyzer is on. Count it
    // as covered whenever Dart is the configured code language, so a Flutter
    // repo never gets a false "dart not indexed" warning.
    if config.code.language == "dart" {
        covered.insert("dart".to_string());
    }
    detected.difference(&covered).cloned().collect()
}

/// Number of files actually indexed for canonical `lang` this run, summed over
/// the dedicated adapter result and the unified tree-sitter pass (a language's
/// `language_id` in `treesitter` equals its canonical id). Used to gate SCIP
/// auto-invocation so a 0-file language never spawns an indexer subprocess.
fn language_file_count(result: &IndexResult, lang: &str) -> usize {
    let dedicated = match lang {
        "dart" => result.code.as_ref().map(|r| r.files),
        "rust" => result.rust.as_ref().map(|r| r.files),
        "go" => result.go.as_ref().map(|r| r.files),
        "python" => result.python.as_ref().map(|r| r.files),
        "typescript" => result.typescript.as_ref().map(|r| r.files),
        "swift" => result.swift.as_ref().map(|r| r.files),
        "java" => result.java.as_ref().map(|r| r.files),
        _ => None,
    }
    .unwrap_or(0);
    let from_treesitter: usize = result
        .treesitter
        .iter()
        .filter(|t| t.language == lang)
        .map(|t| t.files)
        .sum();
    dedicated + from_treesitter
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EngineConfig;

    #[test]
    fn config_command_is_dropped_unless_workspace_is_trusted() {
        // #187: a repo-provided command must NOT run by default.
        let (cmd, notice) =
            resolve_config_command(false, "swift.lsp_command", Some("/tmp/payload.sh"));
        assert_eq!(cmd, None, "untrusted repo command must be ignored");
        let notice = notice.expect("an ignored command must surface a visible notice");
        assert!(notice.contains("/tmp/payload.sh"), "{notice}");
        assert!(notice.contains(TRUST_CONFIG_COMMANDS_ENV), "{notice}");

        // Trusted workspace honours it, with no notice.
        let (cmd, notice) =
            resolve_config_command(true, "swift.lsp_command", Some("/tmp/payload.sh"));
        assert_eq!(cmd.as_deref(), Some("/tmp/payload.sh"));
        assert_eq!(notice, None);

        // Absent value is a no-op either way.
        assert_eq!(
            resolve_config_command(false, "swift.lsp_command", None),
            (None, None)
        );
        assert_eq!(
            resolve_config_command(true, "swift.lsp_command", None),
            (None, None)
        );
    }

    fn dart_config(analyzer: bool) -> EngineConfig {
        let mut cfg = EngineConfig::default();
        cfg.code.paths = vec!["lib".to_string()];
        cfg.enrichment.analyzer = analyzer;
        cfg
    }

    #[test]
    fn dart_scip_autoinvoke_skipped_when_analyzer_sidecar_active() {
        // The Dart analyzer sidecar is richer than generic SCIP (it emits
        // Riverpod / Hive / navigation / IAP domain edges SCIP has no concept
        // of) AND already supplies high-confidence Calls/References. Letting
        // `scip_dart` also run would make the overlay suppress the sidecar's
        // Calls/References in favour of scip's generic ones — a regression of
        // Dart's authoritative source. So when the sidecar is active we must
        // NOT auto-invoke `scip_dart`.
        let langs = indexed_languages(&dart_config(true));
        assert!(
            !langs.contains(&"dart".to_string()),
            "scip_dart must not auto-invoke while the analyzer sidecar is authoritative: {langs:?}"
        );
    }

    #[test]
    fn dart_scip_autoinvoke_enabled_when_analyzer_sidecar_disabled() {
        // With the sidecar off, Dart falls back to the `dart_lightweight`
        // heuristic; `scip_dart` then fills the precision gap (verified: it
        // upgrades the heuristic Calls/References to SCIP). So it SHOULD be
        // auto-invoked.
        let langs = indexed_languages(&dart_config(false));
        assert!(
            langs.contains(&"dart".to_string()),
            "scip_dart should fill the precision gap when the sidecar is disabled: {langs:?}"
        );
    }

    /// A repo `init`-ed when it was pure Python that later grew a Rust crate
    /// must surface `rust` as an unindexed blind spot — otherwise the graph
    /// silently omits an entire language and the agent never learns it exists.
    #[test]
    fn unindexed_languages_flags_present_but_unconfigured_language() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for i in 0..3 {
            std::fs::write(
                root.join("src").join(format!("m{i}.py")),
                "def f():\n    return 1\n",
            )
            .unwrap();
            std::fs::write(
                root.join("src").join(format!("r{i}.rs")),
                "fn f() -> i32 { 1 }\n",
            )
            .unwrap();
        }
        let mut cfg = EngineConfig::default();
        cfg.python.enabled = true;
        let drift = unindexed_languages_against_config(root, &cfg);
        assert!(
            drift.contains(&"rust".to_string()),
            "rust present but unconfigured → must be flagged: {drift:?}"
        );
        assert!(
            !drift.contains(&"python".to_string()),
            "python is configured → not flagged: {drift:?}"
        );
    }

    /// When the config already indexes every detected language there is no
    /// blind spot, so the warning must stay silent (no false nagging).
    #[test]
    fn unindexed_languages_empty_when_config_covers_everything() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for i in 0..3 {
            std::fs::write(
                root.join("src").join(format!("m{i}.py")),
                "def f():\n    return 1\n",
            )
            .unwrap();
        }
        let mut cfg = EngineConfig::default();
        cfg.python.enabled = true;
        let drift = unindexed_languages_against_config(root, &cfg);
        assert!(
            drift.is_empty(),
            "python-only repo fully covered → no drift: {drift:?}"
        );
    }

    /// SCIP auto-invoke is gated on real file counts: a language that indexed 0
    /// files this run must report 0 (so it is filtered out and never spawns an
    /// indexer subprocess), while one with files reports its count.
    #[test]
    fn language_file_count_gates_scip_on_real_files() {
        let mut result = IndexResult::default();
        result.treesitter.push(TreeSitterLangResult {
            language: "rust".to_string(),
            files: 171,
            ..Default::default()
        });
        result.treesitter.push(TreeSitterLangResult {
            language: "go".to_string(),
            files: 0,
            ..Default::default()
        });
        assert_eq!(
            language_file_count(&result, "rust"),
            171,
            "rust has files → SCIP runs"
        );
        assert_eq!(
            language_file_count(&result, "go"),
            0,
            "empty go → SCIP skipped"
        );
        assert_eq!(
            language_file_count(&result, "python"),
            0,
            "absent language → skipped"
        );
    }
}
