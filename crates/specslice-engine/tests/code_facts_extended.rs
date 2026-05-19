//! Extended logic checks for the Dart code-fact graph (P6.2 follow-up).
//!
//! These tests model a wider slice of real Flutter code than the original
//! pixcraft scenario so we can catch silent regressions in the lightweight
//! parser, the body-reference scanner, and the focus subgraph expansion.
//!
//! Each `#[test]` corresponds to one "logic check" that must hold for the
//! graph to behave like a code-fact graph rather than a directory tree.

use std::collections::BTreeSet;
use std::path::Path;

use specslice_engine::graph::{
    build_graph_view, GraphEdge, GraphNode, GraphOptions, GraphView, GraphViewModel,
};
use specslice_engine::index::{index_repository, IndexOptions};
use specslice_engine::init::{init_repository, InitOptions};
use tempfile::TempDir;

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Rich fixture covering eleven Dart constructs the adapter must keep
/// straight:
///
/// 1. Top-level `dart_function` calling a method on a class.
/// 2. Constructor with body that calls another method.
/// 3. Multi-class file (two classes in the same file).
/// 4. Abstract method declaration with no body.
/// 5. Class field with generic type (`Future<int>`) and field initialiser.
/// 6. Static class constant referenced from another file.
/// 7. `test()` / `group()` calls extracted as test artefacts.
/// 8. Identifier inside a `///` doc comment must not produce edges.
/// 9. Identifier inside a string literal must not produce edges.
/// 10. Underscored library-private method names (`_doThing`) still
///     participate in calls/references.
/// 11. Field-typed access (`field.method()`) resolves to the field's
///     declared class type and to the called method on that class.
fn rich_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    write(
        &tmp.path().join("lib/iap/iap_constants.dart"),
        "class IapProductIds {\n  static const String monthly = 'pro_monthly';\n  static const String yearly = 'pro_yearly';\n  static const List<String> all = <String>[monthly, yearly];\n}\n",
    );
    write(
        &tmp.path().join("lib/settings/pro_provider.dart"),
        // Multi-class file: ProNotifier + ProState.
        "import '../iap/iap_constants.dart';\n\
         \n\
         class ProState {\n  bool isPro = false;\n}\n\
         \n\
         class ProNotifier {\n  ProState current = ProState();\n\n  ProNotifier() {\n    bootstrap();\n  }\n\n  /// Reference to IapProductIds in a doc comment must NOT emit edges.\n  void bootstrap() {\n    final ids = IapProductIds.all;\n  }\n\n  Future<void> applyPurchase(String id) async {\n    if (IapProductIds.all.contains(id)) {\n      current.isPro = true;\n      _schedule();\n    }\n  }\n\n  Future<int> remaining() async => 0;\n\n  void _schedule() {}\n}\n",
    );
    write(
        &tmp.path().join("lib/paywall/paywall.dart"),
        "import '../iap/iap_constants.dart';\n\
         import '../settings/pro_provider.dart';\n\
         \n\
         abstract class PaywallBase {\n  void render();\n}\n\
         \n\
         class Paywall implements PaywallBase {\n  ProNotifier notifier = ProNotifier();\n\n  @override\n  void render() {\n    final tag = 'IapProductIds.monthly';\n    notifier.applyPurchase(IapProductIds.monthly);\n  }\n\n  void _buy(String id) {\n    notifier.applyPurchase(id);\n  }\n}\n",
    );
    write(
        &tmp.path().join("lib/main.dart"),
        "import 'settings/pro_provider.dart';\n\nFuture<void> bootstrapPro() async {\n  final notifier = ProNotifier();\n  await notifier.applyPurchase('pro_monthly');\n}\n",
    );
    write(
        &tmp.path().join("test/paywall_test.dart"),
        "import 'package:test/test.dart';\n\nvoid main() {\n  group('paywall', () {\n    test('renders', () {});\n  });\n}\n",
    );
    index_repository(IndexOptions::all(tmp.path())).unwrap();
    tmp
}

fn ids(view: &GraphViewModel) -> BTreeSet<String> {
    view.nodes.iter().map(|n| n.id.clone()).collect()
}

fn edge_exists(view: &GraphViewModel, kind: &str, from_sub: &str, to_sub: &str) -> bool {
    view.edges
        .iter()
        .any(|e| e.kind == kind && e.from.contains(from_sub) && e.to.contains(to_sub))
}

fn node<'a>(view: &'a GraphViewModel, id_sub: &str) -> Option<&'a GraphNode> {
    view.nodes.iter().find(|n| n.id.contains(id_sub))
}

fn edges_from<'a>(view: &'a GraphViewModel, from_sub: &str) -> Vec<&'a GraphEdge> {
    view.edges
        .iter()
        .filter(|e| e.from.contains(from_sub))
        .collect()
}

