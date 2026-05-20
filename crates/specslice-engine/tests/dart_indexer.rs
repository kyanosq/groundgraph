//! Integration tests for the Dart indexer using the watermark fixture.

use std::path::PathBuf;

use specslice_core::artifact_id::{dart_class_id, dart_method_id};
use specslice_core::{EdgeKind, NodeKind};
use specslice_engine::dart_indexer::{
    index_dart, DartIndexOptions, DART_INDEXER_NAME, RESOLVER_DART_ANALYZER,
};
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

fn fresh_store() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();
    (tmp, store)
}

#[test]
fn indexing_dart_fixture_yields_expected_counts() {
    let (_tmp, mut store) = fresh_store();
    let opts = DartIndexOptions {
        repo_root: fixture_path(),
        code_roots: vec![PathBuf::from("lib"), PathBuf::from("test")],
        ..Default::default()
    };
    let result = index_dart(&mut store, &opts).unwrap();

    assert!(result.files >= 3, "files: {}", result.files);
    assert!(result.symbols >= 4, "symbols: {}", result.symbols);
    assert_eq!(result.tests, 1, "test cases");
    assert_eq!(result.declared_implementations, 0);
    assert_eq!(result.declared_verifications, 0);

    let impl_edges = store
        .list_edges_by_kind(EdgeKind::DeclaresImplementation)
        .unwrap();
    assert!(impl_edges.is_empty());
    let verifies = store
        .list_edges_by_kind(EdgeKind::DeclaresVerification)
        .unwrap();
    assert!(verifies.is_empty());

    // Symbol ranges include the method, and the method maps to its parent class.
    let class = dart_class_id(
        "tests/fixtures/flutter_watermark_app/lib/domain/watermark/auto_placement_service.dart"
            .strip_prefix("tests/fixtures/flutter_watermark_app/")
            .unwrap_or(""),
        "AutoPlacementService",
    );
    let method = dart_method_id(
        "lib/domain/watermark/auto_placement_service.dart",
        "AutoPlacementService",
        "placeWatermark",
    );
    let _ = class;
    let _ = method;

    let ranges = store
        .list_symbol_ranges_for_file("lib/domain/watermark/auto_placement_service.dart")
        .unwrap();
    let class_range = ranges
        .iter()
        .find(|r| r.symbol_kind == NodeKind::DartClass)
        .expect("class range");
    let method_range = ranges
        .iter()
        .find(|r| {
            r.symbol_kind == NodeKind::DartMethod && r.qualified_name.ends_with(".placeWatermark")
        })
        .expect("method range");
    assert_eq!(
        method_range.parent_symbol_id.as_ref().unwrap(),
        &class_range.symbol_id
    );

    // Brace-only line that closes the method must be inside method range,
    // and that point intersects with both method and class.
    let hits = store
        .find_symbols_intersecting(
            "lib/domain/watermark/auto_placement_service.dart",
            method_range.start_line + 1,
            method_range.start_line + 1,
        )
        .unwrap();
    let symbol_ids: Vec<_> = hits.iter().map(|r| r.symbol_id.clone()).collect();
    assert!(symbol_ids.contains(&class_range.symbol_id));
    assert!(symbol_ids.contains(&method_range.symbol_id));
}

#[test]
fn re_indexing_is_idempotent() {
    let (_tmp, mut store) = fresh_store();
    let opts = DartIndexOptions {
        repo_root: fixture_path(),
        code_roots: vec![PathBuf::from("lib"), PathBuf::from("test")],
        ..Default::default()
    };
    let first = index_dart(&mut store, &opts).unwrap();
    store.clear_indexer_outputs(DART_INDEXER_NAME).unwrap();
    store.clear_indexer_outputs(RESOLVER_DART_ANALYZER).unwrap();
    let second = index_dart(&mut store, &opts).unwrap();
    assert_eq!(first, second);
}
