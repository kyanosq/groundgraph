//! P6.2: code-fact graph regression tests modelled after the pixcraft IAP
//! flow that the user could not reproduce with the previous adapter.
//!
//! The user's complaint, verbatim:
//!
//! - `--focus lib/core/iap` returned a single module node with `edges: []`.
//! - `--focus dart_method::…#ProNotifier.applyPurchase` returned only the
//!   parent `contains` edge with no `calls` / `references` chain to
//!   `IapProductIds` or the surrounding paywall.
//!
//! These tests pin down the minimum behaviour the engine has to ship before
//! we can claim the graph is a real "code-fact graph" rather than a tree
//! drawing of the filesystem.

use std::collections::BTreeSet;
use std::path::Path;

use specslice_engine::graph::{build_graph_view, GraphOptions, GraphView, GraphViewModel};
use specslice_engine::index::{index_repository, IndexOptions};
use specslice_engine::init::{init_repository, InitOptions};
use tempfile::TempDir;

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Minimal stand-in for the pixcraft in-app purchase flow:
///
/// - `lib/core/iap/iap_constants.dart` declares `IapProductIds`.
/// - `lib/core/settings/pro_provider.dart` declares `ProNotifier` whose
///   `applyPurchase` body references `IapProductIds`.
/// - `lib/features/paywall/paywall_screen.dart` declares `PaywallScreen`
///   whose methods both call `applyPurchase` and reference `IapProductIds`.
fn pixcraft_iap_workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    write(
        &tmp.path().join("lib/core/iap/iap_constants.dart"),
        "class IapProductIds {\n  static const String monthly = 'pro_monthly';\n  static const String yearly = 'pro_yearly';\n  static const String lifetime = 'pro_lifetime';\n  static const List<String> all = <String>[monthly, yearly, lifetime];\n}\n",
    );
    write(
        &tmp.path().join("lib/core/settings/pro_provider.dart"),
        "import '../iap/iap_constants.dart';\n\nclass ProNotifier {\n  bool state = false;\n\n  Future<void> applyPurchase(String productId) async {\n    if (IapProductIds.all.contains(productId)) {\n      state = true;\n    }\n  }\n}\n",
    );
    write(
        &tmp.path()
            .join("lib/features/paywall/paywall_screen.dart"),
        "import '../../core/iap/iap_constants.dart';\nimport '../../core/settings/pro_provider.dart';\n\nclass PaywallScreen {\n  void initStore() {\n    final ids = IapProductIds.all;\n  }\n\n  void listenToPurchaseUpdates(ProNotifier notifier, String purchase) {\n    notifier.applyPurchase(purchase);\n    if (IapProductIds.monthly == purchase) {\n      return;\n    }\n  }\n}\n",
    );
    index_repository(IndexOptions::all(tmp.path())).unwrap();
    tmp
}

fn node_ids(view: &GraphViewModel) -> BTreeSet<String> {
    view.nodes.iter().map(|n| n.id.clone()).collect()
}

fn has_edge(view: &GraphViewModel, kind: &str, from: &str, to: &str) -> bool {
    view.edges
        .iter()
        .any(|e| e.kind == kind && e.from == from && e.to == to)
}

#[test]
fn focus_on_module_expands_to_file_and_symbols() {
    let tmp = pixcraft_iap_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Focus,
            focus: Some("lib/core/iap".into()),
            ..Default::default()
        },
    )
    .unwrap();

    let ids = node_ids(&view);
    assert!(
        ids.contains("module::lib/core/iap"),
        "focus module missing: {ids:?}"
    );
    assert!(
        ids.contains("file::lib/core/iap/iap_constants.dart"),
        "focus should expand to the file inside the module: {ids:?}"
    );
    assert!(
        ids.contains("dart_class::lib/core/iap/iap_constants.dart#IapProductIds"),
        "focus should expand to the class inside the module: {ids:?}"
    );
}

