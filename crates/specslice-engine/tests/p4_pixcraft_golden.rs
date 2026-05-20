//! P4 — Pixcraft golden regression.
//!
//! The Pixcraft fixture under `tests/fixtures/pixcraft_iap` is our
//! production-shaped end-to-end exercise. It covers four feature
//! slices that the real Pixcraft app ships:
//!   * IAP (`lib/core/iap` + `lib/core/settings` + `lib/features/paywall`)
//!   * Editor controller   (`lib/features/editor`)
//!   * Layer repository    (`lib/features/layers`)
//!   * Project lifecycle   (`lib/features/project`)
//!
//! This file *golden-pins* the closed-loop result that an end user
//! sees when they run, in order:
//!   1. `specslice init`       (P0)
//!   2. `specslice index`      (P2 — dart_analyzer sidecar)
//!   3. `specslice candidate list / review`   (P1 + P5)
//!   4. `specslice logic`                     (P3)
//!
//! It exists primarily as a regression net: when somebody changes the
//! sidecar walker or the candidate schema, this test tells them
//! exactly which downstream surface (semantic edges, candidate
//! lifecycle, logic confidence summary) drifted.
//!
//! Like the rest of the dart-analyzer-dependent tests it skips when
//! the Dart SDK or the sidecar source isn't on disk.

use std::path::PathBuf;

use specslice_engine::business_candidates::{
    apply_review, candidate_artifact_id, list_for_review, load_business_candidates, ReviewStatus,
    ReviewVerdict,
};
use specslice_engine::dart_indexer::{index_dart, DartIndexOptions, RESOLVER_DART_ANALYZER};
use specslice_engine::graph::{build_graph_view, GraphOptions, GraphView};
use specslice_engine::init::{init_repository, InitOptions};
use specslice_engine::logic_confidence::{
    compute_logic_confidence, LogicConfidenceKind, LogicConfidenceSource,
};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures/pixcraft_iap")
}

fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn copy_fixture_into(dst: &std::path::Path) {
    let src = fixture_path();
    for entry in walkdir::WalkDir::new(&src) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&src).unwrap();
        let target = dst.join(rel);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(entry.path(), &target).unwrap();
    }
}

fn dart_available() -> bool {
    std::process::Command::new("dart")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn sidecar_source_present() -> bool {
    workspace_dir()
        .join("tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart")
        .exists()
}

struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: Option<&str>) -> Self {
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        Self {
            key: key.into(),
            prev,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(p) => std::env::set_var(&self.key, p),
            None => std::env::remove_var(&self.key),
        }
    }
}

fn setup_indexed_repo() -> Option<(tempfile::TempDir, EnvGuard, EnvGuard)> {
    if !sidecar_source_present() || !dart_available() {
        eprintln!("skipping: dart sidecar unavailable");
        return None;
    }
    let on = EnvGuard::set("SPECSLICE_DART_ANALYZER", Some("1"));
    let sidecar_abs =
        workspace_dir().join("tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart");
    let bin = EnvGuard::set(
        "SPECSLICE_DART_ANALYZER_BIN",
        Some(&format!("dart run {}", sidecar_abs.display())),
    );

    let tmp = tempfile::TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    copy_fixture_into(tmp.path());

    let mut store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    store.migrate().unwrap();
    let result = index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: tmp.path().into(),
            code_roots: vec!["lib".into(), "test".into()],
            exclude_globs: vec![],
        },
    )
    .unwrap();
    assert_eq!(
        result.resolver_used, RESOLVER_DART_ANALYZER,
        "P4 golden requires the sidecar to actually run"
    );
    Some((tmp, on, bin))
}

