//! P8 acceptance: framework-aware semantic edges produced by the Dart
//! analyzer sidecar — `reads_provider`, `navigates_to`, `persists_to`,
//! `subscribes_stream`.
//!
//! The pixcraft_iap fixture exercises every one of those patterns inside
//! `PaywallScreen.listenToPurchaseUpdates`. The sidecar must:
//!
//! - emit a `reads_provider` edge to `proProvider` (a top-level Riverpod
//!   `StateNotifierProvider`),
//! - emit a `navigates_to` edge to `route::/paywall_thanks` (a synthetic
//!   Route node materialised from the `context.push("/paywall_thanks")`
//!   call),
//! - emit a `persists_to` edge to `storage::hive::pro_entitlement` (a
//!   synthetic Storage node materialised from `Hive.box('pro_entitlement')`),
//! - emit a `subscribes_stream` edge from the listener to the stream
//!   producer (`PurchaseStream.updates`).
//!
//! These tests are skipped unless a real `dart` binary is on PATH and the
//! sidecar source ships in the repo — same gating as the P7 happy-path
//! test so the suite stays portable.

use std::path::PathBuf;

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

/// Materialise the pixcraft_iap fixture into a fresh repo, run the
/// analyzer sidecar, and return the resulting graph view. We focus on
/// the paywall feature so the test stays scoped.
fn analyze_pixcraft_with_sidecar() -> Option<specslice_engine::graph::GraphViewModel> {
    if !sidecar_source_present() || !dart_available() {
        return None;
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
        "P8 tests require the analyzer sidecar to be the active resolver, skip_reason={}",
        result.sidecar_skip_reason
    );
    Some(
        build_graph_view(
            tmp.path(),
            GraphOptions {
                view: GraphView::Code,
                ..Default::default()
            },
        )
        .unwrap(),
    )
}

#[test]
fn p8_reads_provider_edge_points_at_pro_provider() {
    let Some(view) = analyze_pixcraft_with_sidecar() else {
        eprintln!("skipping: dart sidecar unavailable");
        return;
    };

    // Provider node exists.
    let provider_node = view
        .nodes
        .iter()
        .find(|n| n.kind == "dart_provider" && n.id.ends_with("#proProvider"))
        .expect("dart_provider node must be created by Pass 1");
    assert!(
        provider_node.id.starts_with("dart_provider::"),
        "provider id must use dart_provider:: prefix, got {}",
        provider_node.id
    );

    // reads_provider edge exists, from listenToPurchaseUpdates → proProvider.
    let edge = view
        .edges
        .iter()
        .find(|e| {
            e.kind == "reads_provider"
                && e.from.contains("PaywallScreen.listenToPurchaseUpdates")
                && e.to == provider_node.id
        })
        .expect("P8 must emit reads_provider edge to proProvider");
    assert_eq!(edge.resolver.as_deref(), Some("dart_analyzer"));
    assert!(
        edge.line_range.is_some(),
        "P6.3 evidence must travel with reads_provider edges"
    );
    let snippet = edge.snippet.as_deref().unwrap_or("");
    assert!(
        snippet.contains("ref.read") && snippet.contains("proProvider"),
        "snippet must capture the actual ref.read(proProvider) call site, got {snippet:?}"
    );
}

#[test]
fn p8_navigates_to_edge_uses_synthetic_route_node() {
    let Some(view) = analyze_pixcraft_with_sidecar() else {
        eprintln!("skipping: dart sidecar unavailable");
        return;
    };

    let route_node = view
        .nodes
        .iter()
        .find(|n| n.kind == "route" && n.id == "route::/paywall_thanks")
        .expect("synthetic route node must materialise for context.push(...)");
    assert_eq!(
        route_node.label, "/paywall_thanks",
        "Route label must equal the literal path"
    );

    let edge = view
        .edges
        .iter()
        .find(|e| {
            e.kind == "navigates_to"
                && e.from.contains("PaywallScreen.listenToPurchaseUpdates")
                && e.to == "route::/paywall_thanks"
        })
        .expect("P8 must emit navigates_to edge to the synthetic Route");
    assert_eq!(edge.resolver.as_deref(), Some("dart_analyzer"));
    let snippet = edge.snippet.as_deref().unwrap_or("");
    assert!(
        snippet.contains("context.push") && snippet.contains("/paywall_thanks"),
        "navigates_to snippet must capture the push call, got {snippet:?}"
    );
}

