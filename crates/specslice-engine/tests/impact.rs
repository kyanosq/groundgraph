//! Integration tests for the impact engine.

use std::path::PathBuf;

use specslice_core::edge::EdgeKind;
use specslice_core::node::NodeKind;
use specslice_core::{ArtifactId, EdgeAssertion, EdgeSource, Node, SymbolRange};
use specslice_engine::dart_indexer::{index_dart, DartIndexOptions};
use specslice_engine::docs_indexer::{index_docs, DocsIndexOptions};
use specslice_engine::git_diff::{ChangeStatus, ChangedFile, Hunk};
use specslice_engine::impact::{
    compute_impact, compute_impact_with_policy, ImpactPolicy, ImpactPropagation,
};
use specslice_engine::links_indexer::{index_links, LinksIndexOptions};
use specslice_store::Store;
use tempfile::TempDir;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("flutter_watermark_app")
}

fn fresh_store_with_index() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();
    let fixture = fixture_path();
    index_docs(
        &mut store,
        &DocsIndexOptions {
            repo_root: fixture.clone(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: Vec::new(),
        },
    )
    .unwrap();
    index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: fixture.clone(),
            code_roots: vec![PathBuf::from("lib"), PathBuf::from("test")],
            ..Default::default()
        },
    )
    .unwrap();
    index_links(
        &mut store,
        &LinksIndexOptions {
            repo_root: fixture,
            manifest_path: PathBuf::from(".specslice/links.yaml"),
        },
    )
    .unwrap();
    (tmp, store)
}

#[test]
fn changing_method_walks_to_parent_class_and_requirement() {
    let (_tmp, store) = fresh_store_with_index();

    // Pretend the line containing `placeWatermark` body was changed.
    let changed = vec![ChangedFile {
        path: "lib/domain/watermark/auto_placement_service.dart".into(),
        status: ChangeStatus::Modified,
        hunks: vec![Hunk {
            new_start: 8,
            new_end: 8,
        }],
    }];

    let report = compute_impact(&store, &changed).unwrap();
    assert!(report
        .changed_symbols
        .iter()
        .any(|s| s.kind == "dart_method"
            && s.name
                .as_deref()
                .map(|n| n.ends_with("placeWatermark"))
                .unwrap_or(false)));
    assert!(report
        .affected_requirements
        .iter()
        .any(|r| r.id == "req::REQ-WATERMARK-001"));
    assert!(report
        .linked_tests
        .iter()
        .any(|t| t.path.as_deref() == Some("test/watermark/auto_placement_service_test.dart")));
    assert!(report
        .affected_docs
        .iter()
        .any(|d| d.path.as_deref() == Some("docs/watermark.md")));
    // Doc not changed → expect "Linked doc sections were not changed" info.
    assert!(report.info.iter().any(|m| m.contains("doc sections")));
    // Tests not changed → expect warning.
    assert!(report
        .warnings
        .iter()
        .any(|w| w.contains("no linked test changed")));
}

#[test]
fn changing_requirement_doc_finds_implementation_and_tests() {
    let (_tmp, store) = fresh_store_with_index();

    let changed = vec![ChangedFile {
        path: "docs/watermark.md".into(),
        status: ChangeStatus::Modified,
        hunks: vec![Hunk {
            new_start: 8,
            new_end: 8,
        }],
    }];

    let report = compute_impact(&store, &changed).unwrap();
    assert!(!report.changed_doc_sections.is_empty());
    assert!(report
        .affected_requirements
        .iter()
        .any(|r| r.id == "req::REQ-WATERMARK-001"));
    assert!(report
        .linked_tests
        .iter()
        .any(|t| t.path.as_deref() == Some("test/watermark/auto_placement_service_test.dart")));
}

