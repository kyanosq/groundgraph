//! P24 — Business documentation export (`specslice business-doc`).
//!
//! This is the *back half* of "build business documentation from code".
//! The pipeline is:
//!
//! ```text
//! specslice propose            -> evidence pack + prompt (business_pack.rs)
//!   -> AI writes business_logic.yaml   (grounded in the pack)
//!   -> specslice candidate review      (human accepts / rejects)
//!   -> specslice business-doc          (this module: accepted claims ->
//!                                        readable business document)
//! ```
//!
//! Where [`crate::business_pack`] turns *code facts* into an evidence
//! pack, this module turns the *human-accepted business claims*
//! ([`crate::business_candidates`]) back into a reader-facing document.
//! It resolves every `evidence:` id against the graph so the document
//! cites real, navigable code/doc/test artifacts (path + line range),
//! and it groups that evidence by role (code / docs / tests / framework
//! signals) so a non-author can audit the claim.
//!
//! By default only **accepted** candidates are exported — the document
//! is the trustworthy, human-confirmed view. `--include-proposed`
//! produces a *draft* that also lists not-yet-confirmed claims (clearly
//! marked), which is useful while the review loop is still in progress.
//!
//! The pass is read-only on both the YAML and the graph.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::{Node, NodeKind};
use specslice_store::Store;

use crate::business_candidates::{load_business_candidates, BusinessCandidate, ReviewStatus};
use crate::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};

pub const BUSINESS_DOC_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct BusinessDocOptions {
    pub repo_root: PathBuf,
    /// Include candidates that are not yet accepted (proposed / pending /
    /// needs_changes). They are tagged as drafts in the output.
    pub include_proposed: bool,
    /// Include candidates a reviewer explicitly rejected. Off by default.
    pub include_rejected: bool,
}

impl Default for BusinessDocOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            include_proposed: false,
            include_rejected: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusinessDoc {
    pub schema_version: u32,
    pub repo_root: String,
    pub stats: BusinessDocStats,
    pub entries: Vec<BusinessDocEntry>,
    /// Soft warnings forwarded from the candidate loader (invalid ids,
    /// duplicates, …) so the CLI can surface them without failing.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessDocStats {
    pub total_candidates: usize,
    pub included: usize,
    pub accepted: usize,
    pub proposed: usize,
    pub needs_changes: usize,
    pub pending: usize,
    pub rejected: usize,
    /// Evidence ids that did not resolve to a graph node (e.g. the code
    /// moved since the candidate was authored — a drift signal).
    pub unresolved_evidence: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusinessDocEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Machine status key: `accepted` / `proposed` / `needs_changes` /
    /// `pending` / `rejected`.
    pub status: String,
    pub confidence: Option<f32>,
    pub recommendation: Option<String>,
    pub reviewer: Option<String>,
    pub reviewed_at: Option<String>,
    pub review_note: Option<String>,
    /// Evidence grouped by role so a reader can audit the claim.
    pub code_evidence: Vec<DocEvidence>,
    pub doc_evidence: Vec<DocEvidence>,
    pub test_evidence: Vec<DocEvidence>,
    pub signal_evidence: Vec<DocEvidence>,
    /// Cited ids that no longer resolve to a node (drift).
    pub unresolved_evidence: Vec<String>,
    pub open_questions: Vec<String>,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocEvidence {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub path: Option<String>,
    pub line_range: Option<(u32, u32)>,
}

/// Open the store + candidates file from the repo root and assemble the
/// document.
pub fn build_business_doc(options: BusinessDocOptions) -> Result<BusinessDoc> {
    let loaded = load_business_candidates(&options.repo_root)?;
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);
    let store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    let nodes = store.list_all_nodes().context("listing nodes")?;
    let nodes_by_id: HashMap<String, &Node> =
        nodes.iter().map(|n| (n.id.to_string(), n)).collect();
    let mut doc = assemble_doc(
        &options.repo_root.to_string_lossy(),
        &loaded.document.candidates,
        &nodes_by_id,
        &options,
    );
    doc.warnings = loaded.warnings;
    Ok(doc)
}