#[test]
fn focus_on_file_expands_to_classes_and_methods() {
    let tmp = pixcraft_iap_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Focus,
            focus: Some("file::lib/core/settings/pro_provider.dart".into()),
            ..Default::default()
        },
    )
    .unwrap();

    let ids = node_ids(&view);
    assert!(ids.contains("file::lib/core/settings/pro_provider.dart"));
    assert!(
        ids.contains("dart_class::lib/core/settings/pro_provider.dart#ProNotifier"),
        "{ids:?}"
    );
    assert!(
        ids.contains("dart_method::lib/core/settings/pro_provider.dart#ProNotifier.applyPurchase"),
        "{ids:?}"
    );
}

#[test]
fn dart_method_body_emits_calls_edge_to_referenced_method() {
    let tmp = pixcraft_iap_workspace();
    let view = build_graph_view(tmp.path(), GraphOptions::default()).unwrap();

    assert!(
        has_edge(
            &view,
            "calls",
            "dart_method::lib/features/paywall/paywall_screen.dart#PaywallScreen.listenToPurchaseUpdates",
            "dart_method::lib/core/settings/pro_provider.dart#ProNotifier.applyPurchase",
        ),
        "missing PaywallScreen.listenToPurchaseUpdates -> ProNotifier.applyPurchase calls edge in: {:?}",
        view.edges.iter().map(|e| (e.kind.clone(), e.from.clone(), e.to.clone())).collect::<Vec<_>>(),
    );
}

#[test]
fn dart_method_body_emits_references_edge_to_referenced_class() {
    let tmp = pixcraft_iap_workspace();
    let view = build_graph_view(tmp.path(), GraphOptions::default()).unwrap();

    assert!(
        has_edge(
            &view,
            "references",
            "dart_method::lib/features/paywall/paywall_screen.dart#PaywallScreen.listenToPurchaseUpdates",
            "dart_class::lib/core/iap/iap_constants.dart#IapProductIds",
        ),
        "missing listenToPurchaseUpdates -> IapProductIds references edge",
    );
    assert!(
        has_edge(
            &view,
            "references",
            "dart_method::lib/core/settings/pro_provider.dart#ProNotifier.applyPurchase",
            "dart_class::lib/core/iap/iap_constants.dart#IapProductIds",
        ),
        "missing ProNotifier.applyPurchase -> IapProductIds references edge",
    );
}

#[test]
fn focus_on_apply_purchase_shows_caller_and_referenced_class() {
    let tmp = pixcraft_iap_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Focus,
            focus: Some(
                "dart_method::lib/core/settings/pro_provider.dart#ProNotifier.applyPurchase".into(),
            ),
            ..Default::default()
        },
    )
    .unwrap();

    let ids = node_ids(&view);
    assert!(
        ids.contains("dart_method::lib/core/settings/pro_provider.dart#ProNotifier.applyPurchase"),
        "focus node itself missing: {ids:?}"
    );
    assert!(
        ids.contains("dart_method::lib/features/paywall/paywall_screen.dart#PaywallScreen.listenToPurchaseUpdates"),
        "focus must surface the caller via incoming calls edge: {ids:?}",
    );
    assert!(
        ids.contains("dart_class::lib/core/iap/iap_constants.dart#IapProductIds"),
        "focus must surface the referenced class via outgoing references edge: {ids:?}",
    );
}

#[test]
fn body_reference_does_not_emit_self_loop() {
    let tmp = pixcraft_iap_workspace();
    let view = build_graph_view(tmp.path(), GraphOptions::default()).unwrap();

    for edge in &view.edges {
        if edge.kind == "calls" || edge.kind == "references" {
            assert_ne!(
                edge.from, edge.to,
                "method body must not produce a self-referential {edge:?}"
            );
        }
    }
}

#[test]
fn references_and_calls_edges_are_fact_layer() {
    let tmp = pixcraft_iap_workspace();
    let view = build_graph_view(tmp.path(), GraphOptions::default()).unwrap();

    for edge in &view.edges {
        if edge.kind == "calls" || edge.kind == "references" {
            assert_eq!(
                edge.layer,
                specslice_engine::graph::GraphLayer::Fact,
                "calls/references edges are deterministic code facts, not confirmed business: {edge:?}",
            );
        }
    }
}
