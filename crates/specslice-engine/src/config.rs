//! Workspace configuration stored in `.specslice.yaml`.
//!
//! MVP-0 only persisted `repo + storage`. The non-invasive MVP schema keeps
//! all SpecSlice-owned metadata under `.specslice/`; business docs and code
//! are scanned as facts only.
//!
//! 1. Make every section optional via `#[serde(default)]`.
//! 2. Drop `#[serde(deny_unknown_fields)]` so future keys are tolerated.
//! 3. Provide sensible defaults that match the original MVP-0 behaviour
//!    (docs roots `docs/specs/adr`, code roots `lib/test`).

use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_FILE_NAME: &str = ".specslice.yaml";
pub const DEFAULT_STORAGE_DIR: &str = ".specslice";
pub const DEFAULT_DB_FILENAME: &str = "graph.db";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EngineConfig {
    #[serde(default)]
    pub repo: RepoConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub docs: DocsConfig,
    #[serde(default)]
    pub code: CodeConfig,
    #[serde(default)]
    pub links: LinksConfig,
    #[serde(default)]
    pub slice: SliceConfig,
    #[serde(default)]
    pub impact: ImpactConfig,
    #[serde(default)]
    pub checks: ChecksConfig,
    #[serde(default)]
    pub dead_code: DeadCodeConfig,
    /// P11 — opt-in Swift language adapter driven by `sourcekit-lsp`.
    /// Disabled by default so existing Dart-only workspaces are not
    /// affected. Operators flip `enabled: true` and (optionally) point
    /// `lsp_command` at a specific binary.
    #[serde(default)]
    pub swift: LanguageAdapterConfig,
    /// P11 — opt-in Go language adapter driven by `gopls`. Same
    /// semantics as `swift`.
    #[serde(default)]
    pub go: LanguageAdapterConfig,
    /// P16 — opt-in Python language adapter. Drives `pyright-langserver`
    /// / `basedpyright-langserver` / `pylsp` (with venv auto-discovery)
    /// for structural symbols + Calls/References, and unconditionally
    /// runs the AST scanner for imports + pytest cases. When LSP is
    /// unavailable the AST scanner takes over the structural pass.
    #[serde(default)]
    pub python: LanguageAdapterConfig,
    /// P20 — opt-in TypeScript adapter driven by
    /// `typescript-language-server --stdio`. AST 补强 always runs for
    /// import edges + jest/vitest test cases regardless of LSP state.
    #[serde(default)]
    pub typescript: LanguageAdapterConfig,
    /// P20 — opt-in Java adapter driven by `jdtls`. AST 补强 always
    /// runs for package declarations + JUnit test methods regardless
    /// of LSP state.
    #[serde(default)]
    pub java: LanguageAdapterConfig,
    /// P21 — Rust adapter, the first **tree-sitter breadth backend**.
    /// Has no LSP tier: the grammar is linked into the engine so the
    /// adapter is always available, deterministic, and fast. The shared
    /// `lsp_command` field is ignored. Enabled by default for `.rs`
    /// workspaces is *not* assumed; operators flip `rust.enabled: true`.
    #[serde(default)]
    pub rust: LanguageAdapterConfig,
    /// P22 — the unified **tree-sitter breadth backend**. One switch that
    /// runs the in-process generic driver for any supported language
    /// (`rust`, `typescript`, `python`, `go`, `c`, `cpp`). Deliberately
    /// separate from the per-language LSP sections (`go` / `typescript` /
    /// `python`): those drive the Tier 3 precision adapters, while this is
    /// the always-available, zero-config Tier 2 structural layer. Rust is
    /// also reachable via the legacy `rust:` switch; when both name Rust
    /// the legacy one wins so we never double-index.
    #[serde(default)]
    pub treesitter: TreeSitterConfig,
    /// P23.7 — **unified language selection** (canonical). Lists the
    /// languages to index, each with its own `paths` / `exclude` /
    /// `lsp_command`. This replaces the per-language switches (`code` for
    /// Dart, `swift`/`go`/`python`/`typescript`/`java`/`rust`, and
    /// `treesitter.languages`), which remain supported as **deprecated
    /// aliases** and are used only when `languages` is empty. See
    /// [`EngineConfig::normalized`].
    #[serde(default)]
    pub languages: Vec<LanguageSelection>,
    /// P23.7 — how the structural graph is *enriched*. Applies only when
    /// `languages` is set. `lsp` (default `true`) routes LSP-capable
    /// languages through their Tier-3 adapter (tree-sitter structure +
    /// optional `Calls`/`References` overlay); when `false` they index via
    /// the structure-only generic driver. `analyzer` (default `true`)
    /// controls the Dart analyzer overlay.
    #[serde(default)]
    pub enrichment: EnrichmentConfig,
}

