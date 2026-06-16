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

/// Schema version of the `.specslice.yaml` contract this build understands
/// (#72). Bump on any breaking change to a config key's name or semantics, and
/// add the matching migration / warning. Every other external SpecSlice
/// contract is versioned (evidence, candidates, questions, the DB `schema_version`
/// table); the config file is the last one that was not.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EngineConfig {
    /// Version of the config schema this file was written against (#72).
    /// `None` for legacy files written before the field existed (treated as
    /// "compatible"); `init` stamps freshly-written configs with
    /// [`CONFIG_SCHEMA_VERSION`]. A value greater than this build supports
    /// triggers a forward-compat warning on load (see
    /// [`EngineConfig::schema_version_notice`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
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
    /// P11 — opt-in Go language adapter. The LSP tier (`gopls`) was
    /// **retired** (ADR-0001 §8.8): structure + heuristic Calls/References
    /// come from the in-process tree-sitter driver, and precision comes
    /// from the offline SCIP overlay (`scip-go`, auto-invoked by `index`).
    /// The shared `lsp_command` field is ignored.
    #[serde(default)]
    pub go: LanguageAdapterConfig,
    /// P16 — opt-in Python language adapter. The LSP tier (pyright /
    /// pylsp) was **retired** (ADR-0001 §8.8): the tree-sitter driver owns
    /// structure + imports + pytest cases + heuristic Calls/References;
    /// precision comes from the SCIP overlay (`scip-python`, auto-invoked).
    /// The shared `lsp_command` field is ignored. NOTE: `scip-python` is
    /// currently broken upstream (empty index), so Python relies on the
    /// heuristic baseline until that is fixed.
    #[serde(default)]
    pub python: LanguageAdapterConfig,
    /// P20 — opt-in TypeScript adapter. The LSP tier
    /// (`typescript-language-server`) was **retired** (ADR-0001 §8.8): the
    /// tree-sitter driver owns structure + imports + jest/vitest cases +
    /// heuristic Calls/References across `.ts`/`.tsx`/`.js`/`.vue`;
    /// precision comes from the SCIP overlay (`scip-typescript`,
    /// auto-invoked). The shared `lsp_command` field is ignored.
    #[serde(default)]
    pub typescript: LanguageAdapterConfig,
    /// P20 — opt-in Java adapter. The LSP tier (`jdtls`) was **retired**
    /// (ADR-0001 §8.8): the tree-sitter driver owns structure + package
    /// declarations + JUnit cases + heuristic Calls/References; precision
    /// comes from the SCIP overlay (`scip-java`). The shared `lsp_command`
    /// field is ignored.
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
    /// `languages` is set. `lsp` (default `true`) now affects **Swift
    /// only** — the sole language retaining an LSP overlay (sourcekit-lsp);
    /// go/python/ts/java retired their LSP (ADR-0001 §8.8) and take
    /// precision from the SCIP overlay instead, so for them `lsp` toggles
    /// nothing beyond routing through the (structure-identical) dedicated
    /// adapter vs the generic driver. `scip` (default `true`) controls the
    /// offline SCIP overlay (the primary precision source for all
    /// non-Swift languages). `analyzer` (default `true`) controls the Dart
    /// analyzer overlay.
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
    /// Ingest offline SCIP indexes (`.specslice/scip/*.scip`) as a
    /// high-confidence `Calls`/`References` overlay (ADR-0001 R1). A no-op
    /// when no `.scip` is present, so it is safe to leave on; the file is
    /// produced out-of-band by a language's SCIP indexer (`rust-analyzer
    /// scip`, `scip-go`, `scip-typescript`, …).
    #[serde(default = "default_true")]
    pub scip: bool,
}

impl Default for EnrichmentConfig {
    fn default() -> Self {
        Self {
            lsp: true,
            analyzer: true,
            scip: true,
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
                // TypeScript is special: only its adapter runs *both* dialects
                // (`.ts`/`.mts`/`.cts` + `.tsx`/`.js`/`.jsx`/`.vue`). The generic
                // single-spec driver would silently miss the entire JSX/JS/Vue
                // dialect (a JS/Vue repo would index zero files), so route through
                // the adapter regardless of `lsp`. The adapter is structure +
                // heuristic only (precision comes from the SCIP overlay).
                "typescript" => self.typescript = adapter_from(sel),
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

    /// Forward-compat guard for `.specslice.yaml` (#72): a warning string when
    /// this file declares a schema version newer than the build supports, else
    /// `None`. Thin wrapper over [`config_schema_notice`] bound to
    /// [`CONFIG_SCHEMA_VERSION`].
    pub fn schema_version_notice(&self) -> Option<String> {
        config_schema_notice(self.schema_version, CONFIG_SCHEMA_VERSION)
    }
}

/// Forward-compat guard for the config schema (#72). Returns a warning when a
/// loaded config declares a `schema_version` newer than `supported` — its keys
/// may carry meanings this build will misinterpret, mirroring the DB's
/// `SchemaTooNew` guard (#153). An unversioned (`None`, legacy) file and any
/// `declared <= supported` pass silently. Pure, so the policy is unit-testable
/// without touching the filesystem.
pub fn config_schema_notice(declared: Option<u32>, supported: u32) -> Option<String> {
    match declared {
        Some(v) if v > supported => Some(format!(
            ".specslice.yaml declares schema_version {v}, but this specslice supports up to \
             {supported}; upgrade specslice or some config keys may be misinterpreted"
        )),
        _ => None,
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
        "csharp" | "c#" | "cs" => Some("csharp"),
        "ruby" | "rb" => Some("ruby"),
        "php" => Some("php"),
        "kotlin" | "kt" => Some("kotlin"),
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
    // Both plural and singular dirs: `docs/` dominates, but `doc/` is the
    // convention of a large slice of classic projects (leveldb, git, vim,
    // postgres). Nonexistent roots are skipped, so listing both is free.
    vec!["docs".into(), "doc".into(), "specs".into(), "adr".into()]
}

fn default_docs_include() -> Vec<String> {
    // rst: the Python ecosystem's documentation format (flask, django,
    // numpy); adoc: the JVM ecosystem's (spring, hibernate, quarkus). The
    // docs indexer parses both natively.
    vec![
        "**/*.md".into(),
        "**/*.mdx".into(),
        "**/*.rst".into(),
        "**/*.adoc".into(),
    ]
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
    /// `doc_stale_code_ref` — doc body references a missing path/symbol.
    #[serde(default = "default_stale_doc_ref_level")]
    pub stale_doc_ref_level: String,
    /// `requirement_implementation_hint` — orphan requirements get graph
    /// suggestions (or a likely-gap callout).
    #[serde(default = "default_requirement_hint_level")]
    pub requirement_hint_level: String,
    /// Glob patterns suppressing `doc_stale_code_ref`. A pattern is matched
    /// against the *referenced* path and against the *document's own* path,
    /// so `legacy/**` mutes refs into legacy code and `docs/external/**`
    /// mutes whole documents that narrate another repository.
    #[serde(default)]
    pub doc_drift_ignore: Vec<String>,
}

impl Default for ChecksConfig {
    fn default() -> Self {
        Self {
            broken_link_level: default_broken_link_level(),
            missing_linked_test_level: default_missing_linked_test_level(),
            orphan_requirement_level: default_orphan_requirement_level(),
            stale_doc_ref_level: default_stale_doc_ref_level(),
            requirement_hint_level: default_requirement_hint_level(),
            doc_drift_ignore: Vec::new(),
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
fn default_stale_doc_ref_level() -> String {
    "warning".into()
}
fn default_requirement_hint_level() -> String {
    "info".into()
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