fn full_graph(repo_root: &Path) -> GraphViewModel {
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
fn check_01_top_level_function_calls_class_method() {
    // bootstrapPro() -> ProNotifier.applyPurchase
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    assert!(
        edge_exists(&view, "calls", "bootstrapPro", "ProNotifier.applyPurchase"),
        "expected calls edge from bootstrapPro to applyPurchase. edges: {:?}",
        view.edges
            .iter()
            .map(|e| format!("{}|{}->{}", e.kind, e.from, e.to))
            .collect::<Vec<_>>()
    );
}

#[test]
fn check_02_constructor_body_emits_calls_edge() {
    // ProNotifier() { bootstrap(); }  ⇒ calls ProNotifier.bootstrap
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    assert!(
        edge_exists(
            &view,
            "calls",
            "lib/settings/pro_provider.dart#ProNotifier.<default>",
            "ProNotifier.bootstrap",
        ),
        "constructor body did not emit calls edge: {:?}",
        view.edges
            .iter()
            .filter(|e| e.from.contains("ProNotifier"))
            .map(|e| format!("{}|{}->{}", e.kind, e.from, e.to))
            .collect::<Vec<_>>()
    );
}

#[test]
fn check_03_multi_class_file_keeps_both_classes() {
    // ProState and ProNotifier both live in pro_provider.dart.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    let id_set = ids(&view);
    assert!(id_set
        .iter()
        .any(|id| id == "dart_class::lib/settings/pro_provider.dart#ProState"));
    assert!(id_set
        .iter()
        .any(|id| id == "dart_class::lib/settings/pro_provider.dart#ProNotifier"));
}

#[test]
fn check_04_abstract_method_recorded_without_body_scanned() {
    // PaywallBase.render() is declared but has no body. It must appear as
    // a dart_method node with no outgoing calls/references edges.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    let render =
        node(&view, "PaywallBase.render").expect("abstract render method missing from graph");
    assert_eq!(render.kind, "dart_method");
    let edges = edges_from(&view, "PaywallBase.render");
    assert!(
        edges
            .iter()
            .all(|e| e.kind != "calls" && e.kind != "references"),
        "abstract method body must not produce calls/references edges: {:?}",
        edges
            .iter()
            .map(|e| format!("{}|{}->{}", e.kind, e.from, e.to))
            .collect::<Vec<_>>()
    );
}

#[test]
fn check_05_field_with_generic_type_is_handled() {
    // ProNotifier.remaining is `Future<int>` returning method; the parser
    // must still produce the symbol despite the angle brackets in the
    // return type.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    assert!(
        node(&view, "ProNotifier.remaining").is_some(),
        "generic-return method should still be parsed"
    );
}

#[test]
fn check_06_static_constant_reference_from_another_file() {
    // Paywall.render references IapProductIds.monthly.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    assert!(
        edge_exists(&view, "references", "Paywall.render", "IapProductIds"),
        "Paywall.render must reference IapProductIds"
    );
}

#[test]
fn check_07_test_and_group_calls_emit_test_nodes() {
    // group('paywall', ...) -> test_group; test('renders', ...) -> test_case
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    let kinds: BTreeSet<&str> = view.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert!(kinds.contains("test_case"), "missing test_case nodes");
    assert!(kinds.contains("test_group"), "missing test_group nodes");
}

#[test]
fn check_08_identifier_inside_doc_comment_is_not_an_edge() {
    // bootstrap()'s `/// Reference to IapProductIds in a doc comment must
    // NOT emit edges.` line must not pull an edge from bootstrap to
    // IapProductIds via the comment text — only via the real `final ids =
    // IapProductIds.all;` statement on the next line.
    //
    // We can't directly assert "no edge via comment", but we *can* assert
    // the count: there is exactly one references edge from bootstrap to
    // IapProductIds, not two.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    let refs: Vec<_> = view
        .edges
        .iter()
        .filter(|e| {
            e.kind == "references"
                && e.from.contains("ProNotifier.bootstrap")
                && e.to.contains("IapProductIds")
        })
        .collect();
    assert_eq!(
        refs.len(),
        1,
        "expected exactly one references edge bootstrap->IapProductIds, got {refs:?}"
    );
}

#[test]
fn check_09_identifier_inside_string_literal_is_not_an_edge() {
    // Paywall.render contains `final tag = 'IapProductIds.monthly';` as a
    // string. The references edge for Paywall.render → IapProductIds must
    // come from the `notifier.applyPurchase(IapProductIds.monthly)` line,
    // not from inside the quoted literal. We assert the edge exists, then
    // confirm only a single such edge exists for Paywall.render.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    let refs: Vec<_> = view
        .edges
        .iter()
        .filter(|e| {
            e.kind == "references"
                && e.from.contains("Paywall.render")
                && e.to.contains("IapProductIds")
        })
        .collect();
    assert_eq!(
        refs.len(),
        1,
        "string literal must not cause a duplicate edge: {refs:?}"
    );
}

#[test]
fn check_10_underscored_private_method_participates() {
    // Paywall._buy → ProNotifier.applyPurchase (field-typed call) and
    // ProNotifier._schedule called from applyPurchase.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    assert!(
        edge_exists(&view, "calls", "Paywall._buy", "ProNotifier.applyPurchase"),
        "underscored private method must still emit calls edge"
    );
    assert!(
        edge_exists(&view, "calls", "ProNotifier.applyPurchase", "_schedule"),
        "applyPurchase must call _schedule"
    );
}

