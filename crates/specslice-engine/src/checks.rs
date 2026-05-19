//! Basic SpecSlice checks.
//!
//! MVP-5 scope (PRD §6 / implementation plan §MVP-5):
//! - `broken_trace`: a declared trace (`@implements` / `@verifies`) points to
//!   a requirement that does not exist (or the symbol does not exist).
//!   Severity: error.
//! - `missing_linked_test`: a requirement has at least one declared
//!   implementation but no declared verification. Severity: warning.
//! - `orphan_requirement`: a requirement has no declared implementation and
//!   no declared verification. Severity: warning.
//! - `impact_review`: synthesised from an [`ImpactReport`]; warns when no
//!   linked tests changed alongside a requirement.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::artifact_id::{dart_class_id, dart_function_id, dart_test_id, file_id};
use specslice_core::{ArtifactId, EdgeKind, NodeKind};
use specslice_store::Store;

use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use crate::impact::ImpactReport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSeverity {
    Error,
    Warning,
    Info,
}

impl CheckSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckSeverity::Error => "error",
            CheckSeverity::Warning => "warning",
            CheckSeverity::Info => "info",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckFinding {
    pub code: String,
    pub severity: CheckSeverity,
    pub message: String,
    pub artifact_id: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CheckReport {
    pub findings: Vec<CheckFinding>,
}

impl CheckReport {
    pub fn errors(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == CheckSeverity::Error)
            .count()
    }
    pub fn warnings(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == CheckSeverity::Warning)
            .count()
    }
    pub fn has_errors(&self) -> bool {
        self.errors() > 0
    }
}

#[derive(Debug, Clone)]
pub struct CheckOptions {
    pub repo_root: PathBuf,
    /// If `Some`, additionally synthesise checks from the impact report.
    pub impact: Option<ImpactReport>,
}

pub fn run_checks(options: CheckOptions) -> Result<CheckReport> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    compute_checks(&store, options.impact.as_ref())
}

pub fn compute_checks(store: &Store, impact: Option<&ImpactReport>) -> Result<CheckReport> {
    let mut report = CheckReport::default();

    let requirement_ids: BTreeSet<_> = store
        .list_nodes_by_kind(NodeKind::Requirement)?
        .into_iter()
        .map(|n| n.id)
        .collect();
    let all_node_ids: BTreeSet<_> = store.list_all_nodes()?.into_iter().map(|n| n.id).collect();

    // broken_trace + broken_related: declared trace / related edges whose
    // target does not exist.
    for edge in store.list_all_edges()? {
        match edge.kind {
            EdgeKind::DeclaresImplementation | EdgeKind::DeclaresVerification => {
                if !all_node_ids.contains(&edge.to_id) {
                    report.findings.push(CheckFinding {
                        code: "broken_trace".into(),
                        severity: CheckSeverity::Error,
                        message: format!(
                            "Trace `{}` points to unknown target `{}`.",
                            edge.kind.as_str(),
                            edge.to_id
                        ),
                        artifact_id: Some(edge.from_id.to_string()),
                        path: None,
                    });
                }
                if !all_node_ids.contains(&edge.from_id) {
                    report.findings.push(CheckFinding {
                        code: "broken_trace".into(),
                        severity: CheckSeverity::Error,
                        message: format!(
                            "Trace `{}` originates from unknown symbol `{}`.",
                            edge.kind.as_str(),
                            edge.from_id
                        ),
                        artifact_id: Some(edge.from_id.to_string()),
                        path: None,
                    });
                }
            }
            EdgeKind::RelatedTo => {
                if !related_target_resolves(store, &edge.to_id, &all_node_ids)? {
                    report.findings.push(CheckFinding {
                        code: "broken_related".into(),
                        severity: CheckSeverity::Error,
                        message: format!(
                            "Related reference `{}` from `{}` does not resolve to any indexed artifact.",
                            edge.to_id, edge.from_id
                        ),
                        artifact_id: Some(edge.from_id.to_string()),
                        path: None,
                    });
                }
            }
            _ => {}
        }
    }

    // missing_linked_test / orphan_requirement.
    for req_id in &requirement_ids {
        let incoming = store.list_edges_to(req_id)?;
        let has_impl = incoming
            .iter()
            .any(|e| e.kind == EdgeKind::DeclaresImplementation);
        let has_test = incoming
            .iter()
            .any(|e| e.kind == EdgeKind::DeclaresVerification);
        let path = store.find_node(req_id)?.and_then(|n| n.path);
        if has_impl && !has_test {
            report.findings.push(CheckFinding {
                code: "missing_linked_test".into(),
                severity: CheckSeverity::Warning,
                message: format!(
                    "Requirement {} has declared implementation but no `@verifies` test.",
                    req_id
                ),
                artifact_id: Some(req_id.to_string()),
                path: path.clone(),
            });
        }
        if !has_impl && !has_test {
            report.findings.push(CheckFinding {
                code: "orphan_requirement".into(),
                severity: CheckSeverity::Warning,
                message: format!(
                    "Requirement {} has no declared implementation or test.",
                    req_id
                ),
                artifact_id: Some(req_id.to_string()),
                path,
            });
        }
    }

    if let Some(impact) = impact {
        for w in &impact.warnings {
            report.findings.push(CheckFinding {
                code: "impact_review".into(),
                severity: CheckSeverity::Warning,
                message: w.clone(),
                artifact_id: None,
                path: None,
            });
        }
        for info in &impact.info {
            report.findings.push(CheckFinding {
                code: "impact_review".into(),
                severity: CheckSeverity::Info,
                message: info.clone(),
                artifact_id: None,
                path: None,
            });
        }
    }

    Ok(report)
}

