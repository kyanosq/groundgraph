//! P6: GraphViewModel engine-level tests.
//!
//! The engine owns the read-only contract; CLI is just a serializer. These
//! tests pin down: layered nodes, layered confirmed edges, focus filtering,
//! `max_nodes` truncation, risk findings, and a stable empty-state for the
//! not-yet-implemented candidates store.

use std::path::Path;

use specslice_engine::graph::{
    build_graph_view, GraphFinding, GraphLayer, GraphOptions, GraphStatus, GraphView,
    GraphViewModel,
};
use specslice_engine::index::{index_repository, IndexOptions};
use specslice_engine::init::{init_repository, InitOptions};
use tempfile::TempDir;

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Watermark fixture with frontmatter doc + Dart class + Dart test + a
/// fully-linked manifest. Used as the canonical "happy path" graph.
fn fixture_with_manifest() -> TempDir {
    let tmp = TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    write(
        &tmp.path().join("docs/watermark.md"),
        "---\nid: REQ-WATERMARK-001\ntype: requirement\ntitle: Auto watermark placement\n---\n\n# Auto watermark placement\n\nWatermark must avoid faces.\n",
    );
    write(
        &tmp.path()
            .join("lib/domain/watermark/auto_placement_service.dart"),
        "class AutoPlacementService {\n  void placeWatermark() {}\n}\n",
    );
    write(
        &tmp.path()
            .join("test/watermark/auto_placement_service_test.dart"),
        "void main() {\n  test('places watermark outside face region', () {});\n}\n",
    );
    write(
        &tmp.path().join(".specslice/links.yaml"),
        "requirements:\n  REQ-WATERMARK-001:\n    docs:\n      - docs/watermark.md#auto-watermark-placement\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n    tests:\n      - test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region\n",
    );
    index_repository(IndexOptions::all(tmp.path())).unwrap();
    tmp
}

/// Same as `fixture_with_manifest` but the manifest only links docs +
/// implementation — verification is intentionally missing so the
/// `missing_linked_test` risk surfaces.
fn fixture_with_missing_test() -> TempDir {
    let tmp = TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    write(
        &tmp.path().join("docs/watermark.md"),
        "---\nid: REQ-WATERMARK-001\ntype: requirement\ntitle: Auto watermark placement\n---\n\n# Auto watermark placement\n\nWatermark must avoid faces.\n",
    );
    write(
        &tmp.path()
            .join("lib/domain/watermark/auto_placement_service.dart"),
        "class AutoPlacementService {\n  void placeWatermark() {}\n}\n",
    );
    write(
        &tmp.path().join("test/watermark/other_test.dart"),
        "void main() {\n  test('unrelated', () {});\n}\n",
    );
    write(
        &tmp.path().join(".specslice/links.yaml"),
        "requirements:\n  REQ-WATERMARK-001:\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n",
    );
    index_repository(IndexOptions::all(tmp.path())).unwrap();
    tmp
}

fn second_requirement_workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    write(
        &tmp.path().join("docs/watermark.md"),
        "---\nid: REQ-WATERMARK-001\ntype: requirement\ntitle: Auto watermark placement\n---\n\n# Auto watermark placement\n\nFaces.\n",
    );
    write(
        &tmp.path().join("docs/audit.md"),
        "---\nid: REQ-AUDIT-001\ntype: requirement\ntitle: Audit log\n---\n\n# Audit log\n\nLogs.\n",
    );
    write(
        &tmp.path()
            .join("lib/domain/watermark/auto_placement_service.dart"),
        "class AutoPlacementService {\n  void placeWatermark() {}\n}\n",
    );
    write(
        &tmp.path().join("lib/domain/audit/audit_log_service.dart"),
        "class AuditLogService {\n  void record() {}\n}\n",
    );
    write(
        &tmp.path()
            .join("test/watermark/auto_placement_service_test.dart"),
        "void main() {\n  test('places watermark outside face region', () {});\n}\n",
    );
    write(
        &tmp.path().join("test/audit/audit_log_service_test.dart"),
        "void main() {\n  test('records audit events', () {});\n}\n",
    );
    write(
        &tmp.path().join(".specslice/links.yaml"),
        "requirements:\n  REQ-WATERMARK-001:\n    docs:\n      - docs/watermark.md#auto-watermark-placement\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n    tests:\n      - test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region\n  REQ-AUDIT-001:\n    docs:\n      - docs/audit.md#audit-log\n    implementations:\n      - lib/domain/audit/audit_log_service.dart#AuditLogService\n    tests:\n      - test/audit/audit_log_service_test.dart#records-audit-events\n",
    );
    index_repository(IndexOptions::all(tmp.path())).unwrap();
    tmp
}