/// One entry in the unified [`EngineConfig::languages`] list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LanguageSelection {
    /// Canonical language id (`dart`, `rust`, `typescript`, `python`,
    /// `go`, `java`, `swift`, `c`, `cpp`); common aliases (`ts`, `py`,
    /// `rs`, `c++`) are accepted.
    pub id: String,
    /// Roots under `repo.root` to scan for this language. Empty falls back
    /// to the language's conventional default.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Glob excludes for this language.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Optional LSP binary override (LSP-capable languages only).
    #[serde(default)]
    pub lsp_command: Option<String>,
}

/// See [`EngineConfig::enrichment`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnrichmentConfig {
    #[serde(default = "default_true")]
    pub lsp: bool,
    #[serde(default = "default_true")]
    pub analyzer: bool,
}

impl Default for EnrichmentConfig {
    fn default() -> Self {
        Self {
            lsp: true,
            analyzer: true,
        }
    }
}

impl EngineConfig {
    /// Fold the canonical [`EngineConfig::languages`] list down onto the
    /// legacy per-language switches so the single index pass has exactly one
    /// authoritative source. A no-op when `languages` is empty (pure legacy
    /// config). Idempotent.
    ///
    /// Routing per entry:
    /// - `dart` → `code` (Dart is always the analyzer/tree-sitter path).
    /// - `rust` → `rust` (no LSP tier; always tree-sitter).
    /// - `swift`/`go`/`python`/`typescript`/`java` → their Tier-3 adapter
    ///   when `enrichment.lsp`, else the structure-only generic driver.
    /// - `c`/`cpp` (and any lsp-disabled language) → the generic
    ///   `treesitter` driver.
    pub fn normalized(mut self) -> Self {
        if self.languages.is_empty() {
            return self;
        }
        let lsp = self.enrichment.lsp;
        let selections = std::mem::take(&mut self.languages);

        // `languages` is *authoritative*: clear every structural switch first
        // (legacy per-language keys are deprecated aliases honoured only when
        // `languages` is absent), then re-enable exactly what was selected so
        // an unlisted language — Dart included — is excluded from the run.
        let dart_default_paths = if self.code.paths.is_empty() {
            vec!["lib".to_string(), "test".to_string()]
        } else {
            self.code.paths.clone()
        };
        self.rust.enabled = false;
        self.swift.enabled = false;
        self.go.enabled = false;
        self.python.enabled = false;
        self.typescript.enabled = false;
        self.java.enabled = false;
        self.treesitter.enabled = false;
        self.treesitter.languages.clear();

        let mut dart_selected = false;
        let mut ts_langs: Vec<String> = Vec::new();
        let mut ts_paths: Vec<String> = Vec::new();
        let mut ts_exclude: Vec<String> = Vec::new();

        let mut into_generic = |sel: &LanguageSelection, canon: &str| {
            if !ts_langs.iter().any(|l| l == canon) {
                ts_langs.push(canon.to_string());
            }
            for p in &sel.paths {
                if !ts_paths.contains(p) {
                    ts_paths.push(p.clone());
                }
            }
            for e in &sel.exclude {
                if !ts_exclude.contains(e) {
                    ts_exclude.push(e.clone());
                }
            }
        };

        for sel in &selections {
            let Some(canon) = canonical_language_id(&sel.id) else {
                continue; // unknown id: skip, never abort.
            };
            match canon {
                "dart" => {
                    dart_selected = true;
                    self.code.paths = if sel.paths.is_empty() {
                        dart_default_paths.clone()
                    } else {
                        sel.paths.clone()
                    };
                    if !sel.exclude.is_empty() {
                        self.code.exclude = sel.exclude.clone();
                    }
                    self.code.language = "dart".into();
                }
                "rust" => self.rust = adapter_from(sel),
                "swift" if lsp => self.swift = adapter_from(sel),
                "go" if lsp => self.go = adapter_from(sel),
                "python" if lsp => self.python = adapter_from(sel),
                "typescript" if lsp => self.typescript = adapter_from(sel),
                "java" if lsp => self.java = adapter_from(sel),
                // c / cpp always, and any lsp-capable language when
                // `enrichment.lsp == false`: structure-only generic driver.
                other => into_generic(sel, other),
            }
        }

        if !dart_selected {
            // Dart was not listed: an empty code root scans nothing.
            self.code.paths = Vec::new();
        }
        if !ts_langs.is_empty() {
            self.treesitter.enabled = true;
            self.treesitter.languages = ts_langs;
            if !ts_paths.is_empty() {
                self.treesitter.paths = ts_paths;
            }
            self.treesitter.exclude = ts_exclude;
        }
        self
    }
}

/// Build a [`LanguageAdapterConfig`] (enabled) from a unified selection.
fn adapter_from(sel: &LanguageSelection) -> LanguageAdapterConfig {
    LanguageAdapterConfig {
        enabled: true,
        paths: sel.paths.clone(),
        exclude: sel.exclude.clone(),
        lsp_command: sel.lsp_command.clone(),
    }
}

