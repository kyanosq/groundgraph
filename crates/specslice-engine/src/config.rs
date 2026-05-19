//! Workspace configuration stored in `.specslice.yaml`.
//!
//! MVP-0 only persisted `repo + storage`. MVP-7 collapsing the PRD §8 schema
//! requires the full set of sections (docs / code / trace / slice / impact /
//! checks). To keep backward compatibility we:
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
    pub trace: TraceConfig,
    #[serde(default)]
    pub slice: SliceConfig,
    #[serde(default)]
    pub impact: ImpactConfig,
    #[serde(default)]
    pub checks: ChecksConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceConfig {
    #[serde(default = "default_trace_implements")]
    #[serde(rename = "explicit_tags_implements", alias = "implements_tag")]
    pub implements_tag: String,
    #[serde(default = "default_trace_verifies")]
    #[serde(rename = "explicit_tags_verifies", alias = "verifies_tag")]
    pub verifies_tag: String,
    #[serde(default = "default_trace_related")]
    #[serde(rename = "explicit_tags_related", alias = "related_tag")]
    pub related_tag: String,
    /// Alternative nested form: `trace.explicit_tags.{implements,verifies,related}`.
    #[serde(default)]
    pub explicit_tags: Option<ExplicitTags>,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            implements_tag: default_trace_implements(),
            verifies_tag: default_trace_verifies(),
            related_tag: default_trace_related(),
            explicit_tags: None,
        }
    }
}

fn default_trace_implements() -> String {
    "@implements".into()
}

fn default_trace_verifies() -> String {
    "@verifies".into()
}

fn default_trace_related() -> String {
    "@related".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExplicitTags {
    #[serde(default)]
    pub implements: Option<String>,
    #[serde(default)]
    pub verifies: Option<String>,
    #[serde(default)]
    pub related: Option<String>,
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
    #[serde(default = "default_broken_trace_level")]
    pub broken_trace_level: String,
    #[serde(default = "default_missing_linked_test_level")]
    pub missing_linked_test_level: String,
    #[serde(default = "default_orphan_requirement_level")]
    pub orphan_requirement_level: String,
}

impl Default for ChecksConfig {
    fn default() -> Self {
        Self {
            broken_trace_level: default_broken_trace_level(),
            missing_linked_test_level: default_missing_linked_test_level(),
            orphan_requirement_level: default_orphan_requirement_level(),
        }
    }
}

fn default_broken_trace_level() -> String {
    "error".into()
}
fn default_missing_linked_test_level() -> String {
    "warning".into()
}
fn default_orphan_requirement_level() -> String {
    "warning".into()
}
