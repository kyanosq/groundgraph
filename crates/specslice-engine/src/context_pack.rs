//! Agent Context Pack.
//!
//! MVP-5 (PRD §7 / implementation plan §MVP-5): produce a JSON bundle for a
//! requirement that an LLM/agent can ingest without browsing the whole repo.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::EdgeKind;
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::slice::{slice_from_store, FeatureSlice, SliceItem};

#[derive(Debug, Clone)]
pub struct ContextOptions {
    pub repo_root: PathBuf,
    pub requirement: String,
    pub include_snippets: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPack {
    pub requirement_id: String,
    pub title: Option<String>,
    pub slice: FeatureSlice,
    pub docs_snippets: Vec<DocSnippet>,
    pub impl_snippets: Vec<CodeSnippet>,
    pub test_snippets: Vec<CodeSnippet>,
    pub edges: Vec<EdgeSummary>,
    /// PRD §7: flat, deduplicated list of file paths an Agent must read to
    /// understand the requirement. Order is `docs → implementation → tests`.
    pub files_to_read: Vec<String>,
    /// PRD §7: test files (not individual test cases) the Agent should run
    /// after touching this requirement.
    pub tests_to_run: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSnippet {
    pub item: SliceItem,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeSnippet {
    pub item: SliceItem,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeSummary {
    pub kind: String,
    pub from: String,
    pub to: String,
}

pub fn build_context(options: ContextOptions) -> Result<ContextPack> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    let slice = slice_from_store(&store, &options.requirement)?;

    let mut docs_snippets = Vec::new();
    let mut impl_snippets = Vec::new();
    let mut test_snippets = Vec::new();

    if options.include_snippets {
        for item in &slice.docs {
            if let Some(text) = read_snippet(&options.repo_root, item)? {
                docs_snippets.push(DocSnippet {
                    item: item.clone(),
                    text,
                });
            }
        }
        for item in &slice.implementation {
            if let Some(text) = read_snippet(&options.repo_root, item)? {
                impl_snippets.push(CodeSnippet {
                    item: item.clone(),
                    text,
                });
            }
        }
        for item in &slice.linked_tests {
            if let Some(text) = read_snippet(&options.repo_root, item)? {
                test_snippets.push(CodeSnippet {
                    item: item.clone(),
                    text,
                });
            }
        }
    }

    let mut edges_summary = Vec::new();
    let req_id = specslice_core::artifact_id::requirement_id(&options.requirement);
    for edge in store.list_edges_to(&req_id)? {
        if matches!(
            edge.kind,
            EdgeKind::Documents | EdgeKind::DeclaresImplementation | EdgeKind::DeclaresVerification
        ) {
            edges_summary.push(EdgeSummary {
                kind: edge.kind.as_str().to_string(),
                from: edge.from_id.to_string(),
                to: edge.to_id.to_string(),
            });
        }
    }

    let files_to_read = collect_unique_paths(&[
        slice.docs.as_slice(),
        slice.implementation.as_slice(),
        slice.linked_tests.as_slice(),
    ]);
    let tests_to_run = collect_unique_paths(&[slice.linked_tests.as_slice()]);

    Ok(ContextPack {
        requirement_id: options.requirement.clone(),
        title: slice.title.clone(),
        slice,
        docs_snippets,
        impl_snippets,
        test_snippets,
        edges: edges_summary,
        files_to_read,
        tests_to_run,
    })
}

fn collect_unique_paths(groups: &[&[SliceItem]]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut ordered = Vec::new();
    for group in groups {
        for item in *group {
            if let Some(p) = &item.path {
                if seen.insert(p.clone()) {
                    ordered.push(p.clone());
                }
            }
        }
    }
    ordered
}

fn read_snippet(repo_root: &Path, item: &SliceItem) -> Result<Option<String>> {
    let Some(path) = &item.path else {
        return Ok(None);
    };
    let abs = repo_root.join(path);
    if !abs.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&abs).with_context(|| format!("reading {}", abs.display()))?;
    let Some((start, end)) = item.line_range else {
        return Ok(Some(text));
    };
    let lines: Vec<&str> = text.lines().collect();
    let start_idx = (start.saturating_sub(1)) as usize;
    let end_idx = (end.min(lines.len() as u32)) as usize;
    if start_idx >= lines.len() {
        return Ok(Some(String::new()));
    }
    Ok(Some(lines[start_idx..end_idx].join("\n")))
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

/// Helper: collect per-node-kind counts in the store. Useful for status-line
/// summaries — not currently used by the CLI but kept for completeness.
pub fn node_kind_counts(store: &Store) -> Result<BTreeMap<String, usize>> {
    let mut map = BTreeMap::new();
    for node in store.list_all_nodes()? {
        *map.entry(node.kind.as_str().to_string()).or_insert(0) += 1;
    }
    Ok(map)
}
