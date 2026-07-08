//! P6.3: evidence on edges + framework-noise reduction + business priority.
//!
//! These tests are the acceptance gate for the user's requirement:
//!
//! > 每条关键边必须有：source file / line range / snippet / confidence /
//! > resolver = analyzer / heuristic / ai_candidate.
//! > 噪声要求：focus 图默认不展示无关 toString/copyWith/l10n generated 边。
//!
//! Each `#[test]` pins one slice of that contract.

use std::path::Path;

use groundgraph_engine::graph::{build_graph_view, GraphOptions, GraphView, GraphViewModel};
use groundgraph_engine::index::{index_repository, IndexOptions};
use groundgraph_engine::init::{init_repository, InitOptions};
use tempfile::TempDir;

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Pixcraft-like fixture that exercises:
/// - real call chain: Paywall.listenToPurchaseUpdates -> ProNotifier.applyPurchase -> IapProductIds
/// - framework noise: toString, copyWith, dispose, initState calls inside paywall
/// - business naming: "Purchase", "Paywall", "Pro", "Iap" — all should rank high
fn workspace_with_evidence_and_noise() -> TempDir {
    let tmp = TempDir::new().unwrap();
    write(
        &tmp.path().join("lib/core/iap/iap_constants.dart"),
        "class IapProductIds {\n  static const String monthly = 'pro_monthly';\n  static const String yearly = 'pro_yearly';\n  static const List<String> all = <String>[monthly, yearly];\n}\n",
    );
    write(
        &tmp.path().join("lib/core/settings/pro_provider.dart"),
        "import '../iap/iap_constants.dart';\n\
         \n\
         class ProNotifier {\n  bool state = false;\n\n  Future<void> applyPurchase(String id) async {\n    if (IapProductIds.all.contains(id)) {\n      state = true;\n    }\n  }\n\n  @override\n  String toString() => 'ProNotifier';\n}\n",
    );
    write(
        &tmp.path()
            .join("lib/features/paywall/paywall_screen.dart"),
        "import '../../core/iap/iap_constants.dart';\n\
         import '../../core/settings/pro_provider.dart';\n\
         \n\
         class PaywallScreen {\n  ProNotifier notifier = ProNotifier();\n\n  void initState() {\n    notifier.toString();\n    notifier.toString();\n  }\n\n  void dispose() {}\n\n  void listenToPurchaseUpdates(String purchase) {\n    notifier.applyPurchase(purchase);\n    if (IapProductIds.monthly == purchase) {\n      return;\n    }\n  }\n}\n",
    );
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    // Pin the lightweight Dart body parser so this evidence-contract fixture is
    // hermetic: P6.3 is specifically about the *heuristic* adapter's edge
    // evidence, so it must exercise that path regardless of whether a Dart
    // analyzer sidecar happens to be installed on the host (with the sidecar
    // enabled the resolver would be `dart_analyzer`, not `dart_lightweight`).
    let cfg_path = tmp.path().join(".groundgraph.yaml");
    let cfg = std::fs::read_to_string(&cfg_path).unwrap();
    let cfg = cfg
        .replace("analyzer: true", "analyzer: false")
        .replace("lsp: true", "lsp: false");
    std::fs::write(&cfg_path, cfg).unwrap();
    index_repository(IndexOptions::all(tmp.path())).unwrap();
    tmp
}

