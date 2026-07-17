//! P3 — Logic Confidence report.
//!
//! Confirmed graph 与 AI 候选层混在同一个 GroundGraph 仓库里时，
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
use groundgraph_core::{EdgeKind, NodeKind};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

use crate::business_candidates::{
    candidate_artifact_id, load_business_candidates, BusinessCandidate, ReviewStatus,
};
use crate::config::{resolve_storage_path, EngineConfig};
use crate::error::EngineResult;

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
pub fn run_logic_confidence(
    options: LogicConfidenceOptions,
) -> EngineResult<LogicConfidenceReport> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config)?;
    let store = Store::open(&db_path)?;
    compute_logic_confidence(&store, &options.repo_root)
}

pub fn compute_logic_confidence(
    store: &Store,
    repo_root: &Path,
) -> EngineResult<LogicConfidenceReport> {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let all_node_handles = store.list_all_nodes()?;

    // --- Requirements (confirmed graph) ---------------------------------
    let req_nodes = store.list_nodes_by_kind(NodeKind::Requirement)?;
    let all_node_ids: BTreeSet<_> = all_node_handles.iter().map(|n| n.id.clone()).collect();
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
                items.push(candidate_item(c, &all_node_ids, store, repo_root)?);
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
    all_nodes: &BTreeSet<groundgraph_core::ArtifactId>,
    store: &Store,
    repo_root: &Path,
) -> Result<LogicConfidenceItem> {
    let unresolved: Vec<String> = c
        .evidence
        .iter()
        .filter(|e| !all_nodes.iter().any(|id| id.as_str() == e.as_str()))
        .cloned()
        .collect();

    // Stale check: walk every resolvable evidence node, collect its
    // `source_file` (if any), and compare the stored hash against the
    // current on-disk hash. Any drift demotes the verdict away from
    // ConfirmedLink — code edits between `--accept` and the next
    // `groundgraph index` must surface.
    let mut stale_paths: Vec<String> = Vec::new();
    for ev in &c.evidence {
        if unresolved.iter().any(|u| u == ev) {
            continue;
        }
        let ev_id = groundgraph_core::ArtifactId::new(ev.clone());
        let Some(node) = store.find_node(&ev_id)? else {
            continue;
        };
        let Some(path) = node.path.as_deref().or(node.source_file.as_deref()) else {
            continue;
        };
        let abs = repo_root.join(path);
        let on_disk = if abs.exists() {
            file_hash(&abs).ok()
        } else {
            None
        };
        let stored = store.get_file_hash(path).ok().flatten();
        if let (Some(d), Some(s)) = (on_disk, stored) {
            if d != s && !stale_paths.iter().any(|p| p == path) {
                stale_paths.push(path.to_string());
            }
        }
    }

    let verdict = match c.review_status() {
        // Accepted only counts as `confirmed_link` when:
        //   1. evidence is non-empty (a candidate without any code
        //      hooks isn't really "confirmed" — it has nothing to
        //      anchor the human verdict to),
        //   2. every cited node still resolves in the graph, and
        //   3. no underlying source file has drifted from the indexed
        //      hash.
        Some(ReviewStatus::Accepted)
            if !c.evidence.is_empty() && unresolved.is_empty() && stale_paths.is_empty() =>
        {
            LogicConfidenceKind::ConfirmedLink
        }
        // Accepted but evidence-less → the human committed without
        // anchoring the claim. Treat the same as an un-linked
        // requirement: surface as MissingLink so the reviewer adds
        // evidence and re-runs the loop.
        Some(ReviewStatus::Accepted) if c.evidence.is_empty() => LogicConfidenceKind::MissingLink,
        // Accepted with broken / stale evidence → StaleLink (same
        // verdict as a requirement whose impl/doc file changed
        // between indexes).
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
    for p in &stale_paths {
        issues.push(format!("证据文件已变更 (需重新 index): {p}"));
    }
    if matches!(verdict, LogicConfidenceKind::MissingLink) && c.evidence.is_empty() {
        issues.push("接受时未提供任何证据 — 请补充 evidence 后重新审阅".into());
    }
    if !c.pending_open_questions().is_empty() {
        issues.push(format!(
            "待确认问题 {} 条",
            c.pending_open_questions().len()
        ));
    }
    Ok(LogicConfidenceItem {
        id: candidate_artifact_id(&c.id),
        kind: LogicConfidenceSource::BusinessCandidate,
        verdict,
        label_cn: verdict.label_cn().to_string(),
        title: c.name.clone(),
        path: None,
        confidence: c.confidence.map(|v| v.get()),
        issues,
        risks: c.risks.clone(),
    })
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

fn load_config(repo_root: &Path) -> crate::error::EngineResult<EngineConfig> {
    crate::config::load_config(repo_root)
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

    fn empty_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
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
        let (store, dir) = empty_store();
        let item = candidate_item(&c, &nodes, &store, dir.path()).unwrap();
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
        let (store, dir) = empty_store();
        let item = candidate_item(&c, &nodes, &store, dir.path()).unwrap();
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
        nodes.insert(groundgraph_core::ArtifactId::new("dart_method::a.dart#A.b"));
        let (store, dir) = empty_store();
        let item = candidate_item(&c, &nodes, &store, dir.path()).unwrap();
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
        nodes.insert(groundgraph_core::ArtifactId::new("dart_method::a.dart#A.b"));
        let (store, dir) = empty_store();
        let item = candidate_item(&c, &nodes, &store, dir.path()).unwrap();
        assert_eq!(item.verdict, LogicConfidenceKind::Rejected);
    }

    #[test]
    fn accepted_candidate_without_any_evidence_is_missing_link_not_confirmed() {
        // Reviewer feedback: an accepted candidate with empty evidence
        // got promoted to `ConfirmedLink` because `unresolved.is_empty()`
        // was true. That is wrong: a "confirmed" verdict must be anchored
        // to *something*. Empty evidence must classify as MissingLink so
        // the reviewer goes back and adds anchors.
        let c = BusinessCandidate {
            id: "x".into(),
            name: "n".into(),
            evidence: vec![],
            status: "accepted".into(),
            ..Default::default()
        };
        let nodes = BTreeSet::new();
        let (store, dir) = empty_store();
        let item = candidate_item(&c, &nodes, &store, dir.path()).unwrap();
        assert_eq!(
            item.verdict,
            LogicConfidenceKind::MissingLink,
            "empty-evidence accepted candidate must not be ConfirmedLink"
        );
        assert!(
            item.issues
                .iter()
                .any(|i| i.contains("接受时未提供任何证据")),
            "expected CN guidance, got: {:?}",
            item.issues
        );
    }

    #[test]
    fn accepted_candidate_with_stale_source_file_is_stale_link() {
        // Reviewer feedback: even when every evidence id resolves,
        // if the underlying source file changed since the last
        // `groundgraph index` run (store hash ≠ disk hash) the verdict
        // must drop from ConfirmedLink to StaleLink so the reviewer
        // re-confirms after re-indexing.
        let tmp = tempfile::TempDir::new().unwrap();
        let store_path = tmp.path().join("graph.db");
        let mut store = Store::open(&store_path).unwrap();
        store.migrate().unwrap();
        // Index a fake file with hash H_old.
        let rel = "lib/a.dart";
        let abs = tmp.path().join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, b"old contents").unwrap();
        let h_old = file_hash(&abs).unwrap();
        store
            .upsert_file_index(&groundgraph_store::FileIndexEntry {
                path: rel.into(),
                hash: h_old.clone(),
                kind: "dart".into(),
                indexed_at: "now".into(),
                index_generation: 1,
            })
            .unwrap();
        // Persist the symbol that the candidate cites; its source file
        // is the rel above.
        let ev_id_str = format!("dart_method::{rel}#A.b");
        let ev_id = groundgraph_core::ArtifactId::new(ev_id_str.clone());
        let node = groundgraph_core::Node {
            id: ev_id.clone(),
            kind: NodeKind::DartMethod,
            path: Some(rel.into()),
            name: Some("A.b".into()),
            start_line: Some(1),
            end_line: Some(2),
            content_hash: None,
            stable_key: None,
            source_file: Some(rel.into()),
            indexer: Some("test".into()),
            metadata_json: None,
        };
        store.upsert_node(&node).unwrap();
        // Now mutate the file on disk — the next confidence run must
        // notice and demote the verdict.
        std::fs::write(&abs, b"new contents").unwrap();
        let c = BusinessCandidate {
            id: "x".into(),
            name: "n".into(),
            evidence: vec![ev_id_str.clone()],
            status: "accepted".into(),
            ..Default::default()
        };
        let mut nodes = BTreeSet::new();
        nodes.insert(ev_id);
        let item = candidate_item(&c, &nodes, &store, tmp.path()).unwrap();
        assert_eq!(item.verdict, LogicConfidenceKind::StaleLink);
        assert!(
            item.issues.iter().any(|i| i.contains("证据文件已变更")),
            "expected stale-source CN message, got: {:?}",
            item.issues
        );
    }
}