#[test]
fn p8_persists_to_edge_uses_synthetic_storage_node() {
    let Some(view) = analyze_pixcraft_with_sidecar() else {
        eprintln!("skipping: dart sidecar unavailable");
        return;
    };

    let storage_node = view
        .nodes
        .iter()
        .find(|n| n.kind == "storage" && n.id == "storage::hive::pro_entitlement")
        .expect("synthetic Storage node must materialise for Hive.box('pro_entitlement').put");
    assert!(
        storage_node.label.starts_with("hive:"),
        "Storage label must declare the hive backend, got {}",
        storage_node.label
    );

    let edge = view
        .edges
        .iter()
        .find(|e| {
            e.kind == "persists_to"
                && e.from.contains("PaywallScreen.listenToPurchaseUpdates")
                && e.to == "storage::hive::pro_entitlement"
        })
        .expect("P8 must emit persists_to edge for the Hive.put call");
    assert_eq!(edge.resolver.as_deref(), Some("dart_analyzer"));
    let snippet = edge.snippet.as_deref().unwrap_or("");
    assert!(
        snippet.contains("Hive.box") && snippet.contains("pro_entitlement"),
        "persists_to snippet must capture the Hive call, got {snippet:?}"
    );
}

#[test]
fn p8_subscribes_stream_edge_exists() {
    let Some(view) = analyze_pixcraft_with_sidecar() else {
        eprintln!("skipping: dart sidecar unavailable");
        return;
    };

    let edge = view
        .edges
        .iter()
        .find(|e| {
            e.kind == "subscribes_stream"
                && e.from.contains("PaywallScreen.listenToPurchaseUpdates")
        })
        .expect("P8 must emit subscribes_stream from the listener method");
    assert_eq!(edge.resolver.as_deref(), Some("dart_analyzer"));
    let snippet = edge.snippet.as_deref().unwrap_or("");
    assert!(
        snippet.contains(".listen("),
        "subscribes_stream snippet must capture the .listen(...) site, got {snippet:?}"
    );
}

#[test]
fn p8_semantic_edges_displace_generic_calls_for_the_same_site() {
    let Some(view) = analyze_pixcraft_with_sidecar() else {
        eprintln!("skipping: dart sidecar unavailable");
        return;
    };

    // The `Hive.box('pro_entitlement').put(...)` and the
    // `context.push('/paywall_thanks')` calls happen inside
    // listenToPurchaseUpdates — when P8 produces a semantic edge for them
    // we must NOT also produce a generic `calls` edge to `put` / `push`,
    // otherwise the noise filter cannot tell them apart and the focus
    // view duplicates them.
    let push_calls = view
        .edges
        .iter()
        .filter(|e| {
            e.kind == "calls"
                && e.from.contains("PaywallScreen.listenToPurchaseUpdates")
                && e.to.ends_with("#push")
        })
        .count();
    assert_eq!(
        push_calls, 0,
        "context.push must not duplicate as `calls` when navigates_to is emitted"
    );
    let put_calls = view
        .edges
        .iter()
        .filter(|e| {
            e.kind == "calls"
                && e.from.contains("PaywallScreen.listenToPurchaseUpdates")
                && e.to.ends_with("#put")
        })
        .count();
    assert_eq!(
        put_calls, 0,
        "Hive.put must not duplicate as `calls` when persists_to is emitted"
    );
}