fn find_node<'a>(view: &'a GraphViewModel, id: &str) -> &'a specslice_engine::graph::GraphNode {
    view.nodes
        .iter()
        .find(|n| n.id == id)
        .unwrap_or_else(|| panic!("node {id} not in graph: {:?}", view.nodes))
}

#[test]
fn graph_view_contains_layered_confirmed_nodes() {
    let tmp = fixture_with_manifest();

    let view = build_graph_view(tmp.path(), GraphOptions::default()).unwrap();

    assert_eq!(view.schema_version, 2);
    assert_eq!(view.view, "overview");
    assert_eq!(view.stats.business_logic, 1);
    assert!(view.stats.documents >= 1);
    assert!(view.stats.code_symbols >= 1);
    assert!(view.stats.tests >= 1);
    assert!(view.stats.confirmed_edges >= 3, "{:?}", view.stats);

    // Requirement node is Confirmed; DocSection is a Fact.
    let req = view
        .nodes
        .iter()
        .find(|n| n.kind == "requirement")
        .expect("requirement node");
    assert_eq!(req.layer, GraphLayer::Confirmed);
    assert_eq!(req.status, GraphStatus::Confirmed);
    assert_eq!(req.id, "req::REQ-WATERMARK-001");

    let doc = view
        .nodes
        .iter()
        .find(|n| n.kind == "doc_section")
        .expect("doc_section node");
    assert_eq!(doc.layer, GraphLayer::Fact);

    // Confirmed business edge from the manifest.
    let impl_edge = view
        .edges
        .iter()
        .find(|e| e.kind == "declares_implementation")
        .expect("declares_implementation edge");
    assert_eq!(impl_edge.layer, GraphLayer::Confirmed);
    assert_eq!(impl_edge.status, GraphStatus::Confirmed);
    assert_eq!(impl_edge.source.as_deref(), Some("external_manifest"));

    // Structural file→symbol edges remain Fact layer.
    let contains_edge = view
        .edges
        .iter()
        .find(|e| e.kind == "contains")
        .expect("contains edge");
    assert_eq!(contains_edge.layer, GraphLayer::Fact);
}

#[test]
fn graph_view_assigns_kind_strings_to_every_node_kind_we_index() {
    let tmp = fixture_with_manifest();
    let view = build_graph_view(tmp.path(), GraphOptions::default()).unwrap();

    let kinds: std::collections::BTreeSet<_> = view.nodes.iter().map(|n| n.kind.as_str()).collect();
    for required in [
        "requirement",
        "doc_section",
        "dart_class",
        "dart_method",
        "test_case",
    ] {
        assert!(
            kinds.contains(required),
            "missing kind `{required}` in {kinds:?}"
        );
    }
}

