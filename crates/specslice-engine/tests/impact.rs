//! Integration tests for the impact engine.

use std::path::PathBuf;

use specslice_engine::dart_indexer::{index_dart, DartIndexOptions};
use specslice_engine::docs_indexer::{index_docs, DocsIndexOptions};
use specslice_engine::git_diff::{ChangeStatus, ChangedFile, Hunk};
use specslice_engine::impact::compute_impact;
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
        },
    )
    .unwrap();
    index_dart(
        &mut store,
        &DartIndexOptions {
            repo_root: fixture,
            code_roots: vec![PathBuf::from("lib"), PathBuf::from("test")],
            ..Default::default()
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
    // Doc not changed → expect "Related doc sections were not changed" info.
    assert!(report.info.iter().any(|m| m.contains("doc sections")));
    // Tests not changed → expect warning.
    assert!(report
        .warnings
        .iter()
        .any(|w| w.contains("no related test changed")));
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
fn doc_change_surfaces_related_implementations() {
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
        !report.related_implementations.is_empty(),
        "related_implementations must not be empty when REQ is affected"
    );
    assert!(report.related_implementations.iter().any(
        |item| item.path.as_deref() == Some("lib/domain/watermark/auto_placement_service.dart")
    ));
}

#[test]
fn related_implementations_field_is_empty_when_only_tests_change() {
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
    // The test file alone does not affect any requirement (no doc trace),
    // so there should be no related implementations either.
    assert!(report.related_implementations.is_empty());
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
        .all(|w| !w.contains("no related test changed")));
}