/// Pure assembly step: turn candidates + a node lookup into a document.
/// Kept separate from store/YAML I/O so it is exhaustively testable.
fn assemble_doc(
    repo_root: &str,
    candidates: &[BusinessCandidate],
    nodes_by_id: &HashMap<String, &Node>,
    options: &BusinessDocOptions,
) -> BusinessDoc {
    let mut stats = BusinessDocStats {
        total_candidates: candidates.len(),
        ..Default::default()
    };
    let mut entries: Vec<BusinessDocEntry> = Vec::new();

    for c in candidates {
        let status = effective_status(c);
        match status {
            EntryStatus::Accepted => stats.accepted += 1,
            EntryStatus::Proposed => stats.proposed += 1,
            EntryStatus::NeedsChanges => stats.needs_changes += 1,
            EntryStatus::Pending => stats.pending += 1,
            EntryStatus::Rejected => stats.rejected += 1,
        }
        if !status.is_included(options) {
            continue;
        }

        let mut code = Vec::new();
        let mut docs = Vec::new();
        let mut tests = Vec::new();
        let mut signals = Vec::new();
        let mut unresolved = Vec::new();
        for ev in &c.evidence {
            match nodes_by_id.get(ev) {
                Some(node) => {
                    let de = doc_evidence(node);
                    match bucket_of(node.kind) {
                        Bucket::Doc => docs.push(de),
                        Bucket::Test => tests.push(de),
                        Bucket::Signal => signals.push(de),
                        Bucket::Code => code.push(de),
                    }
                }
                None => unresolved.push(ev.clone()),
            }
        }
        stats.unresolved_evidence += unresolved.len();

        let review = c.review.as_ref();
        entries.push(BusinessDocEntry {
            id: c.id.clone(),
            name: c.name.clone(),
            description: c.description.trim().to_string(),
            status: status.key().to_string(),
            confidence: c.confidence,
            recommendation: c.recommendation.clone(),
            reviewer: review.and_then(|r| r.reviewer.clone()),
            reviewed_at: review.and_then(|r| r.reviewed_at.clone()),
            review_note: review.and_then(|r| r.note.clone()),
            code_evidence: code,
            doc_evidence: docs,
            test_evidence: tests,
            signal_evidence: signals,
            unresolved_evidence: unresolved,
            open_questions: c.open_questions.clone(),
            risks: c.risks.clone(),
        });
    }

    // Order: accepted first, then by confidence desc, then by name.
    entries.sort_by(|a, b| {
        status_rank(&a.status)
            .cmp(&status_rank(&b.status))
            .then(
                b.confidence
                    .unwrap_or(0.0)
                    .partial_cmp(&a.confidence.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.name.cmp(&b.name))
    });

    stats.included = entries.len();
    BusinessDoc {
        schema_version: BUSINESS_DOC_SCHEMA_VERSION,
        repo_root: repo_root.to_string(),
        stats,
        entries,
        warnings: Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryStatus {
    Accepted,
    Rejected,
    NeedsChanges,
    Pending,
    Proposed,
}

impl EntryStatus {
    fn key(self) -> &'static str {
        match self {
            EntryStatus::Accepted => "accepted",
            EntryStatus::Rejected => "rejected",
            EntryStatus::NeedsChanges => "needs_changes",
            EntryStatus::Pending => "pending",
            EntryStatus::Proposed => "proposed",
        }
    }

    fn is_included(self, options: &BusinessDocOptions) -> bool {
        match self {
            EntryStatus::Accepted => true,
            EntryStatus::Rejected => options.include_rejected,
            EntryStatus::NeedsChanges | EntryStatus::Pending | EntryStatus::Proposed => {
                options.include_proposed
            }
        }
    }
}

fn effective_status(c: &BusinessCandidate) -> EntryStatus {
    match c.review_status() {
        Some(ReviewStatus::Accepted) => EntryStatus::Accepted,
        Some(ReviewStatus::Rejected) => EntryStatus::Rejected,
        Some(ReviewStatus::NeedsChanges) => EntryStatus::NeedsChanges,
        Some(ReviewStatus::Pending) => EntryStatus::Pending,
        None => EntryStatus::Proposed,
    }
}

fn status_rank(key: &str) -> u8 {
    match key {
        "accepted" => 0,
        "needs_changes" => 1,
        "pending" => 2,
        "proposed" => 3,
        "rejected" => 4,
        _ => 5,
    }
}

enum Bucket {
    Code,
    Doc,
    Test,
    Signal,
}

fn bucket_of(kind: NodeKind) -> Bucket {
    use NodeKind::*;
    match kind {
        DocSection | Requirement | AcceptanceCriterion | Adr => Bucket::Doc,
        Route | DartProvider | Storage => Bucket::Signal,
        _ if kind.is_test() => Bucket::Test,
        _ => Bucket::Code,
    }
}

fn doc_evidence(node: &Node) -> DocEvidence {
    DocEvidence {
        id: node.id.to_string(),
        kind: node.kind.as_str().to_string(),
        name: node
            .name
            .clone()
            .or_else(|| node.stable_key.clone())
            .unwrap_or_else(|| node.id.to_string()),
        path: node.path.clone(),
        line_range: match (node.start_line, node.end_line) {
            (Some(s), Some(e)) => Some((s, e)),
            _ => None,
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::ArtifactId;

    fn node(id: &str, kind: NodeKind, path: &str, name: &str) -> Node {
        Node {
            id: ArtifactId::new(id.to_string()),
            kind,
            path: Some(path.to_string()),
            name: Some(name.to_string()),
            start_line: Some(10),
            end_line: Some(42),
            content_hash: None,
            stable_key: None,
            source_file: Some(path.to_string()),
            source_hash: None,
            indexer: Some("test".into()),
            index_generation: None,
            metadata_json: None,
        }
    }

    fn candidate(id: &str, name: &str, status: &str, evidence: &[&str]) -> BusinessCandidate {
        BusinessCandidate {
            id: id.to_string(),
            name: name.to_string(),
            description: format!("{name} 的业务描述。"),
            evidence: evidence.iter().map(|s| s.to_string()).collect(),
            confidence: Some(0.8),
            status: status.to_string(),
            ..Default::default()
        }
    }

    fn nodes_map<'a>(nodes: &'a [Node]) -> HashMap<String, &'a Node> {
        nodes.iter().map(|n| (n.id.to_string(), n)).collect()
    }

    #[test]
    fn only_accepted_candidates_export_by_default() {
        let nodes = vec![node(
            "dart_class::lib/features/auth/auth_bloc.dart#AuthBloc",
            NodeKind::DartClass,
            "lib/features/auth/auth_bloc.dart",
            "AuthBloc",
        )];
        let map = nodes_map(&nodes);
        let candidates = vec![
            candidate(
                "auth",
                "登录鉴权",
                "accepted",
                &["dart_class::lib/features/auth/auth_bloc.dart#AuthBloc"],
            ),
            candidate("draft", "草稿能力", "proposed", &[]),
        ];

        let doc = assemble_doc(".", &candidates, &map, &BusinessDocOptions::default());
        assert_eq!(doc.stats.total_candidates, 2);
        assert_eq!(doc.stats.accepted, 1);
        assert_eq!(doc.stats.proposed, 1);
        assert_eq!(doc.stats.included, 1, "only accepted exported by default");
        assert_eq!(doc.entries.len(), 1);
        assert_eq!(doc.entries[0].id, "auth");
        assert_eq!(doc.entries[0].status, "accepted");
        assert_eq!(doc.entries[0].code_evidence.len(), 1);
        assert_eq!(
            doc.entries[0].code_evidence[0].path.as_deref(),
            Some("lib/features/auth/auth_bloc.dart")
        );
    }

    #[test]
    fn include_proposed_adds_drafts() {
        let map: HashMap<String, &Node> = HashMap::new();
        let candidates = vec![
            candidate("a", "已确认能力", "accepted", &[]),
            candidate("b", "草稿能力", "proposed", &[]),
        ];
        let opts = BusinessDocOptions {
            include_proposed: true,
            ..Default::default()
        };
        let doc = assemble_doc(".", &candidates, &map, &opts);
        assert_eq!(doc.stats.included, 2);
        // accepted ranks before proposed
        assert_eq!(doc.entries[0].id, "a");
        assert_eq!(doc.entries[1].id, "b");
        assert_eq!(doc.entries[1].status, "proposed");
    }

    #[test]
    fn evidence_is_grouped_by_role_and_unresolved_tracked() {
        let nodes = vec![
            node(
                "dart_class::lib/features/cart/cart_bloc.dart#CartBloc",
                NodeKind::DartClass,
                "lib/features/cart/cart_bloc.dart",
                "CartBloc",
            ),
            node(
                "doc_section::docs/cart.md#Cart",
                NodeKind::DocSection,
                "docs/cart.md",
                "Cart",
            ),
            node(
                "test_case::test/cart_test.dart#adds item",
                NodeKind::TestCase,
                "test/cart_test.dart",
                "adds item",
            ),
            node("route::/cart", NodeKind::Route, "route::/cart", "/cart"),
        ];
        let map = nodes_map(&nodes);
        let candidates = vec![candidate(
            "cart",
            "购物车",
            "accepted",
            &[
                "dart_class::lib/features/cart/cart_bloc.dart#CartBloc",
                "doc_section::docs/cart.md#Cart",
                "test_case::test/cart_test.dart#adds item",
                "route::/cart",
                "dart_class::lib/features/cart/GONE.dart#Gone",
            ],
        )];
        let doc = assemble_doc(".", &candidates, &map, &BusinessDocOptions::default());
        let e = &doc.entries[0];
        assert_eq!(e.code_evidence.len(), 1, "code symbol");
        assert_eq!(e.doc_evidence.len(), 1, "doc section");
        assert_eq!(e.test_evidence.len(), 1, "test case");
        assert_eq!(e.signal_evidence.len(), 1, "route signal");
        assert_eq!(
            e.unresolved_evidence,
            vec!["dart_class::lib/features/cart/GONE.dart#Gone".to_string()],
            "missing evidence flagged as drift"
        );
        assert_eq!(doc.stats.unresolved_evidence, 1);
    }

    #[test]
    fn legacy_confirmed_status_is_accepted() {
        let map: HashMap<String, &Node> = HashMap::new();
        let candidates = vec![candidate("x", "历史能力", "confirmed", &[])];
        let doc = assemble_doc(".", &candidates, &map, &BusinessDocOptions::default());
        assert_eq!(doc.stats.accepted, 1);
        assert_eq!(doc.entries.len(), 1, "legacy confirmed counts as accepted");
    }
}