#[test]
fn graph_focus_keeps_only_connected_neighbourhood() {
    let tmp = second_requirement_workspace();

    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            focus: Some("REQ-WATERMARK-001".into()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(view.focus.as_deref(), Some("REQ-WATERMARK-001"));
    let watermark = find_node(&view, "req::REQ-WATERMARK-001");
    assert_eq!(watermark.layer, GraphLayer::Confirmed);
    assert!(view.nodes.iter().all(|n| n.id != "req::REQ-AUDIT-001"));
    assert!(view.nodes.iter().all(|n| !n.id.contains("audit")));
    assert!(view
        .edges
        .iter()
        .all(|e| !e.from.contains("audit") && !e.to.contains("audit")));
}

#[test]
fn graph_focus_resolves_full_artifact_id() {
    let tmp = second_requirement_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            focus: Some("req::REQ-WATERMARK-001".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(view.nodes.iter().any(|n| n.id == "req::REQ-WATERMARK-001"));
    assert!(view.nodes.iter().all(|n| n.id != "req::REQ-AUDIT-001"));
}

#[test]
fn graph_focus_unknown_id_returns_empty_with_finding() {
    let tmp = second_requirement_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            focus: Some("REQ-DOES-NOT-EXIST".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(view.nodes.is_empty());
    assert!(view.edges.is_empty());
    assert!(
        view.findings
            .iter()
            .any(|f: &GraphFinding| f.code == "focus_not_found"),
        "{:?}",
        view.findings
    );
}

#[test]
fn graph_max_nodes_truncates_and_emits_finding() {
    let tmp = second_requirement_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            max_nodes: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(view.nodes.len() <= 3, "{}", view.nodes.len());
    let trunc = view
        .findings
        .iter()
        .find(|f| f.code == "graph_truncated")
        .expect("graph_truncated finding");
    assert_eq!(trunc.severity, "warning");
    // Confirmed business nodes survive the truncation.
    assert!(view.nodes.iter().any(|n| n.layer == GraphLayer::Confirmed));
    // Edges referencing dropped nodes are gone.
    let node_ids: std::collections::HashSet<_> = view.nodes.iter().map(|n| n.id.clone()).collect();
    for e in &view.edges {
        assert!(
            node_ids.contains(&e.from) && node_ids.contains(&e.to),
            "{e:?}"
        );
    }
}

#[test]
fn graph_include_risks_lifts_missing_linked_test_into_findings() {
    let tmp = fixture_with_missing_test();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            include_risks: true,
            ..Default::default()
        },
    )
    .unwrap();
    let missing = view
        .findings
        .iter()
        .find(|f| f.code == "missing_linked_test")
        .expect("missing_linked_test finding");
    assert_eq!(missing.severity, "warning");
    assert_eq!(missing.target_id.as_deref(), Some("req::REQ-WATERMARK-001"));
    assert!(view.stats.risks >= 1);
}

#[test]
fn graph_include_risks_disabled_yields_zero_findings_for_missing_test() {
    let tmp = fixture_with_missing_test();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            include_risks: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(view
        .findings
        .iter()
        .all(|f| f.code != "missing_linked_test"));
}

// ---------------------------------------------------------------------------
// P6.1: code graph explorer
// ---------------------------------------------------------------------------

fn multi_module_workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    write(
        &tmp.path().join("lib/features/editor/canvas.dart"),
        "class Canvas {\n  void draw() {}\n}\n",
    );
    write(
        &tmp.path().join("lib/features/editor/toolbar.dart"),
        "class Toolbar {\n  void render() {}\n}\n",
    );
    write(
        &tmp.path().join("lib/core/utils.dart"),
        "class Utils {\n  static int double(int x) => x * 2;\n}\n",
    );
    write(
        &tmp.path().join("test/features/editor/canvas_test.dart"),
        "void main() {\n  test('paints rectangles', () {});\n  test('paints text', () {});\n}\n",
    );
    write(
        &tmp.path().join("docs/overview.md"),
        "# Overview\n\nAn editor.\n",
    );
    index_repository(IndexOptions::all(tmp.path())).unwrap();
    tmp
}

#[test]
fn overview_view_emits_module_nodes_with_child_counts() {
    let tmp = multi_module_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Overview,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.view, "overview");

    let modules: Vec<&specslice_engine::graph::GraphNode> =
        view.nodes.iter().filter(|n| n.kind == "module").collect();
    assert!(!modules.is_empty(), "no module nodes emitted");
    let module_paths: Vec<&str> = modules
        .iter()
        .map(|n| n.path.as_deref().unwrap_or(""))
        .collect();
    // Nested module hierarchy must include the leaf editor module.
    assert!(
        module_paths.contains(&"lib/features/editor"),
        "missing leaf module: {module_paths:?}"
    );
    assert!(
        module_paths.contains(&"lib"),
        "missing root lib module: {module_paths:?}"
    );

    // Top-level modules (no module parent) carry a child_count summarising
    // their direct subtree.
    let lib = modules
        .iter()
        .find(|n| n.path.as_deref() == Some("lib"))
        .expect("lib module");
    assert!(lib.parent_id.is_none());
    assert!(lib.child_count >= 2, "lib should aggregate ≥2 children");
}