/// True when a Related edge's target can be matched against the graph.
///
/// Two shapes are accepted:
/// 1. A literal artifact id that already exists as a node (e.g. when Dart's
///    `@related` tag points at an indexed class).
/// 2. A `symbol://path#name` / `test://path#slug` URI generated by the docs
///    indexer. We resolve it by scanning nodes for one whose `path` and
///    `name` match the URI's components.
fn related_target_resolves(
    store: &Store,
    target: &ArtifactId,
    all_ids: &BTreeSet<ArtifactId>,
) -> Result<bool> {
    if all_ids.contains(target) {
        return Ok(true);
    }
    let raw = target.as_str();
    let (kinds_to_try, after, scheme) = if let Some(rest) = raw.strip_prefix("symbol://") {
        (
            vec![
                NodeKind::DartClass,
                NodeKind::DartMethod,
                NodeKind::DartFunction,
                NodeKind::DartConstructor,
            ],
            rest,
            "symbol",
        )
    } else if let Some(rest) = raw.strip_prefix("test://") {
        (vec![NodeKind::TestCase, NodeKind::TestGroup], rest, "test")
    } else if let Some(rest) = raw.strip_prefix("ref://") {
        (
            vec![
                NodeKind::DartClass,
                NodeKind::DartMethod,
                NodeKind::DartFunction,
                NodeKind::DartConstructor,
                NodeKind::TestCase,
                NodeKind::TestGroup,
                NodeKind::File,
            ],
            rest,
            "ref",
        )
    } else {
        return Ok(false);
    };
    let (path, name) = split_uri_target(after);

    // Fast path: try canonical artifact ids derived from the URI.
    if let (Some(p), Some(n)) = (path, name) {
        let candidate_ids: Vec<ArtifactId> = match scheme {
            "symbol" => vec![dart_class_id(p, n), dart_function_id(p, n)],
            "test" => vec![dart_test_id(p, n)],
            _ => vec![],
        };
        for id in candidate_ids {
            if all_ids.contains(&id) {
                return Ok(true);
            }
        }
    }
    if let Some(p) = path {
        if all_ids.contains(&file_id(p)) && name.is_none() {
            return Ok(true);
        }
    }

    // Slow path: linear scan tolerating differences between raw display
    // names and their slugged form.
    for kind in kinds_to_try {
        for node in store.list_nodes_by_kind(kind)? {
            let path_match = match (path, node.path.as_deref()) {
                (Some(p), Some(node_path)) => p == node_path,
                (None, _) => true,
                _ => false,
            };
            let name_match = match (name, node.name.as_deref()) {
                (Some(n), Some(node_name)) => n == node_name,
                (None, _) => true,
                _ => false,
            };
            if path_match && name_match {
                return Ok(true);
            }
            // Tolerate slug-vs-text drift: compare the URI fragment to a
            // slugged node name.
            if path_match {
                if let (Some(n), Some(node_name)) = (name, node.name.as_deref()) {
                    if specslice_core::artifact_id::slugify(node_name) == n {
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(false)
}

fn split_uri_target(after_scheme: &str) -> (Option<&str>, Option<&str>) {
    match after_scheme.split_once('#') {
        Some((p, n)) => {
            let p = if p.is_empty() { None } else { Some(p) };
            let n = if n.is_empty() { None } else { Some(n) };
            (p, n)
        }
        None => {
            if after_scheme.is_empty() {
                (None, None)
            } else {
                (Some(after_scheme), None)
            }
        }
    }
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
