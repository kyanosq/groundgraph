//! Basic SpecSlice checks.
//!
//! MVP-5 scope (PRD §6 / implementation plan §MVP-5):
//! - `broken_link`: a manifest-declared relationship points to a node that
//!   does not exist.
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
use specslice_core::{EdgeKind, NodeKind};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckPolicy {
    pub broken_link_level: String,
    pub missing_linked_test_level: String,
    pub orphan_requirement_level: String,
}

impl Default for CheckPolicy {
    fn default() -> Self {
        Self {
            broken_link_level: "error".into(),
            missing_linked_test_level: "warning".into(),
            orphan_requirement_level: "warning".into(),
        }
    }
}

impl From<&crate::config::ChecksConfig> for CheckPolicy {
    fn from(value: &crate::config::ChecksConfig) -> Self {
        Self {
            broken_link_level: value.broken_link_level.clone(),
            missing_linked_test_level: value.missing_linked_test_level.clone(),
            orphan_requirement_level: value.orphan_requirement_level.clone(),
        }
    }
}

pub fn run_checks(options: CheckOptions) -> Result<CheckReport> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    compute_checks_with_policy(
        &store,
        options.impact.as_ref(),
        CheckPolicy::from(&config.checks),
    )
}

pub fn compute_checks(store: &Store, impact: Option<&ImpactReport>) -> Result<CheckReport> {
    compute_checks_with_policy(store, impact, CheckPolicy::default())
}

pub fn compute_checks_with_policy(
    store: &Store,
    impact: Option<&ImpactReport>,
    policy: CheckPolicy,
) -> Result<CheckReport> {
    let mut report = CheckReport::default();

    let requirement_ids: BTreeSet<_> = store
        .list_nodes_by_kind(NodeKind::Requirement)?
        .into_iter()
        .map(|n| n.id)
        .collect();
    let all_node_ids: BTreeSet<_> = store.list_all_nodes()?.into_iter().map(|n| n.id).collect();

    // Manifest-declared relationship edges whose endpoints do not exist.
    for edge in store.list_all_edges()? {
        match edge.kind {
            EdgeKind::Documents
            | EdgeKind::DeclaresImplementation
            | EdgeKind::DeclaresVerification => {
                if !all_node_ids.contains(&edge.to_id) {
                    push_configured_finding(
                        &mut report,
                        &policy.broken_link_level,
                        "broken_link",
                        format!(
                            "Link `{}` points to unknown target `{}`.",
                            edge.kind.as_str(),
                            edge.to_id
                        ),
                        Some(edge.from_id.to_string()),
                        None,
                    );
                }
                if !all_node_ids.contains(&edge.from_id) {
                    push_configured_finding(
                        &mut report,
                        &policy.broken_link_level,
                        "broken_link",
                        format!(
                            "Link `{}` originates from unknown artifact `{}`.",
                            edge.kind.as_str(),
                            edge.from_id
                        ),
                        Some(edge.from_id.to_string()),
                        None,
                    );
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
            push_configured_finding(
                &mut report,
                &policy.missing_linked_test_level,
                "missing_linked_test",
                format!(
                    "Requirement {} has linked implementation but no linked verification test.",
                    req_id
                ),
                Some(req_id.to_string()),
                path.clone(),
            );
        }
        if !has_impl && !has_test {
            push_configured_finding(
                &mut report,
                &policy.orphan_requirement_level,
                "orphan_requirement",
                format!(
                    "Requirement {} has no declared implementation or test.",
                    req_id
                ),
                Some(req_id.to_string()),
                path,
            );
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

fn push_configured_finding(
    report: &mut CheckReport,
    level: &str,
    code: &str,
    message: String,
    artifact_id: Option<String>,
    path: Option<String>,
) {
    let Some(severity) = severity_from_level(level) else {
        return;
    };
    report.findings.push(CheckFinding {
        code: code.into(),
        severity,
        message,
        artifact_id,
        path,
    });
}

fn severity_from_level(level: &str) -> Option<CheckSeverity> {
    match level.trim().to_ascii_lowercase().as_str() {
        "error" => Some(CheckSeverity::Error),
        "warning" | "warn" => Some(CheckSeverity::Warning),
        "info" => Some(CheckSeverity::Info),
        "off" | "none" | "ignore" => None,
        _ => Some(CheckSeverity::Warning),
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
