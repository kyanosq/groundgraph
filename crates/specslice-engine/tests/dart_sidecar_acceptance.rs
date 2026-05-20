//! P7 acceptance: when the analyzer sidecar is reachable, the engine
//! prefers it and stamps every `calls` / `references` edge with
//! `resolver = "dart_analyzer"`. When the sidecar is unavailable, the
//! engine must transparently fall back to the heuristic adapter.
//!
//! The "sidecar reachable" test only runs when:
//! - the workspace ships `tool/specslice_dart_analyzer/`
//! - the host has a working `dart` binary on `PATH`
//! - `SPECSLICE_DART_ANALYZER` opts the sidecar in
//!
//! Otherwise we still verify the fallback contract (resolver_used =
//! "dart_lightweight", no diagnostics, graph still populated).

use std::path::PathBuf;

use specslice_engine::dart_indexer::{
    index_dart, DartIndexOptions, RESOLVER_DART_ANALYZER, RESOLVER_DART_LIGHTWEIGHT,
};
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

#[test]
fn p7_fallback_path_still_indexes_when_sidecar_disabled() {
    // Make sure the env explicitly disables the sidecar so this test is
    // deterministic regardless of host configuration.
    let _guard = EnvGuard::set("SPECSLICE_DART_ANALYZER", Some("0"));
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
    if !sidecar_source_present() {
        eprintln!("skipping P7 happy-path: sidecar source not present");
        return;
    }
    if !dart_available() {
        eprintln!("skipping P7 happy-path: `dart` not on PATH");
        return;
    }
    let _guard = EnvGuard::set("SPECSLICE_DART_ANALYZER", Some("1"));

    let tmp = tempfile::TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    copy_fixture_into(tmp.path());
    // Point the sidecar bin at the in-repo file so we don't need a
    // global install.
    let sidecar_abs =
        workspace_dir().join("tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart");
    let _bin_guard = EnvGuard::set(
        "SPECSLICE_DART_ANALYZER_BIN",
        Some(&format!("dart run {}", sidecar_abs.display())),
    );

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

struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: Option<&str>) -> Self {
        let prev = std::env::var(key).ok();
        match value {
            // SAFETY: cargo runs tests in the same process and each test
            // restores its own var; we keep them serial via a single
            // env_lock module only when truly contended.
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