#[test]
fn p4_sidecar_resolves_all_four_feature_slices_with_semantic_edges() {
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };

    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Code,
            ..Default::default()
        },
    )
    .unwrap();

    // (a) Storage buckets: every Hive box mentioned in the fixture
    //     surfaces as a synthetic `storage` node. We pin the exact set
    //     because losing one means the body visitor regressed on local
    //     variable tracking.
    let storage_ids: std::collections::BTreeSet<&str> = view
        .nodes
        .iter()
        .filter(|n| n.kind == "storage")
        .map(|n| n.id.as_str())
        .collect();
    for expected in [
        "storage::hive::pro_entitlement",
        "storage::hive::editor_state",
        "storage::hive::project_layers",
        "storage::hive::projects",
    ] {
        assert!(
            storage_ids.contains(expected),
            "missing storage node {expected}; got {storage_ids:?}"
        );
    }

    // (b) Routes: the three navigation surfaces.
    let route_ids: std::collections::BTreeSet<&str> = view
        .nodes
        .iter()
        .filter(|n| n.kind == "route")
        .map(|n| n.id.as_str())
        .collect();
    for expected in [
        "route::/paywall_thanks",
        "route::/editor",
        "route::/projects",
    ] {
        assert!(
            route_ids.contains(expected),
            "missing route node {expected}; got {route_ids:?}"
        );
    }

    // (c) Persists-to edges originate from each feature's mutator,
    //     including code where `Hive.openBox` was stored in a local
    //     variable (editor + project_lifecycle).
    let persists_from: std::collections::BTreeSet<String> = view
        .edges
        .iter()
        .filter(|e| e.kind == "persists_to")
        .map(|e| e.from.clone())
        .collect();
    let must_persist_from = [
        // editor — uses `final box = await Hive.openBox(...)`.
        "dart_method::lib/features/editor/editor_controller.dart#EditorController.applyTool",
        // layers — direct `Hive.openBox` in helper.
        "dart_method::lib/features/layers/layer_repository.dart#LayerRepository._persist",
        // paywall — direct `Hive.box(...).put(...)` chain.
        "dart_method::lib/features/paywall/paywall_screen.dart#PaywallScreen.listenToPurchaseUpdates",
        // project lifecycle — multiple methods each persist.
        "dart_method::lib/features/project/project_lifecycle.dart#ProjectLifecycle.createProject",
        "dart_method::lib/features/project/project_lifecycle.dart#ProjectLifecycle.saveProject",
        "dart_method::lib/features/project/project_lifecycle.dart#ProjectLifecycle.closeProject",
    ];
    for site in must_persist_from {
        assert!(
            persists_from.contains(site),
            "expected persists_to from {site}; got {persists_from:?}"
        );
    }

    // (d) Streams: subscribed in two places — paywall purchases AND
    //     project autosave.
    let subscribes_count = view
        .edges
        .iter()
        .filter(|e| e.kind == "subscribes_stream")
        .count();
    assert!(
        subscribes_count >= 2,
        "expected at least 2 subscribes_stream edges (paywall + project autosave), got {subscribes_count}"
    );

    // (e) Test nodes show up — at least one per feature.
    let test_paths: std::collections::BTreeSet<String> = view
        .nodes
        .iter()
        .filter(|n| n.kind == "test_case")
        .map(|n| n.id.clone())
        .collect();
    for expected_substr in [
        "test/iap/iap_constants_test",
        "test/editor/editor_controller_test",
        "test/layers/layer_repository_test",
        "test/project/project_lifecycle_test",
    ] {
        assert!(
            test_paths.iter().any(|p| p.contains(expected_substr)),
            "expected test node containing {expected_substr}; got {test_paths:?}"
        );
    }
}