#[test]
fn no_changes_yield_empty_report() {
    let (_tmp, store) = fresh_store_with_index();
    let report = compute_impact(&store, &[]).unwrap();
    assert!(report.changed_files.is_empty());
    assert!(report.affected_requirements.is_empty());
    assert!(report.warnings.is_empty());
    assert!(report.info.is_empty());
}

#[test]
fn doc_change_surfaces_linked_implementations() {
    // PRD §4.4 Doc Impact: changing a requirement doc must produce, in
    // addition to affected requirements and linked tests, the *implementation*
    // symbols that declare the requirement. This makes the report directly
    // actionable for "doc change → re-read these files".
    let (_tmp, store) = fresh_store_with_index();

    let changed = vec![ChangedFile {
        path: "docs/watermark.md".into(),
        status: ChangeStatus::Modified,
        hunks: vec![Hunk {
            new_start: 8,
            new_end: 8,
        }],
    }];
    let report = compute_impact(&store, &changed).unwrap();

    assert!(
        !report.linked_implementations.is_empty(),
        "linked_implementations must not be empty when REQ is affected"
    );
    assert!(report.linked_implementations.iter().any(
        |item| item.path.as_deref() == Some("lib/domain/watermark/auto_placement_service.dart")
    ));
}

#[test]
fn linked_implementations_field_is_empty_when_only_tests_change() {
    let (_tmp, store) = fresh_store_with_index();
    let changed = vec![ChangedFile {
        path: "test/watermark/auto_placement_service_test.dart".into(),
        status: ChangeStatus::Modified,
        hunks: vec![Hunk {
            new_start: 6,
            new_end: 6,
        }],
    }];
    let report = compute_impact(&store, &changed).unwrap();
    // The test file alone does not affect any requirement,
    // so there should be no linked implementations either.
    assert!(report.linked_implementations.is_empty());
}

#[test]
fn changing_test_file_does_not_emit_missing_test_warning() {
    let (_tmp, store) = fresh_store_with_index();

    let changed = vec![ChangedFile {
        path: "test/watermark/auto_placement_service_test.dart".into(),
        status: ChangeStatus::Modified,
        hunks: vec![Hunk {
            new_start: 6,
            new_end: 6,
        }],
    }];

    let report = compute_impact(&store, &changed).unwrap();
    assert!(report
        .warnings
        .iter()
        .all(|w| !w.contains("no linked test changed")));
}

