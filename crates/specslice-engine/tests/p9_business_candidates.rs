//! P9 acceptance: AI-authored business candidates loaded from
//! `.specslice/candidates/business_logic.yaml` appear in the graph
//! view as `business_candidate` nodes on the Candidate layer, with
//! `derives_from` edges to every cited code-fact node.
//!
//! Like the P8 tests, this is only meaningful end-to-end with the Dart
//! analyzer sidecar active, because the cited evidence (dart_provider
//! / route / storage nodes) comes from P8. We gate the test the same
//! way: skip when `dart` is unavailable or the sidecar source is
//! missing.

use std::path::PathBuf;

use specslice_engine::business_candidates::load_business_candidates;
use specslice_engine::dart_indexer::{index_dart, DartIndexOptions, RESOLVER_DART_ANALYZER};
use specslice_engine::graph::{build_graph_view, GraphOptions, GraphView};
use specslice_engine::init::{init_repository, InitOptions};

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

#[test]
fn p9_fixture_yaml_is_readable_standalone() {
    // The candidates loader must work on the fixture even without the
    // engine — this protects us from silent regressions to the parser
    // (e.g. typo'd keys, broken serde tags).
    let outcome = load_business_candidates(&fixture_path()).unwrap();
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
    let outcome = load_business_candidates(&fixture_path()).unwrap();
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
            (0.0..=1.0).contains(&confidence),
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
    if !sidecar_source_present() || !dart_available() {
        eprintln!("skipping: dart sidecar unavailable");
        return;
    }
    let _on = EnvGuard::set("SPECSLICE_DART_ANALYZER", Some("1"));
    let sidecar_abs =
        workspace_dir().join("tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart");
    let _bin = EnvGuard::set(
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
            disable_analyzer: false,
        },
    )
    .unwrap();
    assert_eq!(
        result.resolver_used, RESOLVER_DART_ANALYZER,
        "P9 graph test requires the analyzer sidecar"
    );

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
            specslice_engine::GraphLayer::Candidate,
            "candidate nodes must live on layer=candidate"
        );
        assert_eq!(
            node.column,
            specslice_engine::GraphColumn::Business,
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
    if !sidecar_source_present() || !dart_available() {
        eprintln!("skipping: dart sidecar unavailable");
        return;
    }
    let _on = EnvGuard::set("SPECSLICE_DART_ANALYZER", Some("1"));
    let sidecar_abs =
        workspace_dir().join("tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart");
    let _bin = EnvGuard::set(
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
    index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: tmp.path().into(),
            code_roots: vec!["lib".into(), "test".into()],
            exclude_globs: vec![],
            disable_analyzer: false,
        },
    )
    .unwrap();

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