#[test]
fn p4_candidate_review_cycle_persists_to_yaml_and_dedupes_questions() {
    // P1 review loop must round-trip through the fixture's YAML and
    // hold its shape across reads. Doesn't need the sidecar — operates
    // purely on the YAML — but we ship it next to the golden tests so
    // the regression coverage is grouped by deliverable.
    let tmp = tempfile::TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    copy_fixture_into(tmp.path());

    // Snapshot: every candidate starts un-reviewed.
    let initial = list_for_review(tmp.path()).unwrap();
    assert!(
        !initial.needs_review.is_empty(),
        "fresh fixture must have candidates needing review"
    );
    assert!(
        initial.already_reviewed.is_empty(),
        "fresh fixture must not have any reviewed candidates"
    );

    // 1) Accept one candidate, answer one of its open questions.
    let accept_id = "complete_purchase_unlocks_pro";
    let accepted_question =
        "Is the purchase receipt validated server-side before applyPurchase flips state to true?";
    apply_review(
        tmp.path(),
        accept_id,
        ReviewVerdict {
            status: ReviewStatus::Accepted,
            reviewer: Some("alice".into()),
            note: Some("Confirmed via call with payments team.".into()),
            answered_questions: vec![accepted_question.into()],
            reviewed_at: Some("2026-05-19T00:00:00Z".into()),
        },
    )
    .unwrap();

    // 2) Reject one candidate (the restore-purchases-is-incomplete one).
    apply_review(
        tmp.path(),
        "restore_purchases_is_incomplete",
        ReviewVerdict {
            status: ReviewStatus::Rejected,
            reviewer: Some("alice".into()),
            note: Some("Out of scope for this milestone.".into()),
            answered_questions: vec![],
            reviewed_at: Some("2026-05-19T00:00:00Z".into()),
        },
    )
    .unwrap();

    // 3) Mark one candidate as needs_changes (editor persists last tool).
    apply_review(
        tmp.path(),
        "editor_persists_last_tool_to_hive",
        ReviewVerdict {
            status: ReviewStatus::NeedsChanges,
            reviewer: Some("alice".into()),
            note: Some("Need to confirm migration story for old EditorTool values.".into()),
            answered_questions: vec![],
            reviewed_at: Some("2026-05-19T00:00:00Z".into()),
        },
    )
    .unwrap();

    // Re-load and check the audit trail survived a round-trip.
    let outcome = load_business_candidates(tmp.path()).unwrap();
    let by_id: std::collections::HashMap<&str, _> = outcome
        .document
        .candidates
        .iter()
        .map(|c| (c.id.as_str(), c))
        .collect();
    let accepted = by_id
        .get(accept_id)
        .expect("candidate missing after review");
    assert_eq!(accepted.review_status(), Some(ReviewStatus::Accepted));
    assert_eq!(
        accepted.review.as_ref().unwrap().reviewer.as_deref(),
        Some("alice")
    );
    // The answered question is filtered out of pending_open_questions
    // so the CLI doesn't re-prompt the human.
    let pending: Vec<&str> = accepted.pending_open_questions();
    assert!(
        !pending.contains(&accepted_question),
        "answered question should be filtered from pending list, got {pending:?}"
    );
    assert!(
        !pending.is_empty(),
        "other open questions should still surface; got {pending:?}"
    );

    let rejected = by_id
        .get("restore_purchases_is_incomplete")
        .expect("rejected candidate missing");
    assert_eq!(rejected.review_status(), Some(ReviewStatus::Rejected));

    let needs_changes = by_id
        .get("editor_persists_last_tool_to_hive")
        .expect("needs_changes candidate missing");
    assert_eq!(
        needs_changes.review_status(),
        Some(ReviewStatus::NeedsChanges)
    );

    // After the three verdicts, list_for_review must reflect the split.
    // NOTE: `list_for_review` treats only `accepted` and `rejected` as
    // "already_reviewed" — `needs_changes` still requires a human pass,
    // so it stays in the `needs_review` bucket on purpose.
    let after = list_for_review(tmp.path()).unwrap();
    assert_eq!(
        after.already_reviewed.len(),
        2,
        "accepted + rejected count toward already_reviewed; needs_changes stays in needs_review"
    );
    let reviewed_ids: std::collections::BTreeSet<&str> = after
        .already_reviewed
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert!(reviewed_ids.contains("complete_purchase_unlocks_pro"));
    assert!(reviewed_ids.contains("restore_purchases_is_incomplete"));

    let pending_ids: std::collections::BTreeSet<&str> =
        after.needs_review.iter().map(|c| c.id.as_str()).collect();
    assert!(!pending_ids.contains("complete_purchase_unlocks_pro"));
    assert!(!pending_ids.contains("restore_purchases_is_incomplete"));
    // needs_changes-tagged candidate is still in the needs_review list
    // so the reviewer can take another pass.
    assert!(pending_ids.contains("editor_persists_last_tool_to_hive"));
}

