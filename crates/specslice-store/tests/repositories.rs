//! Integration tests for the store repository APIs.

use specslice_core::{
    ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Evidence, EvidenceKind, Node, NodeKind,
    SymbolRange,
};
use specslice_store::{FileIndexEntry, Store};
use tempfile::TempDir;

fn fresh_store() -> (TempDir, Store) {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("graph.db");
    let mut store = Store::open(&path).expect("open store");
    store.migrate().expect("migrate");
    (tmp, store)
}

#[test]
fn upsert_node_is_idempotent_and_round_trips() {
    let (_tmp, mut store) = fresh_store();
    let mut node = Node::new(
        ArtifactId::new("req::REQ-WATERMARK-001"),
        NodeKind::Requirement,
    );
    node.name = Some("Auto watermark placement".into());
    node.path = Some("docs/watermark.md".into());
    node.start_line = Some(1);
    node.end_line = Some(20);

    store.upsert_node(&node).expect("first upsert");
    store.upsert_node(&node).expect("second upsert idempotent");

    let loaded = store
        .find_node(&node.id)
        .expect("query")
        .expect("node exists");
    assert_eq!(loaded, node);

    let requirements = store
        .list_nodes_by_kind(NodeKind::Requirement)
        .expect("list");
    assert_eq!(requirements.len(), 1);
    assert_eq!(requirements[0].id, node.id);
}

#[test]
fn upsert_edge_is_idempotent_and_indexes_by_kind() {
    let (_tmp, mut store) = fresh_store();
    let edge = EdgeAssertion::declared(
        ArtifactId::new("dart_class::a.dart#Foo"),
        ArtifactId::new("req::REQ-1"),
        EdgeKind::DeclaresImplementation,
        EdgeSource::ExplicitTrace,
    );
    store.upsert_edge(&edge).expect("first upsert");
    store.upsert_edge(&edge).expect("second upsert idempotent");

    let listed = store
        .list_edges_by_kind(EdgeKind::DeclaresImplementation)
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0], edge);

    let outgoing = store.list_edges_from(&edge.from_id).unwrap();
    assert_eq!(outgoing.len(), 1);
    let incoming = store.list_edges_to(&edge.to_id).unwrap();
    assert_eq!(incoming.len(), 1);
}

#[test]
fn upsert_evidence_round_trips() {
    let (_tmp, mut store) = fresh_store();
    let ev = Evidence {
        id: ArtifactId::new("ev::1"),
        artifact_id: ArtifactId::new("a"),
        kind: EvidenceKind::DocSection,
        path: Some("docs/a.md".into()),
        start_line: Some(2),
        end_line: Some(5),
        snippet: Some("# H".into()),
        hash: None,
        metadata_json: None,
    };
    store.upsert_evidence(&ev).unwrap();
    store.upsert_evidence(&ev).unwrap();
    let listed = store.list_evidence_for_artifact(&ev.artifact_id).unwrap();
    assert_eq!(listed, vec![ev]);
}

#[test]
fn symbol_ranges_query_by_file_and_line() {
    let (_tmp, mut store) = fresh_store();
    let class_id = ArtifactId::new("dart_class::a.dart#Foo");
    let method_id = ArtifactId::new("dart_method::a.dart#Foo.bar");

    store
        .upsert_symbol_range(&SymbolRange {
            file_path: "a.dart".into(),
            symbol_id: class_id.clone(),
            start_line: 1,
            end_line: 20,
            symbol_kind: NodeKind::DartClass,
            qualified_name: "Foo".into(),
            parent_symbol_id: None,
        })
        .unwrap();
    store
        .upsert_symbol_range(&SymbolRange {
            file_path: "a.dart".into(),
            symbol_id: method_id.clone(),
            start_line: 5,
            end_line: 10,
            symbol_kind: NodeKind::DartMethod,
            qualified_name: "Foo.bar".into(),
            parent_symbol_id: Some(class_id.clone()),
        })
        .unwrap();
    // Idempotent: re-upserting the method must not duplicate.
    store
        .upsert_symbol_range(&SymbolRange {
            file_path: "a.dart".into(),
            symbol_id: method_id.clone(),
            start_line: 5,
            end_line: 10,
            symbol_kind: NodeKind::DartMethod,
            qualified_name: "Foo.bar".into(),
            parent_symbol_id: Some(class_id.clone()),
        })
        .unwrap();

    let all = store.list_symbol_ranges_for_file("a.dart").unwrap();
    assert_eq!(all.len(), 2);

    let hits = store.find_symbols_intersecting("a.dart", 7, 8).unwrap();
    let ids: Vec<_> = hits.iter().map(|r| r.symbol_id.clone()).collect();
    assert!(ids.contains(&class_id));
    assert!(ids.contains(&method_id));
}

#[test]
fn file_index_upserts_and_reads_back_hash() {
    let (_tmp, mut store) = fresh_store();
    let entry = FileIndexEntry {
        path: "docs/a.md".into(),
        hash: "deadbeef".into(),
        kind: "markdown".into(),
        indexed_at: "2026-05-19T00:00:00Z".into(),
        index_generation: 1,
    };
    store.upsert_file_index(&entry).unwrap();

    let again = FileIndexEntry {
        hash: "newhash".into(),
        index_generation: 2,
        ..entry.clone()
    };
    store.upsert_file_index(&again).unwrap();
    assert_eq!(
        store.get_file_hash("docs/a.md").unwrap().as_deref(),
        Some("newhash")
    );
    assert!(store.get_file_hash("missing.md").unwrap().is_none());
}

#[test]
fn clear_indexer_outputs_removes_relevant_rows() {
    let (_tmp, mut store) = fresh_store();
    let mut node = Node::new(
        ArtifactId::new("dart_class::a.dart#Foo"),
        NodeKind::DartClass,
    );
    node.indexer = Some("dart_lightweight".into());
    store.upsert_node(&node).unwrap();

    let mut edge = EdgeAssertion::declared(
        ArtifactId::new("dart_class::a.dart#Foo"),
        ArtifactId::new("req::REQ-1"),
        EdgeKind::DeclaresImplementation,
        EdgeSource::ExplicitTrace,
    );
    edge.indexer = Some("dart_lightweight".into());
    store.upsert_edge(&edge).unwrap();

    store
        .upsert_symbol_range(&SymbolRange {
            file_path: "a.dart".into(),
            symbol_id: ArtifactId::new("dart_class::a.dart#Foo"),
            start_line: 1,
            end_line: 10,
            symbol_kind: NodeKind::DartClass,
            qualified_name: "Foo".into(),
            parent_symbol_id: None,
        })
        .unwrap();

    store.clear_indexer_outputs("dart_lightweight").unwrap();

    assert!(store
        .find_node(&ArtifactId::new("dart_class::a.dart#Foo"))
        .unwrap()
        .is_none());
    assert!(store
        .list_edges_by_kind(EdgeKind::DeclaresImplementation)
        .unwrap()
        .is_empty());
    assert!(store
        .list_symbol_ranges_for_file("a.dart")
        .unwrap()
        .is_empty());
}
