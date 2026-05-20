//! P3 — Logic Confidence report.
//!
//! Confirmed graph 与 AI 候选层混在同一个 SpecSlice 仓库里时，
//! 用户最关心的问题不是「哪些边存在」而是「哪些业务逻辑可信」。
//! 这个模块把 graph 里的 requirement / business_candidate 拍平成一
//! 张「可信度报表」，按以下分类标注：
//!
//! - `confirmed_link`：requirement / candidate 已被人工审阅接受，
//!   并且其链接的文档 / 实现 / 测试都在当前索引里能找到。
//! - `candidate_only`：AI 提出但尚未被人工审阅 (review_status = None
//!   或 pending)；不进入 confirmed slice。
//! - `needs_changes`：reviewer 标记 needs_changes — 还需要补充测试 /
//!   产品边界 / 实现，不能直接进入 confirmed graph。
//! - `rejected`：reviewer 已拒绝；保留为「不要再次提出」。
//! - `missing_doc`：candidate / requirement 缺业务文档，但仍有代码 /
//!   测试事实可以引用。
//! - `missing_link`：requirement 没有任何 docs/impl/test 关联。
//! - `missing_test`：有实现但没有 declared verification。
//! - `stale_link`：linked file 的 hash 与 file_index 不一致（已编辑
//!   但未重新 index），需要复核。
//! - `unknown`：证据不足，无法分类（用于兜底，不应频繁出现）。
//!
//! 报告设计为「只读，可序列化」，CLI 直接 `serde_json::to_string_pretty`
//! 就能输出 machine-readable JSON。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{EdgeKind, NodeKind};
use specslice_store::Store;

use crate::business_candidates::{
    candidate_artifact_id, load_business_candidates, BusinessCandidate, ReviewStatus,
};
use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};

/// Confidence verdict per item. Order in this enum reflects display
/// priority (most actionable first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicConfidenceKind {
    ConfirmedLink,
    StaleLink,
    NeedsChanges,
    MissingTest,
    MissingDoc,
    MissingLink,
    CandidateOnly,
    Rejected,
    Unknown,
}