/// Resolve a (possibly aliased) language id to its canonical spec id, or
/// `None` if unrecognised. Single source of truth for the accepted aliases,
/// shared by the unified config and the tree-sitter registry.
pub fn canonical_language_id(id: &str) -> Option<&'static str> {
    match id.trim().to_ascii_lowercase().as_str() {
        "dart" => Some("dart"),
        "rust" | "rs" => Some("rust"),
        // JavaScript is parsed by the JSX-aware TypeScript grammar, so it
        // routes through the same `typescript` adapter (which indexes
        // `.js` / `.jsx` / `.mjs` / `.cjs` alongside `.ts` / `.tsx`).
        "typescript" | "ts" | "javascript" | "js" => Some("typescript"),
        "python" | "py" => Some("python"),
        "go" | "golang" => Some("go"),
        "java" => Some("java"),
        "swift" => Some("swift"),
        "c" => Some("c"),
        "cpp" | "c++" | "cxx" => Some("cpp"),
        _ => None,
    }
}

/// Config for the unified tree-sitter breadth backend. See
/// [`EngineConfig::treesitter`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TreeSitterConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Languages to index. Recognised ids: `rust`, `typescript`,
    /// `python`, `go`, `c`, `cpp` (a few aliases like `ts`/`py`/`c++`
    /// are accepted). Unknown entries are skipped.
    #[serde(default)]
    pub languages: Vec<String>,
    /// Roots under `repo.root` to scan, shared across the listed
    /// languages. Empty means the repo root.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Glob excludes applied to every language in this section.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl TreeSitterConfig {
    /// Scan roots, falling back to the repo root when none are set.
    pub fn roots(&self) -> Vec<String> {
        if self.paths.is_empty() {
            vec![".".to_string()]
        } else {
            self.paths.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinksConfig {
    #[serde(default = "default_links_path")]
    pub path: String,
}

impl Default for LinksConfig {
    fn default() -> Self {
        Self {
            path: default_links_path(),
        }
    }
}

fn default_links_path() -> String {
    ".specslice/links.yaml".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoConfig {
    pub root: String,
    pub default_branch: String,
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            root: ".".into(),
            default_branch: "main".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageConfig {
    pub path: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            path: format!("{DEFAULT_STORAGE_DIR}/{DEFAULT_DB_FILENAME}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocsConfig {
    #[serde(default = "default_docs_paths")]
    pub paths: Vec<String>,
    #[serde(default = "default_docs_include")]
    pub include: Vec<String>,
    #[serde(default)]
    pub requirement_patterns: Vec<String>,
    #[serde(default)]
    pub adr_patterns: Vec<String>,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            paths: default_docs_paths(),
            include: default_docs_include(),
            requirement_patterns: Vec::new(),
            adr_patterns: Vec::new(),
        }
    }
}

fn default_docs_paths() -> Vec<String> {
    vec!["docs".into(), "specs".into(), "adr".into()]
}

fn default_docs_include() -> Vec<String> {
    vec!["**/*.md".into(), "**/*.mdx".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeConfig {
    #[serde(default = "default_code_language")]
    pub language: String,
    #[serde(default = "default_code_paths")]
    pub paths: Vec<String>,
    #[serde(default)]
    pub adapter: AdapterConfig,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for CodeConfig {
    fn default() -> Self {
        Self {
            language: default_code_language(),
            paths: default_code_paths(),
            adapter: AdapterConfig::default(),
            exclude: Vec::new(),
        }
    }
}

fn default_code_language() -> String {
    "dart".into()
}

fn default_code_paths() -> Vec<String> {
    vec!["lib".into(), "test".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterConfig {
    #[serde(default = "default_adapter_backend")]
    pub backend: String,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            backend: default_adapter_backend(),
        }
    }
}

fn default_adapter_backend() -> String {
    "lightweight".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SliceConfig {
    #[serde(default = "default_slice_max_depth")]
    pub max_depth: u32,
    #[serde(default = "default_slice_max_nodes")]
    pub max_nodes: u32,
    #[serde(default = "default_slice_min_score")]
    pub min_score: f64,
    #[serde(default)]
    pub include_imports: bool,
    #[serde(default)]
    pub include_candidates: bool,
}

impl Default for SliceConfig {
    fn default() -> Self {
        Self {
            max_depth: default_slice_max_depth(),
            max_nodes: default_slice_max_nodes(),
            min_score: default_slice_min_score(),
            include_imports: false,
            include_candidates: false,
        }
    }
}

fn default_slice_max_depth() -> u32 {
    3
}

fn default_slice_max_nodes() -> u32 {
    120
}

fn default_slice_min_score() -> f64 {
    0.35
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImpactConfig {
    #[serde(default = "default_true")]
    pub auto_reindex_changed_files: bool,
    #[serde(default = "default_true")]
    pub propagate_to_parent_symbol: bool,
    #[serde(default = "default_true")]
    pub include_doc_changes: bool,
    #[serde(default = "default_stale_doc_level")]
    pub stale_doc_level: String,
    #[serde(default = "default_missing_test_change_level")]
    pub missing_test_change_level: String,
}

impl Default for ImpactConfig {
    fn default() -> Self {
        Self {
            auto_reindex_changed_files: true,
            propagate_to_parent_symbol: true,
            include_doc_changes: true,
            stale_doc_level: default_stale_doc_level(),
            missing_test_change_level: default_missing_test_change_level(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_stale_doc_level() -> String {
    "info".into()
}
fn default_missing_test_change_level() -> String {
    "warning".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChecksConfig {
    #[serde(default = "default_broken_link_level")]
    pub broken_link_level: String,
    #[serde(default = "default_missing_linked_test_level")]
    pub missing_linked_test_level: String,
    #[serde(default = "default_orphan_requirement_level")]
    pub orphan_requirement_level: String,
}

impl Default for ChecksConfig {
    fn default() -> Self {
        Self {
            broken_link_level: default_broken_link_level(),
            missing_linked_test_level: default_missing_linked_test_level(),
            orphan_requirement_level: default_orphan_requirement_level(),
        }
    }
}

fn default_broken_link_level() -> String {
    "error".into()
}
fn default_missing_linked_test_level() -> String {
    "warning".into()
}
fn default_orphan_requirement_level() -> String {
    "warning".into()
}

/// Configuration consumed by `specslice dead-code` (P7).
///
/// All sections are optional. Defaults give a useful zero-config
/// experience on a Flutter app: `lib/main.dart` is the entry, common
/// codegen suffixes are ignored, and no path is treated as "public
/// API" unless the operator explicitly enumerates one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeadCodeConfig {
    /// File paths whose top-level `main()` (and any other exported
    /// symbol) is considered an entry point. Relative to the repo
    /// root.
    #[serde(default = "default_dead_code_entrypoints")]
    pub entrypoints: Vec<String>,
    /// Glob patterns whose matching files are excluded from the
    /// dead-code report (codegen, generated, vendored). Patterns are
    /// matched against the *relative* repo-root path.
    #[serde(default = "default_dead_code_ignore")]
    pub ignore: Vec<String>,
    /// Glob patterns marking a "public API surface". Symbols under
    /// these paths can never be flagged as high-confidence dead, even
    /// when no caller appears in the graph (consumers may live
    /// outside the repo).
    #[serde(default)]
    pub public_api_roots: Vec<String>,
}

impl Default for DeadCodeConfig {
    fn default() -> Self {
        Self {
            entrypoints: default_dead_code_entrypoints(),
            ignore: default_dead_code_ignore(),
            public_api_roots: Vec::new(),
        }
    }
}

fn default_dead_code_entrypoints() -> Vec<String> {
    vec!["lib/main.dart".into()]
}

fn default_dead_code_ignore() -> Vec<String> {
    vec![
        "**/*.g.dart".into(),
        "**/*.freezed.dart".into(),
        "**/*.gr.dart".into(),
        "**/generated/**".into(),
        "**/l10n/app_localizations*.dart".into(),
        "**/.dart_tool/**".into(),
    ]
}

/// Shared shape for the per-language LSP adapter sections (Swift, Go,
/// and future Python / TypeScript / Java entries). `enabled` opts the
/// language in; the engine silently does nothing when it is `false`.
/// `paths` mirrors `code.paths` but is per-language so a single repo
/// can index multiple languages with non-overlapping roots.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LanguageAdapterConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Directories under `repo.root` that hold this language's
    /// sources. Empty defaults to the language-specific suggestion
    /// (Swift: `Sources test Tests`; Go: `.` and `cmd`). The engine
    /// resolves the actual fallback at run-time so the YAML keeps a
    /// minimal opt-in shape.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Glob patterns to skip even when they live under `paths`. Same
    /// matcher as `code.exclude`.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Optional LSP binary override. Defaults to `sourcekit-lsp` for
    /// Swift / `gopls` for Go, looked up on `PATH`. The corresponding
    /// `SPECSLICE_SWIFT_LSP_BIN` / `SPECSLICE_GO_LSP_BIN` env vars take
    /// precedence at runtime.
    #[serde(default)]
    pub lsp_command: Option<String>,
}

impl LanguageAdapterConfig {
    pub fn paths_or(&self, fallback: &[&str]) -> Vec<String> {
        if self.paths.is_empty() {
            fallback.iter().map(|s| (*s).to_string()).collect()
        } else {
            self.paths.clone()
        }
    }
}
