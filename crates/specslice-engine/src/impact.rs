//! PR Impact engine.
//!
//! MVP-4 (PRD §4 / implementation plan §MVP-4):
//! - Read `git diff --unified=0 base..head`.
//! - Resolve changed files to changed symbols via `symbol_ranges`.
//! - Walk manifest-declared relationships (direct + parent class).
//! - For changed doc sections, walk `documents` → Requirement → impl/tests.
//! - Report changed_symbols, affected_requirements, affected_docs, linked_tests
//!   and warnings.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{ArtifactId, EdgeKind, NodeKind};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::git_diff::{git_diff, parse_unified_diff, ChangedFile, Hunk};
use crate::index::{index_repository, IndexOptions};
use crate::slice::SliceItem;

#[derive(Debug, Clone)]
pub struct ImpactOptions {
    pub repo_root: PathBuf,
    pub base_ref: String,
    pub head_ref: String,
    /// If true, run a full re-index before computing impact (default true).
    pub reindex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactPolicy {
    pub propagate_to_parent_symbol: bool,
    pub include_doc_changes: bool,
    pub stale_doc_level: String,
    pub missing_test_change_level: String,
}

impl Default for ImpactPolicy {
    fn default() -> Self {
        Self {
            propagate_to_parent_symbol: true,
            include_doc_changes: true,
            stale_doc_level: "info".into(),
            missing_test_change_level: "warning".into(),
        }
    }
}

impl From<&crate::config::ImpactConfig> for ImpactPolicy {
    fn from(value: &crate::config::ImpactConfig) -> Self {
        Self {
            propagate_to_parent_symbol: value.propagate_to_parent_symbol,
            include_doc_changes: value.include_doc_changes,
            stale_doc_level: value.stale_doc_level.clone(),
            missing_test_change_level: value.missing_test_change_level.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImpactReport {
    pub changed_files: Vec<String>,
    pub changed_symbols: Vec<SliceItem>,
    pub changed_doc_sections: Vec<SliceItem>,
    pub affected_requirements: Vec<SliceItem>,
    pub affected_docs: Vec<SliceItem>,
    pub linked_tests: Vec<SliceItem>,
    /// Implementation symbols that declare any affected requirement.
    /// Populated regardless of whether the implementation was itself changed
    /// — this is what PRD §4.4 "Doc Impact" requires so the report stays
    /// actionable for doc-only changes.
    pub linked_implementations: Vec<SliceItem>,
    pub warnings: Vec<String>,
    pub info: Vec<String>,
}

pub fn run_impact(options: ImpactOptions) -> Result<ImpactReport> {
    let config = load_config(&options.repo_root)?;
    if options.reindex && config.impact.auto_reindex_changed_files {
        index_repository(IndexOptions::all(options.repo_root.clone()))
            .context("re-indexing repository before impact")?;
    }
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;

    let diff_text = git_diff(&options.repo_root, &options.base_ref, &options.head_ref)?;
    let changed = parse_unified_diff(&diff_text);
    compute_impact_with_policy(&store, &changed, ImpactPolicy::from(&config.impact))
}

/// Compute an impact report from an already-parsed diff. Useful in tests.
pub fn compute_impact(store: &Store, changed: &[ChangedFile]) -> Result<ImpactReport> {
    compute_impact_with_policy(store, changed, ImpactPolicy::default())
}

pub fn compute_impact_with_policy(
    store: &Store,
    changed: &[ChangedFile],
    policy: ImpactPolicy,
) -> Result<ImpactReport> {
    let mut report = ImpactReport::default();
    let mut affected_reqs: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut changed_symbol_ids: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut changed_doc_section_ids: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut any_test_changed = false;

    for file in changed {
        report.changed_files.push(file.path.clone());
        let is_dart = file.path.ends_with(".dart");
        let is_test_file = file.path.starts_with("test/") || file.path.contains("/test/");
        if is_dart && is_test_file {
            any_test_changed = true;
        }

        for hunk in &file.hunks {
            let symbols = find_changed_symbols(store, &file.path, *hunk)?;
            for symbol in symbols {
                changed_symbol_ids.insert(symbol.symbol_id.clone());
                report.changed_symbols.push(SliceItem {
                    id: symbol.symbol_id.to_string(),
                    kind: symbol.symbol_kind.as_str().to_string(),
                    path: Some(symbol.file_path.clone()),
                    name: Some(symbol.qualified_name.clone()),
                    line_range: Some((symbol.start_line, symbol.end_line)),
                });

                // Propagate from symbol → declared requirement. By default we
                // walk parent symbols; config can disable that for stricter
                // direct-only impact.
                let file_ranges = store.list_symbol_ranges_for_file(&symbol.file_path)?;
                let mut visited: BTreeSet<ArtifactId> = BTreeSet::new();
                let mut cursor: Option<ArtifactId> = Some(symbol.symbol_id.clone());
                while let Some(id) = cursor.clone() {
                    if !visited.insert(id.clone()) {
                        break;
                    }
                    let mut hit = false;
                    for edge in store.list_edges_from(&id)? {
                        if edge.kind == EdgeKind::DeclaresImplementation {
                            affected_reqs.insert(edge.to_id);
                            hit = true;
                        }
                    }
                    if hit {
                        break;
                    }
                    if !policy.propagate_to_parent_symbol {
                        break;
                    }
                    cursor = file_ranges
                        .iter()
                        .find(|r| r.symbol_id == id)
                        .and_then(|r| r.parent_symbol_id.clone());
                }
            }

            // Markdown change → affected doc sections + their REQ.
            if policy.include_doc_changes
                && (file.path.ends_with(".md") || file.path.ends_with(".mdx"))
            {
                let sections = find_doc_sections_for(store, &file.path, *hunk)?;
                for sec in sections {
                    if !changed_doc_section_ids.contains(&sec.id) {
                        changed_doc_section_ids.insert(sec.id.clone());
                        report.changed_doc_sections.push(SliceItem {
                            id: sec.id.to_string(),
                            kind: sec.kind.as_str().to_string(),
                            path: sec.path.clone(),
                            name: sec.name.clone(),
                            line_range: Some((
                                sec.start_line.unwrap_or(0),
                                sec.end_line.unwrap_or(0),
                            )),
                        });
                    }
                    for edge in store.list_edges_from(&sec.id)? {
                        if edge.kind == EdgeKind::Documents {
                            affected_reqs.insert(edge.to_id);
                        }
                    }
                }
            }
        }
    }

    // Resolve affected requirements → docs, tests, implementations.
    let mut docs_set: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut tests_set: BTreeSet<ArtifactId> = BTreeSet::new();
    let mut impl_set: BTreeSet<ArtifactId> = BTreeSet::new();
    for req_id in &affected_reqs {
        for edge in store.list_edges_to(req_id)? {
            match edge.kind {
                EdgeKind::Documents => {
                    docs_set.insert(edge.from_id);
                }
                EdgeKind::DeclaresVerification => {
                    tests_set.insert(edge.from_id);
                }
                EdgeKind::DeclaresImplementation => {
                    impl_set.insert(edge.from_id);
                }
                _ => {}
            }
        }
    }

    for req_id in &affected_reqs {
        if let Some(node) = store.find_node(req_id)? {
            report.affected_requirements.push(SliceItem {
                id: node.id.to_string(),
                kind: node.kind.as_str().to_string(),
                path: node.path,
                name: node.name,
                line_range: None,
            });
        }
    }
    for doc_id in &docs_set {
        if let Some(node) = store.find_node(doc_id)? {
            report.affected_docs.push(SliceItem {
                id: node.id.to_string(),
                kind: node.kind.as_str().to_string(),
                path: node.path,
                name: node.name,
                line_range: None,
            });
        }
    }
    for test_id in &tests_set {
        if let Some(node) = store.find_node(test_id)? {
            report.linked_tests.push(SliceItem {
                id: node.id.to_string(),
                kind: node.kind.as_str().to_string(),
                path: node.path,
                name: node.name,
                line_range: None,
            });
        }
    }
    for impl_id in &impl_set {
        if let Some(node) = store.find_node(impl_id)? {
            report.linked_implementations.push(SliceItem {
                id: node.id.to_string(),
                kind: node.kind.as_str().to_string(),
                path: node.path,
                name: node.name,
                line_range: match (node.start_line, node.end_line) {
                    (Some(s), Some(e)) => Some((s, e)),
                    _ => None,
                },
            });
        }
    }

    sort_items(&mut report.changed_symbols);
    sort_items(&mut report.changed_doc_sections);
    sort_items(&mut report.affected_requirements);
    sort_items(&mut report.affected_docs);
    sort_items(&mut report.linked_tests);
    sort_items(&mut report.linked_implementations);

    // Warnings & info.
    if !report.affected_requirements.is_empty()
        && !report.linked_tests.is_empty()
        && !any_test_changed
    {
        push_impact_message(
            &mut report,
            &policy.missing_test_change_level,
            "Affected requirement has linked tests, but no linked test changed in this PR."
                .to_string(),
        );
    }
    if !report.affected_requirements.is_empty() && report.changed_doc_sections.is_empty() {
        push_impact_message(
            &mut report,
            &policy.stale_doc_level,
            "Linked doc sections were not changed. Review whether docs are still accurate."
                .to_string(),
        );
    }

    Ok(report)
}

fn push_impact_message(report: &mut ImpactReport, level: &str, message: String) {
    match level.trim().to_ascii_lowercase().as_str() {
        "warning" | "warn" => report.warnings.push(message),
        "info" => report.info.push(message),
        "off" | "none" | "ignore" => {}
        _ => report.info.push(message),
    }
}

fn find_changed_symbols(
    store: &Store,
    path: &str,
    hunk: Hunk,
) -> Result<Vec<specslice_core::SymbolRange>> {
    let ranges = store.find_symbols_intersecting(path, hunk.new_start, hunk.new_end)?;
    Ok(filter_most_specific_symbols(ranges))
}

fn filter_most_specific_symbols(
    ranges: Vec<specslice_core::SymbolRange>,
) -> Vec<specslice_core::SymbolRange> {
    ranges
        .iter()
        .filter(|candidate| {
            !ranges.iter().any(|other| {
                other.symbol_id != candidate.symbol_id
                    && candidate.start_line <= other.start_line
                    && other.end_line <= candidate.end_line
                    && (other.end_line - other.start_line)
                        < (candidate.end_line - candidate.start_line)
            })
        })
        .cloned()
        .collect()
}

fn find_doc_sections_for(
    store: &Store,
    path: &str,
    hunk: Hunk,
) -> Result<Vec<specslice_core::Node>> {
    // Doc sections are stored as nodes with start_line/end_line; we iterate
    // by kind here. The fixture has few enough sections that a linear scan
    // is fine.
    let mut hits = Vec::new();
    for node in store.list_nodes_by_kind(NodeKind::DocSection)? {
        if node.path.as_deref() != Some(path) {
            continue;
        }
        let start = node.start_line.unwrap_or(0);
        let end = node.end_line.unwrap_or(u32::MAX);
        if hunk.new_start <= end && start <= hunk.new_end {
            hits.push(node);
        }
    }
    Ok(hits)
}

fn sort_items(items: &mut [SliceItem]) {
    items.sort_by(|a, b| a.id.cmp(&b.id));
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