impl LogicConfidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LogicConfidenceKind::ConfirmedLink => "confirmed_link",
            LogicConfidenceKind::StaleLink => "stale_link",
            LogicConfidenceKind::NeedsChanges => "needs_changes",
            LogicConfidenceKind::MissingTest => "missing_test",
            LogicConfidenceKind::MissingDoc => "missing_doc",
            LogicConfidenceKind::MissingLink => "missing_link",
            LogicConfidenceKind::CandidateOnly => "candidate_only",
            LogicConfidenceKind::Rejected => "rejected",
            LogicConfidenceKind::Unknown => "unknown",
        }
    }

    pub fn label_cn(self) -> &'static str {
        match self {
            LogicConfidenceKind::ConfirmedLink => "已确认",
            LogicConfidenceKind::StaleLink => "需复核 (文件已变更)",
            LogicConfidenceKind::NeedsChanges => "需补充",
            LogicConfidenceKind::MissingTest => "缺验证测试",
            LogicConfidenceKind::MissingDoc => "缺业务文档",
            LogicConfidenceKind::MissingLink => "未关联任何 docs/impl/test",
            LogicConfidenceKind::CandidateOnly => "AI 候选 (未审阅)",
            LogicConfidenceKind::Rejected => "已拒绝",
            LogicConfidenceKind::Unknown => "证据不足",
        }
    }

    /// Whether this verdict should be treated as a risk (i.e. not
    /// `confirmed_link`). CLI uses this to filter with `--only-risks`.
    pub fn is_risk(self) -> bool {
        !matches!(self, LogicConfidenceKind::ConfirmedLink)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicConfidenceSource {
    Requirement,
    BusinessCandidate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicConfidenceItem {
    pub id: String,
    pub kind: LogicConfidenceSource,
    pub verdict: LogicConfidenceKind,
    /// One-line Chinese label of the verdict — pre-rendered so the CLI
    /// doesn't have to know about the enum mapping.
    pub label_cn: String,
    /// Short human-readable name (`Node::name` for requirements,
    /// `BusinessCandidate::name` for candidates).
    pub title: String,
    /// Optional path of the underlying spec / doc / yaml.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Reviewer-set confidence (0..1) when the candidate has been
    /// reviewed; AI-set confidence otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Issues that influenced the verdict — paths or candidate ids that
    /// look stale, missing tests, etc. Surfaces in CLI human output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
    /// AI-authored risks copied over from the candidate (if any) so the
    /// confidence report can stand alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LogicConfidenceReport {
    pub repo_root: String,
    pub items: Vec<LogicConfidenceItem>,
    pub summary: LogicConfidenceSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LogicConfidenceSummary {
    pub confirmed_link: usize,
    pub stale_link: usize,
    pub needs_changes: usize,
    pub missing_test: usize,
    pub missing_doc: usize,
    pub missing_link: usize,
    pub candidate_only: usize,
    pub rejected: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone)]
pub struct LogicConfidenceOptions {
    pub repo_root: PathBuf,
}

/// Build the confidence report from a workspace. Read-only: nothing on
/// disk is mutated.
pub fn run_logic_confidence(options: LogicConfidenceOptions) -> Result<LogicConfidenceReport> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    compute_logic_confidence(&store, &options.repo_root)
}

pub fn compute_logic_confidence(store: &Store, repo_root: &Path) -> Result<LogicConfidenceReport> {
    let mut items = Vec::new();
    let mut warnings = Vec::new();

    // --- Requirements (confirmed graph) ---------------------------------
    let req_nodes = store.list_nodes_by_kind(NodeKind::Requirement)?;
    let all_node_ids: BTreeSet<_> = store.list_all_nodes()?.into_iter().map(|n| n.id).collect();
    for req in req_nodes {
        let incoming = store.list_edges_to(&req.id)?;
        let mut linked_paths: Vec<String> = Vec::new();
        let mut has_impl = false;
        let mut has_test = false;
        let mut has_doc = false;
        let mut broken: Vec<String> = Vec::new();
        for e in &incoming {
            match e.kind {
                EdgeKind::DeclaresImplementation => has_impl = true,
                EdgeKind::DeclaresVerification => has_test = true,
                EdgeKind::Documents => has_doc = true,
                _ => continue,
            }
            if !all_node_ids.contains(&e.from_id) {
                broken.push(format!(
                    "{} -> {} (broken)",
                    e.kind.as_str(),
                    e.from_id.as_str()
                ));
                continue;
            }
            if let Some(node) = store.find_node(&e.from_id)? {
                if let Some(p) = node.path {
                    linked_paths.push(p);
                }
            }
        }
        linked_paths.sort();
        linked_paths.dedup();

        let stale: Vec<String> = linked_paths
            .iter()
            .filter_map(|p| {
                let abs = repo_root.join(p);
                let on_disk = if abs.exists() {
                    file_hash(&abs).ok()
                } else {
                    None
                };
                let stored = store.get_file_hash(p).ok().flatten();
                match (on_disk, stored) {
                    (Some(d), Some(s)) if d != s => Some(format!("stale: {p}")),
                    _ => None,
                }
            })
            .collect();

        let verdict = if !broken.is_empty() || !stale.is_empty() {
            LogicConfidenceKind::StaleLink
        } else if !has_impl && !has_test && !has_doc {
            LogicConfidenceKind::MissingLink
        } else if !has_test && has_impl {
            LogicConfidenceKind::MissingTest
        } else if !has_doc {
            LogicConfidenceKind::MissingDoc
        } else {
            LogicConfidenceKind::ConfirmedLink
        };
        let mut issues = broken;
        issues.extend(stale);
        items.push(LogicConfidenceItem {
            id: req.stable_key.clone().unwrap_or_else(|| req.id.to_string()),
            kind: LogicConfidenceSource::Requirement,
            verdict,
            label_cn: verdict.label_cn().to_string(),
            title: req
                .name
                .clone()
                .or(req.stable_key.clone())
                .unwrap_or_else(|| req.id.to_string()),
            path: req.path.clone(),
            confidence: None,
            issues,
            risks: Vec::new(),
        });
    }

    // --- Business candidates (P9 layer) ----------------------------------
    match load_business_candidates(repo_root) {
        Ok(outcome) => {
            for w in &outcome.warnings {
                warnings.push(w.clone());
            }
            for c in &outcome.document.candidates {
                items.push(candidate_item(c, &all_node_ids));
            }
        }
        Err(e) => warnings.push(format!("加载业务候选失败：{e}")),
    }

    // Sort by descending priority of verdict (risks first), then id.
    items.sort_by(|a, b| {
        verdict_order(a.verdict)
            .cmp(&verdict_order(b.verdict))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut summary = LogicConfidenceSummary::default();
    for it in &items {
        match it.verdict {
            LogicConfidenceKind::ConfirmedLink => summary.confirmed_link += 1,
            LogicConfidenceKind::StaleLink => summary.stale_link += 1,
            LogicConfidenceKind::NeedsChanges => summary.needs_changes += 1,
            LogicConfidenceKind::MissingTest => summary.missing_test += 1,
            LogicConfidenceKind::MissingDoc => summary.missing_doc += 1,
            LogicConfidenceKind::MissingLink => summary.missing_link += 1,
            LogicConfidenceKind::CandidateOnly => summary.candidate_only += 1,
            LogicConfidenceKind::Rejected => summary.rejected += 1,
            LogicConfidenceKind::Unknown => summary.unknown += 1,
        }
    }

    Ok(LogicConfidenceReport {
        repo_root: repo_root.to_string_lossy().into_owned(),
        items,
        summary,
        warnings,
    })
}

fn candidate_item(
    c: &BusinessCandidate,
    all_nodes: &BTreeSet<specslice_core::ArtifactId>,
) -> LogicConfidenceItem {
    let unresolved: Vec<String> = c
        .evidence
        .iter()
        .filter(|e| !all_nodes.iter().any(|id| id.as_str() == e.as_str()))
        .cloned()
        .collect();
    let verdict = match c.review_status() {
        Some(ReviewStatus::Accepted) if unresolved.is_empty() => LogicConfidenceKind::ConfirmedLink,
        Some(ReviewStatus::Accepted) => LogicConfidenceKind::StaleLink,
        Some(ReviewStatus::Rejected) => LogicConfidenceKind::Rejected,
        Some(ReviewStatus::NeedsChanges) => LogicConfidenceKind::NeedsChanges,
        Some(ReviewStatus::Pending) => LogicConfidenceKind::CandidateOnly,
        None if c.evidence.is_empty() => LogicConfidenceKind::Unknown,
        None => LogicConfidenceKind::CandidateOnly,
    };
    let mut issues = Vec::new();
    for u in &unresolved {
        issues.push(format!("证据未解析: {u}"));
    }
    if !c.pending_open_questions().is_empty() {
        issues.push(format!(
            "待确认问题 {} 条",
            c.pending_open_questions().len()
        ));
    }
    LogicConfidenceItem {
        id: candidate_artifact_id(&c.id),
        kind: LogicConfidenceSource::BusinessCandidate,
        verdict,
        label_cn: verdict.label_cn().to_string(),
        title: c.name.clone(),
        path: None,
        confidence: c.confidence,
        issues,
        risks: c.risks.clone(),
    }
}

fn verdict_order(v: LogicConfidenceKind) -> u8 {
    match v {
        LogicConfidenceKind::StaleLink => 0,
        LogicConfidenceKind::NeedsChanges => 1,
        LogicConfidenceKind::MissingTest => 2,
        LogicConfidenceKind::MissingDoc => 3,
        LogicConfidenceKind::MissingLink => 4,
        LogicConfidenceKind::CandidateOnly => 5,
        LogicConfidenceKind::Unknown => 6,
        LogicConfidenceKind::Rejected => 7,
        LogicConfidenceKind::ConfirmedLink => 8,
    }
}

fn file_hash(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes =
        std::fs::read(path).with_context(|| format!("reading {} for hash", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_label_and_string_round_trip() {
        for v in [
            LogicConfidenceKind::ConfirmedLink,
            LogicConfidenceKind::StaleLink,
            LogicConfidenceKind::NeedsChanges,
            LogicConfidenceKind::MissingTest,
            LogicConfidenceKind::MissingDoc,
            LogicConfidenceKind::MissingLink,
            LogicConfidenceKind::CandidateOnly,
            LogicConfidenceKind::Rejected,
            LogicConfidenceKind::Unknown,
        ] {
            assert!(!v.as_str().is_empty(), "{v:?} should have a code");
            assert!(!v.label_cn().is_empty(), "{v:?} should have a label");
        }
        assert!(LogicConfidenceKind::StaleLink.is_risk());
        assert!(!LogicConfidenceKind::ConfirmedLink.is_risk());
    }

    #[test]
    fn verdict_order_puts_stale_first_and_confirmed_last() {
        assert!(
            verdict_order(LogicConfidenceKind::StaleLink)
                < verdict_order(LogicConfidenceKind::CandidateOnly)
        );
        assert!(
            verdict_order(LogicConfidenceKind::CandidateOnly)
                < verdict_order(LogicConfidenceKind::ConfirmedLink)
        );
    }

    #[test]
    fn unreviewed_candidate_classifies_candidate_only() {
        let c = BusinessCandidate {
            id: "x".into(),
            name: "n".into(),
            evidence: vec!["dart_method::a.dart#A.b".into()],
            ..Default::default()
        };
        let nodes = BTreeSet::new();
        let item = candidate_item(&c, &nodes);
        assert_eq!(item.verdict, LogicConfidenceKind::CandidateOnly);
        assert!(item.issues.iter().any(|i| i.contains("证据未解析")));
    }

    #[test]
    fn accepted_candidate_with_unresolved_evidence_is_stale() {
        let c = BusinessCandidate {
            id: "x".into(),
            name: "n".into(),
            evidence: vec!["dart_method::missing.dart#X.y".into()],
            status: "accepted".into(),
            ..Default::default()
        };
        let nodes = BTreeSet::new();
        let item = candidate_item(&c, &nodes);
        assert_eq!(item.verdict, LogicConfidenceKind::StaleLink);
    }

    #[test]
    fn accepted_candidate_with_resolved_evidence_is_confirmed_link() {
        let c = BusinessCandidate {
            id: "x".into(),
            name: "n".into(),
            evidence: vec!["dart_method::a.dart#A.b".into()],
            status: "accepted".into(),
            ..Default::default()
        };
        let mut nodes = BTreeSet::new();
        nodes.insert(specslice_core::ArtifactId::new("dart_method::a.dart#A.b"));
        let item = candidate_item(&c, &nodes);
        assert_eq!(item.verdict, LogicConfidenceKind::ConfirmedLink);
    }

    #[test]
    fn rejected_candidate_classifies_rejected_even_with_resolved_evidence() {
        let c = BusinessCandidate {
            id: "x".into(),
            name: "n".into(),
            evidence: vec!["dart_method::a.dart#A.b".into()],
            status: "rejected".into(),
            ..Default::default()
        };
        let mut nodes = BTreeSet::new();
        nodes.insert(specslice_core::ArtifactId::new("dart_method::a.dart#A.b"));
        let item = candidate_item(&c, &nodes);
        assert_eq!(item.verdict, LogicConfidenceKind::Rejected);
    }
}