fn view(repo_root: &Path) -> GraphViewModel {
    build_graph_view(
        repo_root,
        GraphOptions {
            view: GraphView::Code,
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn p63_every_calls_edge_carries_source_file_line_and_snippet() {
    let tmp = workspace_with_evidence_and_noise();
    let v = view(tmp.path());
    let body_edges: Vec<_> = v
        .edges
        .iter()
        .filter(|e| e.kind == "calls" || e.kind == "references")
        .collect();
    assert!(
        !body_edges.is_empty(),
        "fixture must produce at least one calls/references edge"
    );
    for e in &body_edges {
        assert!(
            e.source_file.is_some(),
            "{kind} {from}->{to} missing source_file",
            kind = e.kind,
            from = e.from,
            to = e.to
        );
        assert!(
            e.line_range.is_some(),
            "{kind} {from}->{to} missing line_range",
            kind = e.kind,
            from = e.from,
            to = e.to
        );
        assert!(
            e.snippet.is_some() && !e.snippet.as_deref().unwrap_or("").is_empty(),
            "{kind} {from}->{to} missing snippet",
            kind = e.kind,
            from = e.from,
            to = e.to
        );
        assert!(
            e.resolver.is_some(),
            "{kind} {from}->{to} missing resolver",
            kind = e.kind,
            from = e.from,
            to = e.to
        );
    }
}

#[test]
fn p63_resolver_is_dart_lightweight_for_heuristic_adapter() {
    let tmp = workspace_with_evidence_and_noise();
    let v = view(tmp.path());
    let resolvers: std::collections::BTreeSet<String> = v
        .edges
        .iter()
        .filter(|e| e.kind == "calls" || e.kind == "references")
        .filter_map(|e| e.resolver.clone())
        .collect();
    assert!(
        resolvers.contains("dart_lightweight"),
        "expected resolver=dart_lightweight on body-derived edges, got {resolvers:?}"
    );
}

#[test]
fn p63_apply_purchase_call_snippet_points_at_actual_invocation_line() {
    let tmp = workspace_with_evidence_and_noise();
    let v = view(tmp.path());
    let listener_to_apply: Vec<_> = v
        .edges
        .iter()
        .filter(|e| {
            e.kind == "calls"
                && e.from.contains("PaywallScreen.listenToPurchaseUpdates")
                && e.to.contains("ProNotifier.applyPurchase")
        })
        .collect();
    assert_eq!(
        listener_to_apply.len(),
        1,
        "expected one PaywallScreen.listenToPurchaseUpdates → ProNotifier.applyPurchase call"
    );
    let edge = listener_to_apply[0];
    let snippet = edge.snippet.as_deref().unwrap_or("");
    assert!(
        snippet.contains("applyPurchase"),
        "snippet must capture the calling line, got: {snippet:?}"
    );
    let (start, end) = edge.line_range.expect("line_range");
    assert!(
        start > 0 && start <= end,
        "non-positive line range {start}..{end}"
    );
}

#[test]
fn p63_default_noise_methods_are_filtered_from_calls() {
    // toString / dispose / initState / build / copyWith / hashCode are
    // common framework noise. The default focus / code view must not
    // surface them as calls edges.
    let tmp = workspace_with_evidence_and_noise();
    let v = view(tmp.path());
    let noisy: Vec<_> = v
        .edges
        .iter()
        .filter(|e| {
            e.kind == "calls"
                && matches!(
                    extract_target_name(&e.to).as_str(),
                    "toString"
                        | "dispose"
                        | "initState"
                        | "build"
                        | "copyWith"
                        | "hashCode"
                        | "noSuchMethod"
                        | "runtimeType"
                )
        })
        .collect();
    assert!(
        noisy.is_empty(),
        "default view must hide framework noise, got: {noisy:?}",
    );
}

#[test]
fn p63_noise_methods_remain_available_when_user_opts_in() {
    // Hiding noise is a *default*, not a hard delete. When the user passes
    // `include_noise = true`, the toString call from PaywallScreen.initState
    // must come back.
    let tmp = workspace_with_evidence_and_noise();
    let v = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Code,
            include_noise: true,
            ..Default::default()
        },
    )
    .unwrap();
    let has_tostring_call = v
        .edges
        .iter()
        .any(|e| e.kind == "calls" && extract_target_name(&e.to) == "toString");
    assert!(
        has_tostring_call,
        "include_noise=true must surface toString edges again"
    );
}

#[test]
fn p63_business_keyword_nodes_outrank_noise_in_focus_truncation() {
    // Focus on the paywall module with max_nodes=4: the business-relevant
    // chain (PaywallScreen + listenToPurchaseUpdates + applyPurchase +
    // IapProductIds) must outrank any incidental neighbour the truncation
    // priority would otherwise pick.
    let tmp = workspace_with_evidence_and_noise();
    let v = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Focus,
            focus: Some("module::lib/features/paywall".into()),
            max_nodes: Some(4),
            ..Default::default()
        },
    )
    .unwrap();
    let ids: std::collections::BTreeSet<String> = v.nodes.iter().map(|n| n.id.clone()).collect();
    // At least 3 of these 4 business-relevant ids must survive truncation.
    let business_ids = [
        "module::lib/features/paywall",
        "file::lib/features/paywall/paywall_screen.dart",
        "dart_class::lib/features/paywall/paywall_screen.dart#PaywallScreen",
        "dart_method::lib/features/paywall/paywall_screen.dart#PaywallScreen.listenToPurchaseUpdates",
    ];
    let kept = business_ids
        .iter()
        .filter(|b| ids.iter().any(|i| i == *b))
        .count();
    assert!(
        kept >= 3,
        "max-node truncation lost business-relevant nodes; kept={kept} ids={ids:?}",
    );
}

fn extract_target_name(id: &str) -> String {
    // dart_method::path#Class.method  -> method
    // dart_fn::path#name              -> name
    // dart_class::path#Class          -> Class
    // file::path                      -> path basename
    if let Some(after_hash) = id.split_once('#').map(|(_, r)| r) {
        if let Some((_, last)) = after_hash.rsplit_once('.') {
            return last.into();
        }
        return after_hash.into();
    }
    id.into()
}