#[test]
fn check_11_field_typed_access_resolves_class_type() {
    // Paywall.notifier is `ProNotifier`. The body call
    // `notifier.applyPurchase(...)` must resolve to:
    //  - a `references` edge Paywall.render → ProNotifier
    //  - a `calls` edge Paywall.render → ProNotifier.applyPurchase
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    assert!(
        edge_exists(&view, "references", "Paywall.render", "ProNotifier"),
        "field-typed access must emit references to field's class"
    );
    assert!(
        edge_exists(
            &view,
            "calls",
            "Paywall.render",
            "ProNotifier.applyPurchase"
        ),
        "field-typed call must emit calls to ProNotifier.applyPurchase"
    );
}

#[test]
fn check_12_focus_on_class_expands_to_methods_and_neighbours() {
    // Focusing on the ProNotifier class must surface every method on it
    // (bootstrap / applyPurchase / remaining / _schedule / <default>
    // constructor) plus IapProductIds via references.
    let tmp = rich_fixture();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Focus,
            focus: Some("dart_class::lib/settings/pro_provider.dart#ProNotifier".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let id_set = ids(&view);
    for needle in [
        "ProNotifier.applyPurchase",
        "ProNotifier.bootstrap",
        "ProNotifier._schedule",
        "ProNotifier.<default>",
        "IapProductIds",
    ] {
        assert!(
            id_set.iter().any(|id| id.contains(needle)),
            "class focus missed {needle}"
        );
    }
}

#[test]
fn check_13_focus_on_unknown_id_emits_finding() {
    let tmp = rich_fixture();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Focus,
            focus: Some("nope_does_not_exist".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(view.nodes.is_empty(), "unknown focus must clear the graph");
    assert!(
        view.findings.iter().any(|f| f.code == "focus_not_found"),
        "unknown focus must emit focus_not_found finding"
    );
}

#[test]
fn check_14_business_view_warns_when_no_requirements() {
    // The rich fixture declares no business requirements, so a Business
    // view must surface the `no_business_logic` finding rather than
    // pretending to have linked work.
    let tmp = rich_fixture();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Business,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        view.findings.iter().any(|f| f.code == "no_business_logic"),
        "business view without requirements must emit no_business_logic"
    );
}

#[test]
fn check_15_default_view_keeps_calls_and_references_in_stats() {
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    let kinds: BTreeSet<&str> = view.edges.iter().map(|e| e.kind.as_str()).collect();
    for needed in ["calls", "references", "contains", "imports"] {
        assert!(
            kinds.contains(needed),
            "default code-view graph missing {needed} edges: {kinds:?}"
        );
    }
}

#[test]
fn check_16_max_nodes_truncation_emits_finding() {
    // Truncate down to 2 nodes — every fixture produces > 2 — and verify
    // we get the `graph_truncated` finding plus the priority-kept nodes.
    let tmp = rich_fixture();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Code,
            max_nodes: Some(2),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.nodes.len(), 2, "max_nodes must clip to limit");
    assert!(
        view.findings.iter().any(|f| f.code == "graph_truncated"),
        "max_nodes clip must emit graph_truncated"
    );
}

#[test]
fn check_17_module_parents_form_a_chain() {
    // module::lib/settings should have parent module::lib; module::lib
    // should have no parent. This is what makes the focus-descendant
    // expansion work.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    let lib_settings = node(&view, "module::lib/settings").expect("lib/settings missing");
    assert_eq!(lib_settings.parent_id.as_deref(), Some("module::lib"));
    let lib = node(&view, "module::lib").expect("lib module missing");
    assert!(lib.parent_id.is_none(), "lib is the root module");
}

#[test]
fn check_18_focus_on_top_level_function_pulls_in_callee_class() {
    // Focusing on `bootstrapPro` should expand to ProNotifier and
    // applyPurchase via the 1-hop neighbour pass.
    let tmp = rich_fixture();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Focus,
            focus: Some("dart_fn::lib/main.dart#bootstrapPro".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let id_set = ids(&view);
    assert!(id_set
        .iter()
        .any(|id| id.contains("ProNotifier.applyPurchase")));
}

#[test]
fn check_19_no_self_loops_in_calls() {
    // A method that recursively names itself in its body must not appear
    // as a self-referential edge. We assert this property over the full
    // graph rather than constructing a separate fixture.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    let self_loops: Vec<_> = view.edges.iter().filter(|e| e.from == e.to).collect();
    assert!(
        self_loops.is_empty(),
        "self-loops must not appear in the code-fact graph: {self_loops:?}"
    );
}

#[test]
fn check_20_calls_and_references_are_fact_layer() {
    // Body-derived edges are Fact-layer (not Confirmed); they should never
    // be presented as manifest-declared truth.
    let tmp = rich_fixture();
    let view = full_graph(tmp.path());
    for e in &view.edges {
        if e.kind == "calls" || e.kind == "references" {
            assert!(
                matches!(e.layer, specslice_engine::graph::GraphLayer::Fact),
                "{:?} edge promoted out of Fact layer: {:?}",
                e.kind,
                e
            );
        }
    }
}