#[test]
fn p4_logic_confidence_report_reflects_review_outcomes() {
    let Some((tmp, _on, _bin)) = setup_indexed_repo() else {
        return;
    };

    // Apply the same three verdicts so the confidence report has
    // every status to render.
    apply_review(
        tmp.path(),
        "complete_purchase_unlocks_pro",
        ReviewVerdict {
            status: ReviewStatus::Accepted,
            reviewer: Some("alice".into()),
            note: None,
            answered_questions: vec![],
            reviewed_at: Some("2026-05-19T00:00:00Z".into()),
        },
    )
    .unwrap();
    apply_review(
        tmp.path(),
        "restore_purchases_is_incomplete",
        ReviewVerdict {
            status: ReviewStatus::Rejected,
            reviewer: Some("alice".into()),
            note: None,
            answered_questions: vec![],
            reviewed_at: Some("2026-05-19T00:00:00Z".into()),
        },
    )
    .unwrap();
    apply_review(
        tmp.path(),
        "editor_persists_last_tool_to_hive",
        ReviewVerdict {
            status: ReviewStatus::NeedsChanges,
            reviewer: Some("alice".into()),
            note: None,
            answered_questions: vec![],
            reviewed_at: Some("2026-05-19T00:00:00Z".into()),
        },
    )
    .unwrap();

    let store = specslice_store::Store::open(tmp.path().join(".specslice/graph.db")).unwrap();
    let report = compute_logic_confidence(&store, tmp.path()).unwrap();

    // Sanity: every item is a BusinessCandidate (no Requirements in
    // this fixture) and we have one item per YAML candidate.
    let candidates_in_report: Vec<&str> = report
        .items
        .iter()
        .filter(|i| i.kind == LogicConfidenceSource::BusinessCandidate)
        .map(|i| i.id.as_str())
        .collect();
    let outcome = load_business_candidates(tmp.path()).unwrap();
    assert_eq!(
        candidates_in_report.len(),
        outcome.document.candidates.len(),
        "every YAML candidate must appear in the confidence report; report ids = {candidates_in_report:?}"
    );

    let by_id: std::collections::HashMap<String, &specslice_engine::LogicConfidenceItem> =
        report.items.iter().map(|i| (i.id.clone(), i)).collect();

    // Accepted with linked evidence -> confirmed_link.
    let accepted_item = by_id
        .get(&candidate_artifact_id("complete_purchase_unlocks_pro"))
        .expect("accepted candidate item missing");
    assert_eq!(accepted_item.verdict, LogicConfidenceKind::ConfirmedLink);

    // Rejected -> rejected verdict (regardless of evidence).
    let rejected_item = by_id
        .get(&candidate_artifact_id("restore_purchases_is_incomplete"))
        .expect("rejected candidate item missing");
    assert_eq!(rejected_item.verdict, LogicConfidenceKind::Rejected);

    // NeedsChanges -> needs_changes verdict.
    let needs_changes_item = by_id
        .get(&candidate_artifact_id("editor_persists_last_tool_to_hive"))
        .expect("needs-changes candidate item missing");
    assert_eq!(
        needs_changes_item.verdict,
        LogicConfidenceKind::NeedsChanges
    );

    // Un-reviewed candidates fall through to `candidate_only`.
    let untouched_item = by_id
        .get(&candidate_artifact_id("paywall_thanks_route_is_unverified"))
        .expect("untouched candidate item missing");
    assert_eq!(
        untouched_item.verdict,
        LogicConfidenceKind::CandidateOnly,
        "unreviewed candidates must land on `candidate_only`"
    );

    // The summary cross-foots: numbers must add up to the total
    // number of items.
    let summary_total = report.summary.confirmed_link
        + report.summary.stale_link
        + report.summary.needs_changes
        + report.summary.missing_test
        + report.summary.missing_doc
        + report.summary.missing_link
        + report.summary.candidate_only
        + report.summary.rejected
        + report.summary.unknown;
    assert_eq!(
        summary_total,
        report.items.len(),
        "logic confidence summary buckets must cover every item"
    );
    assert!(report.summary.confirmed_link >= 1);
    assert!(report.summary.rejected >= 1);
    assert!(report.summary.needs_changes >= 1);
    assert!(report.summary.candidate_only >= 1);
}
