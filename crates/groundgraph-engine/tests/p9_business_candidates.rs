//! P9 acceptance: AI-authored business candidates loaded from
//! `.groundgraph/candidates/business_logic.yaml` appear in the graph
//! view as `business_candidate` nodes on the Candidate layer, with
//! `derives_from` edges to every cited code-fact node.
//!
//! Like the P8 tests, this is only meaningful end-to-end with the Dart
//! analyzer sidecar active, because the cited evidence (dart_provider
//! / route / storage nodes) comes from P8. We gate the test the same
//! way: skip when `dart` is unavailable or the sidecar source is
//! missing.

mod common;

use groundgraph_engine::business_candidates::load_business_candidates;
use groundgraph_engine::graph::{build_graph_view, GraphOptions, GraphView};

#[test]
fn p9_fixture_yaml_is_readable_standalone() {
    // The candidates loader must work on the fixture even without the
    // engine — this protects us from silent regressions to the parser
    // (e.g. typo'd keys, broken serde tags).
    let outcome = load_business_candidates(&common::fixture_dir("pixcraft_iap")).unwrap();
    assert!(
        outcome.warnings.is_empty(),
        "fixture candidate file must parse cleanly, warnings = {:?}",
        outcome.warnings
    );
    assert!(
        outcome.document.candidates.len() >= 4,
        "fixture must seed at least 4 business candidates (purchase / restore / expiry / lifecycle), got {}",
        outcome.document.candidates.len()
    );
    let ids: Vec<&str> = outcome
        .document
        .candidates
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert!(ids.contains(&"complete_purchase_unlocks_pro"));
    assert!(ids.contains(&"restore_purchases_is_incomplete"));
    assert!(ids.contains(&"missing_subscription_expiry_check"));
    assert!(ids.contains(&"purchase_stream_listener_lifecycle"));
}

#[test]
fn p9_every_candidate_carries_evidence_and_open_questions() {
    let outcome = load_business_candidates(&common::fixture_dir("pixcraft_iap")).unwrap();
    for c in &outcome.document.candidates {
        assert!(
            !c.evidence.is_empty(),
            "candidate `{}` must cite at least one code-fact evidence id",
            c.id
        );
        assert!(
            c.confidence.is_some(),
            "candidate `{}` must declare a confidence so the UI can render it",
            c.id
        );
        let confidence = c.confidence.unwrap();
        assert!(
            (0.0..=1.0).contains(&confidence.get()),
            "candidate `{}` confidence {} must be in 0.0..=1.0",
            c.id,
            confidence
        );
        assert!(
            !c.open_questions.is_empty(),
            "candidate `{}` should expose at least one open question — AI candidates without unknowns smell overconfident",
            c.id
        );
        assert_eq!(
            c.status, "proposed",
            "fresh AI candidates must start as `proposed`; human must promote them"
        );
    }
}

#[test]
fn p9_candidates_surface_in_business_view_with_derives_from_edges() {
    let Some(tmp) =
        common::setup_indexed_dart_repo("p9_business_candidates", "pixcraft_iap", &["lib", "test"])
    else {
        return;
    };

    // Default options: candidates are included.
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Business,
            ..Default::default()
        },
    )
    .unwrap();

    // (a) candidate node exists for every YAML candidate.
    let mut candidate_node_count = 0usize;
    for cid in [
        "complete_purchase_unlocks_pro",
        "restore_purchases_is_incomplete",
        "missing_subscription_expiry_check",
        "purchase_stream_listener_lifecycle",
        "paywall_thanks_route_is_unverified",
    ] {
        let full = format!("business_candidate::{cid}");
        let node = view
            .nodes
            .iter()
            .find(|n| n.id == full)
            .unwrap_or_else(|| panic!("expected candidate node `{full}` in graph"));
        assert_eq!(node.kind, "business_candidate");
        assert_eq!(
            node.layer,
            groundgraph_engine::GraphLayer::Candidate,
            "candidate nodes must live on layer=candidate"
        );
        assert_eq!(
            node.column,
            groundgraph_engine::GraphColumn::Business,
            "candidate nodes belong in the business column"
        );
        assert!(
            node.confidence.is_some(),
            "candidate `{cid}` must surface its confidence"
        );
        candidate_node_count += 1;
    }
    assert_eq!(candidate_node_count, 5);
    assert!(
        !view.findings.iter().any(|f| f.code == "no_business_logic"),
        "business view with loaded candidates must not tell the user to seed candidates"
    );

    // (b) derives_from edges fan out from each candidate to the cited code facts.
    let derives = view
        .edges
        .iter()
        .filter(|e| e.kind == "derives_from")
        .count();
    assert!(
        derives >= 10,
        "expected many derives_from edges across all candidates, got {derives}"
    );
    // Spot check: the purchase candidate must cite the applyPurchase method,
    // proProvider, the Hive box, AND the route — multiple kinds of evidence.
    let purchase_evidence: std::collections::BTreeSet<&str> = view
        .edges
        .iter()
        .filter(|e| {
            e.kind == "derives_from"
                && e.from == "business_candidate::complete_purchase_unlocks_pro"
        })
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        purchase_evidence
            .iter()
            .any(|s| s.contains("ProNotifier.applyPurchase")),
        "complete_purchase candidate must cite applyPurchase, evidence={purchase_evidence:?}"
    );
    assert!(
        purchase_evidence.iter().any(|s| s.contains("proProvider")),
        "complete_purchase candidate must cite proProvider"
    );
    assert!(
        purchase_evidence
            .iter()
            .any(|s| s.starts_with("storage::hive::")),
        "complete_purchase candidate must cite the Hive storage node"
    );
    assert!(
        purchase_evidence.iter().any(|s| s.starts_with("route::")),
        "complete_purchase candidate must cite the synthetic route node"
    );
}

#[test]
fn p9_include_candidates_false_hides_business_candidates() {
    let Some(tmp) =
        common::setup_indexed_dart_repo("p9_business_candidates", "pixcraft_iap", &["lib", "test"])
    else {
        return;
    };

    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Business,
            include_candidates: false,
            ..Default::default()
        },
    )
    .unwrap();
    let any_candidate = view.nodes.iter().any(|n| n.kind == "business_candidate");
    assert!(
        !any_candidate,
        "include_candidates=false must hide every business_candidate node"
    );
    let any_derives = view.edges.iter().any(|e| e.kind == "derives_from");
    assert!(
        !any_derives,
        "include_candidates=false must drop every derives_from edge"
    );
}