/// P14 — impact must follow `Calls` / `References` edges from the
/// changed symbols outward so reviewers see (a) who is affected
/// downstream and (b) which tests exercise the changed code even when
/// no manual `declares_verification` manifest entry exists.
#[test]
fn impact_propagates_via_calls_and_references_to_callers_and_tests() {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();

    // Symbol A is what the diff hit; B calls A; T (a test) calls B.
    // With depth=2 we expect to surface both B and T, plus T promoted
    // into `linked_tests` even though no `declares_verification` exists.
    let path = "lib/foo.dart";
    let a_id = ArtifactId::new("dart_method::lib/foo.dart#A.greet");
    let b_id = ArtifactId::new("dart_method::lib/foo.dart#B.welcome");
    let t_id = ArtifactId::new("test_case::test/foo_test.dart#greet works");

    for (id, kind, name, start, end, file) in [
        (
            a_id.clone(),
            NodeKind::DartMethod,
            "A.greet",
            10u32,
            20u32,
            path,
        ),
        (
            b_id.clone(),
            NodeKind::DartMethod,
            "B.welcome",
            30,
            35,
            path,
        ),
        (
            t_id.clone(),
            NodeKind::TestCase,
            "greet works",
            5,
            8,
            "test/foo_test.dart",
        ),
    ] {
        let mut node = Node::new(id.clone(), kind);
        node.name = Some(name.to_string());
        node.path = Some(file.to_string());
        node.start_line = Some(start);
        node.end_line = Some(end);
        node.indexer = Some("test_fixture".into());
        store.upsert_node(&node).unwrap();
        store
            .upsert_symbol_range(&SymbolRange {
                file_path: file.into(),
                symbol_id: id,
                start_line: start,
                end_line: end,
                symbol_kind: kind,
                qualified_name: name.into(),
                parent_symbol_id: None,
            })
            .unwrap();
    }

    for (from, to, kind) in [
        (b_id.clone(), a_id.clone(), EdgeKind::Calls),
        (t_id.clone(), b_id.clone(), EdgeKind::Calls),
    ] {
        let mut edge = EdgeAssertion::fact(from, to, kind, EdgeSource::LanguageAdapter);
        edge.indexer = Some("test_fixture".into());
        store.upsert_edge(&edge).unwrap();
    }

    let changed = vec![ChangedFile {
        path: path.into(),
        status: ChangeStatus::Modified,
        hunks: vec![Hunk {
            new_start: 15,
            new_end: 15,
        }],
    }];

    // Baseline (depth=0): no propagation, so neither B nor T appear.
    let report_baseline = compute_impact_with_policy(
        &store,
        &changed,
        ImpactPolicy {
            propagation: ImpactPropagation {
                call_depth: 0,
                ..ImpactPropagation::default()
            },
            ..ImpactPolicy::default()
        },
    )
    .unwrap();
    assert!(
        report_baseline.propagated_symbols.is_empty(),
        "depth=0 must disable propagation"
    );

    // depth=2: B (depth=1) and T (depth=2) both surface; T is also in
    // linked_tests so PR reviewers see it without a manual manifest.
    let report = compute_impact_with_policy(
        &store,
        &changed,
        ImpactPolicy {
            propagation: ImpactPropagation {
                call_depth: 2,
                ..ImpactPropagation::default()
            },
            ..ImpactPolicy::default()
        },
    )
    .unwrap();
    let propagated_ids: Vec<&str> = report
        .propagated_symbols
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert!(
        propagated_ids.contains(&b_id.as_str()),
        "depth=1 caller B must surface in propagated_symbols, got {propagated_ids:?}"
    );
    assert!(
        propagated_ids.contains(&t_id.as_str()),
        "depth=2 caller T must surface in propagated_symbols, got {propagated_ids:?}"
    );
    assert!(
        report.linked_tests.iter().any(|t| t.id == t_id.as_str()),
        "test reachable via Calls/References must be promoted into linked_tests"
    );
    // The changed symbol itself must not appear in propagated_symbols
    // (it's already in changed_symbols).
    assert!(
        !propagated_ids.contains(&a_id.as_str()),
        "changed symbol must not duplicate into propagated_symbols"
    );
}

#[test]
fn impact_policy_can_disable_parent_doc_and_warning_propagation() {
    let (_tmp, store) = fresh_store_with_index();
    let method_change = vec![ChangedFile {
        path: "lib/domain/watermark/auto_placement_service.dart".into(),
        status: ChangeStatus::Modified,
        hunks: vec![Hunk {
            new_start: 8,
            new_end: 8,
        }],
    }];
    let report = compute_impact_with_policy(
        &store,
        &method_change,
        ImpactPolicy {
            propagate_to_parent_symbol: false,
            missing_test_change_level: "off".into(),
            ..ImpactPolicy::default()
        },
    )
    .unwrap();
    assert!(
        report.affected_requirements.is_empty(),
        "method-only change should not walk to parent when policy disables it"
    );
    assert!(report.warnings.is_empty());

    let doc_change = vec![ChangedFile {
        path: "docs/watermark.md".into(),
        status: ChangeStatus::Modified,
        hunks: vec![Hunk {
            new_start: 8,
            new_end: 8,
        }],
    }];
    let report = compute_impact_with_policy(
        &store,
        &doc_change,
        ImpactPolicy {
            include_doc_changes: false,
            ..ImpactPolicy::default()
        },
    )
    .unwrap();
    assert!(report.changed_doc_sections.is_empty());
    assert!(report.affected_requirements.is_empty());
}