#[test]
fn overview_view_links_files_and_symbols_to_modules() {
    let tmp = multi_module_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Overview,
            ..Default::default()
        },
    )
    .unwrap();

    let canvas_class = view
        .nodes
        .iter()
        .find(|n| n.id == "dart_class::lib/features/editor/canvas.dart#Canvas")
        .expect("canvas class node");
    let parent = canvas_class.parent_id.as_deref().expect("class parent_id");
    let parent_file = view
        .nodes
        .iter()
        .find(|n| n.id == parent)
        .expect("parent file node");
    assert_eq!(parent_file.kind, "file");
    assert_eq!(
        parent_file.parent_id.as_deref(),
        Some("module::lib/features/editor"),
        "file should point at its module"
    );
    let leaf_module = view
        .nodes
        .iter()
        .find(|n| n.id == "module::lib/features/editor")
        .expect("leaf module");
    assert_eq!(
        leaf_module.parent_id.as_deref(),
        Some("module::lib/features"),
        "leaf module should chain up"
    );
}

#[test]
fn overview_default_visible_marks_only_top_level_modules() {
    let tmp = multi_module_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Overview,
            ..Default::default()
        },
    )
    .unwrap();
    let visible: Vec<&specslice_engine::graph::GraphNode> =
        view.nodes.iter().filter(|n| n.default_visible).collect();
    assert!(!visible.is_empty(), "no nodes visible by default");
    for n in &visible {
        assert_eq!(n.kind, "module", "non-module shown by default: {n:?}");
        assert!(
            n.parent_id.is_none(),
            "non-root module shown by default: {n:?}"
        );
    }
    // Symbols hidden.
    assert!(view
        .nodes
        .iter()
        .filter(|n| n.kind == "dart_class")
        .all(|n| !n.default_visible));
}

#[test]
fn code_view_matches_overview_aggregation_but_marks_view_field() {
    let tmp = multi_module_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Code,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.view, "code");
    assert!(view.nodes.iter().any(|n| n.kind == "module"));
}

#[test]
fn business_view_on_empty_repo_emits_no_business_finding() {
    let tmp = multi_module_workspace();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Business,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.view, "business");
    // No requirement nodes exist → engine surfaces a guidance finding.
    assert!(view
        .nodes
        .iter()
        .all(|n| n.kind != "requirement" || !n.default_visible));
    assert!(
        view.findings.iter().any(|f| f.code == "no_business_logic"),
        "missing no_business_logic finding: {:?}",
        view.findings
    );
}

#[test]
fn business_view_with_manifest_shows_requirement_plus_neighbours() {
    let tmp = fixture_with_manifest();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Business,
            ..Default::default()
        },
    )
    .unwrap();
    let visible: Vec<&specslice_engine::graph::GraphNode> =
        view.nodes.iter().filter(|n| n.default_visible).collect();
    let req = visible
        .iter()
        .find(|n| n.id == "req::REQ-WATERMARK-001")
        .expect("requirement visible");
    assert_eq!(req.layer, GraphLayer::Confirmed);
    // Direct neighbours (doc section, class, test case) are also visible.
    assert!(visible.iter().any(|n| n.kind == "doc_section"));
    assert!(visible.iter().any(|n| n.kind == "dart_class"));
    assert!(visible.iter().any(|n| n.kind == "test_case"));
}

#[test]
fn focus_view_overrides_default_visibility_to_neighbourhood() {
    let tmp = fixture_with_manifest();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            view: GraphView::Focus,
            focus: Some("REQ-WATERMARK-001".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.view, "focus");
    let visible: Vec<&specslice_engine::graph::GraphNode> =
        view.nodes.iter().filter(|n| n.default_visible).collect();
    assert!(visible.iter().any(|n| n.id == "req::REQ-WATERMARK-001"));
    // Modules are not the primary visible surface in focus view.
    assert!(visible.iter().all(|n| n.kind != "module"));
}

#[test]
fn graph_include_candidates_is_stable_empty_state_until_store_exists() {
    let tmp = fixture_with_manifest();
    let view = build_graph_view(
        tmp.path(),
        GraphOptions {
            include_candidates: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view.stats.candidate_edges, 0);
    assert!(view.nodes.iter().all(|n| n.layer != GraphLayer::Candidate));
    assert!(view.edges.iter().all(|e| e.layer != GraphLayer::Candidate));
}
