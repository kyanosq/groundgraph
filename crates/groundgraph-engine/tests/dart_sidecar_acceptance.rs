//! P7 acceptance: when the analyzer sidecar is reachable, the engine
//! prefers it and stamps every `calls` / `references` edge with
//! `resolver = "dart_analyzer"`. When the sidecar is unavailable, the
//! engine must transparently fall back to the heuristic adapter.
//!
//! The "sidecar reachable" test only runs when:
//! - the workspace ships `tool/groundgraph_dart_analyzer/`
//! - the host has a working `dart` binary on `PATH`
//! - `GROUNDGRAPH_DART_ANALYZER` opts the sidecar in
//!
//! Otherwise we still verify the fallback contract (resolver_used =
//! "dart_lightweight", no diagnostics, graph still populated).

mod common;

use groundgraph_engine::dart_indexer::{
    index_dart, DartIndexOptions, RESOLVER_DART_ANALYZER, RESOLVER_DART_LIGHTWEIGHT,
};
use groundgraph_engine::graph::{build_graph_view, GraphOptions, GraphView};
use groundgraph_engine::init::{init_repository, InitOptions};

#[test]
fn p7_fallback_path_still_indexes_when_sidecar_disabled() {
    // Make sure the env explicitly disables the sidecar so this test is
    // deterministic regardless of host configuration.
    let _serial = common::env_lock();
    let _guard = common::EnvGuard::set("GROUNDGRAPH_DART_ANALYZER", Some("0"));
    let tmp = tempfile::TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    common::copy_fixture_into(&common::fixture_dir("pixcraft_iap"), tmp.path());
    let mut store =
        groundgraph_store::Store::open(tmp.path().join(".groundgraph/graph.db")).unwrap();
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
        result.resolver_used, RESOLVER_DART_LIGHTWEIGHT,
        "fallback path must report dart_lightweight"
    );
    assert!(
        result.symbols > 0,
        "heuristic adapter must still produce symbols"
    );

    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Code,
            focus: Some("module::lib/core/iap".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let resolvers: std::collections::BTreeSet<String> = view
        .edges
        .iter()
        .filter(|e| e.kind == "calls" || e.kind == "references")
        .filter_map(|e| e.resolver.clone())
        .collect();
    assert!(
        resolvers == ["dart_lightweight".to_string()].into_iter().collect(),
        "fallback edges must all be resolver=dart_lightweight, got {resolvers:?}"
    );
}

#[test]
fn p7_sidecar_runs_when_enabled_and_tags_edges_dart_analyzer() {
    if !common::dart_golden_ready(
        common::sidecar_source_present() && common::dart_available(),
        "p7_sidecar_acceptance",
    ) {
        return;
    }
    let _serial = common::env_lock();
    let _guard = common::EnvGuard::set("GROUNDGRAPH_DART_ANALYZER", Some("1"));

    let tmp = tempfile::TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    common::copy_fixture_into(&common::fixture_dir("pixcraft_iap"), tmp.path());
    // Point the sidecar bin at the in-repo file so we don't need a
    // global install.
    let _bin_guard = common::EnvGuard::set(
        "GROUNDGRAPH_DART_ANALYZER_BIN",
        Some(&format!("dart run {}", common::sidecar_path().display())),
    );

    let mut store =
        groundgraph_store::Store::open(tmp.path().join(".groundgraph/graph.db")).unwrap();
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
        "sidecar happy path must report dart_analyzer, skip_reason={}",
        result.sidecar_skip_reason
    );
    assert!(
        result.symbols >= 5,
        "sidecar must surface fixture symbols, got {}",
        result.symbols
    );
    assert!(
        result.tests >= 1,
        "sidecar must surface fixture tests, got {}",
        result.tests
    );

    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Code,
            focus: Some("module::lib/core/iap".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let resolvers: std::collections::BTreeSet<String> = view
        .edges
        .iter()
        .filter(|e| e.kind == "calls" || e.kind == "references")
        .filter_map(|e| e.resolver.clone())
        .collect();
    assert!(
        view.stats.tests >= 1,
        "sidecar graph view must retain test nodes, got stats={:?}",
        view.stats
    );
    let iap_class = view
        .nodes
        .iter()
        .find(|n| n.id == "dart_class::lib/core/iap/iap_constants.dart#IapProductIds")
        .expect("sidecar graph view should contain IapProductIds class");
    assert_eq!(
        iap_class.source.as_deref(),
        Some("dart_analyzer"),
        "code nodes emitted by the analyzer sidecar should surface source=dart_analyzer"
    );
    assert!(
        resolvers.contains("dart_analyzer"),
        "expected at least one resolver=dart_analyzer edge, got {resolvers:?}"
    );
    let listener_to_apply = view.edges.iter().find(|e| {
        e.kind == "calls"
            && e.from.contains("PaywallScreen.listenToPurchaseUpdates")
            && e.to.contains("ProNotifier.applyPurchase")
    });
    let edge = listener_to_apply
        .expect("sidecar must emit listenToPurchaseUpdates → applyPurchase calls edge");
    assert_eq!(
        edge.resolver.as_deref(),
        Some("dart_analyzer"),
        "key business edge must come from the analyzer sidecar"
    );
    assert!(
        edge.snippet
            .as_deref()
            .unwrap_or("")
            .contains("applyPurchase"),
        "snippet must capture the call site"
    );
}
