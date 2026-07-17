//! P23.8 — SQLite-grade property tests for the store layer.
//!
//! The example-based tests in `repositories.rs` pin specific kinds/sources;
//! these fuzz the *freeform* columns (ids, paths, hashes, indexer tags,
//! generations, JSON blobs, confidence) with arbitrary safe text to prove the
//! write→read round-trip is lossless and `upsert` is idempotent regardless of
//! the byte content SQLite has to store.

use groundgraph_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use groundgraph_store::Store;
use proptest::prelude::*;
use tempfile::TempDir;

fn open_store() -> (TempDir, Store) {
    let tmp = TempDir::new().expect("tempdir");
    let mut store = Store::open(tmp.path().join("graph.db")).expect("open");
    store.migrate().expect("migrate");
    (tmp, store)
}

// Arbitrary UTF-8 excluding NUL (the one byte SQLite TEXT cannot hold).
const SAFE_TEXT: &str = "[^\\x00]{0,40}";
// Non-empty id-ish text (still excludes NUL); ids are the primary key so they
// must be non-empty.
const SAFE_ID: &str = "[^\\x00]{1,40}";

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn arbitrary_node_round_trips_and_upsert_is_idempotent(
        id in SAFE_ID,
        kind in proptest::sample::select(NodeKind::ALL.to_vec()),
        path in proptest::option::of(SAFE_TEXT),
        name in proptest::option::of(SAFE_TEXT),
        // Valid line ranges only: `Node::validate` (#168) rejects an inverted
        // `start > end` at the store boundary, so swap any inverted pair the
        // arbitrary u32s produce rather than generating illegal nodes.
        (start_line, end_line) in (
            proptest::option::of(any::<u32>()),
            proptest::option::of(any::<u32>()),
        )
            .prop_map(|(s, e)| match (s, e) {
                (Some(a), Some(b)) if a > b => (Some(b), Some(a)),
                other => other,
            }),
        content_hash in proptest::option::of(SAFE_TEXT),
        stable_key in proptest::option::of(SAFE_TEXT),
        source_file in proptest::option::of(SAFE_TEXT),
        indexer in proptest::option::of(SAFE_TEXT),
        metadata_json in proptest::option::of(SAFE_TEXT),
    ) {
        let (_tmp, mut store) = open_store();
        let node = Node {
            id: ArtifactId::new(id),
            kind,
            path,
            name,
            start_line,
            end_line,
            content_hash,
            stable_key,
            source_file,
            indexer,
            metadata_json,
        };
        store.upsert_node(&node).unwrap();
        // Second upsert must be a no-op (idempotent), not a duplicate.
        store.upsert_node(&node).unwrap();

        let loaded = store.find_node(&node.id).unwrap().expect("node present");
        prop_assert_eq!(loaded, node.clone());
        let by_kind = store.list_nodes_by_kind(node.kind).unwrap();
        prop_assert_eq!(by_kind.len(), 1);
        prop_assert_eq!(&by_kind[0], &node);
    }

    #[test]
    fn arbitrary_edge_round_trips_and_upsert_is_idempotent(
        from in SAFE_ID,
        to in SAFE_ID,
        // Arbitrary f32 — including NaN/±∞/out-of-range: `Confidence::new`
        // sanitises at construction, and the row must round-trip equal to the
        // sanitised value (#168).
        confidence in any::<f32>(),
        evidence_json in proptest::option::of(SAFE_TEXT),
        source_file in proptest::option::of(SAFE_TEXT),
        indexer in proptest::option::of(SAFE_TEXT),
        metadata_json in proptest::option::of(SAFE_TEXT),
    ) {
        let (_tmp, mut store) = open_store();
        let mut edge = EdgeAssertion::fact(
            ArtifactId::new(from),
            ArtifactId::new(to),
            EdgeKind::Calls,
            EdgeSource::ExternalManifest,
        );
        edge.confidence = groundgraph_core::Confidence::new(confidence);
        edge.evidence_json = evidence_json;
        edge.source_file = source_file;
        edge.indexer = indexer;
        edge.metadata_json = metadata_json;

        store.upsert_edge(&edge).unwrap();
        store.upsert_edge(&edge).unwrap();

        let by_kind = store.list_edges_by_kind(EdgeKind::Calls).unwrap();
        prop_assert_eq!(by_kind.len(), 1);
        prop_assert_eq!(&by_kind[0], &edge);
        prop_assert_eq!(store.list_edges_from(&edge.from_id).unwrap().len(), 1);
        prop_assert_eq!(store.list_edges_to(&edge.to_id).unwrap().len(), 1);
    }
}
