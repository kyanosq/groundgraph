//! Integration tests for the store repository APIs.

use specslice_core::{
    ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Evidence, EvidenceKind, Node, NodeKind,
    SymbolRange,
};
use specslice_store::{FileIndexEntry, FulltextRow, Store};
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
        EdgeSource::ExternalManifest,
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
fn evidence_can_round_trip_every_kind() {
    let (_tmp, mut store) = fresh_store();
    use specslice_core::EvidenceKind;
    for (idx, kind) in [
        EvidenceKind::DocSection,
        EvidenceKind::DartDocComment,
        EvidenceKind::DartTestCall,
        EvidenceKind::DartGroupCall,
        EvidenceKind::Import,
        EvidenceKind::GitDiff,
    ]
    .iter()
    .enumerate()
    {
        let id = ArtifactId::new(format!("ev::{idx}"));
        let artifact = ArtifactId::new(format!("a::{idx}"));
        let ev = Evidence {
            id: id.clone(),
            artifact_id: artifact.clone(),
            kind: *kind,
            path: None,
            start_line: None,
            end_line: None,
            snippet: None,
            hash: None,
            metadata_json: None,
        };
        store.upsert_evidence(&ev).unwrap();
        let listed = store.list_evidence_for_artifact(&artifact).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, *kind);
    }
}

#[test]
fn edges_can_round_trip_every_kind_and_source_and_status() {
    let (_tmp, mut store) = fresh_store();
    use specslice_core::{EdgeAssertion, EdgeKind, EdgeSource};
    let kinds = [
        EdgeKind::Contains,
        EdgeKind::Imports,
        EdgeKind::Documents,
        EdgeKind::DeclaresImplementation,
        EdgeKind::DeclaresVerification,
    ];
    let sources = [
        EdgeSource::Filesystem,
        EdgeSource::LanguageAdapter,
        EdgeSource::Markdown,
        EdgeSource::ExternalManifest,
        EdgeSource::GitDiff,
    ];
    for (idx, kind) in kinds.iter().enumerate() {
        let mut edge = EdgeAssertion::declared(
            ArtifactId::new(format!("from-{idx}")),
            ArtifactId::new(format!("to-{idx}")),
            *kind,
            sources[idx % sources.len()],
        );
        // Toggle status to Deprecated to cover both states.
        if idx % 2 == 0 {
            edge.status = specslice_core::EdgeStatus::Deprecated;
        }
        store.upsert_edge(&edge).unwrap();
        let reloaded = store.list_edges_by_kind(*kind).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].kind, *kind);
    }
    let all = store.list_all_edges().unwrap();
    assert_eq!(all.len(), kinds.len());
}

#[test]
fn list_all_nodes_returns_every_inserted_kind() {
    let (_tmp, mut store) = fresh_store();
    use specslice_core::NodeKind;
    let kinds = [
        NodeKind::File,
        NodeKind::Requirement,
        NodeKind::AcceptanceCriterion,
        NodeKind::Adr,
        NodeKind::DocSection,
        NodeKind::DartClass,
        NodeKind::DartMethod,
        NodeKind::DartFunction,
        NodeKind::DartConstructor,
        NodeKind::TestCase,
        NodeKind::TestGroup,
        NodeKind::DartProvider,
        NodeKind::Route,
        NodeKind::Storage,
        NodeKind::BusinessCandidate,
        NodeKind::SwiftClass,
        NodeKind::SwiftStruct,
        NodeKind::SwiftEnum,
        NodeKind::SwiftProtocol,
        NodeKind::SwiftMethod,
        NodeKind::SwiftFunction,
        NodeKind::SwiftInitializer,
        NodeKind::GoStruct,
        NodeKind::GoInterface,
        NodeKind::GoMethod,
        NodeKind::GoFunction,
        NodeKind::PythonModule,
        NodeKind::PythonClass,
        NodeKind::PythonFunction,
        NodeKind::PythonMethod,
        NodeKind::TypescriptModule,
        NodeKind::TypescriptClass,
        NodeKind::TypescriptInterface,
        NodeKind::TypescriptEnum,
        NodeKind::TypescriptFunction,
        NodeKind::TypescriptMethod,
        NodeKind::JavaPackage,
        NodeKind::JavaClass,
        NodeKind::JavaInterface,
        NodeKind::JavaEnum,
        NodeKind::JavaMethod,
        NodeKind::JavaConstructor,
    ];
    for (idx, kind) in kinds.iter().enumerate() {
        let mut node = Node::new(ArtifactId::new(format!("n::{idx}")), *kind);
        node.name = Some(format!("{kind:?}"));
        store.upsert_node(&node).unwrap();
    }
    let all = store.list_all_nodes().unwrap();
    assert_eq!(all.len(), kinds.len());
}

#[test]
fn decode_error_surfaces_when_raw_sql_writes_unknown_kind() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("graph.db");
    {
        let mut store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
    }
    // Use a separate rusqlite connection to inject an invalid kind.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO nodes (id, kind) VALUES ('bad', 'martian_kind')",
        [],
    )
    .unwrap();
    drop(conn);

    let store = Store::open(&db_path).unwrap();
    let err = store
        .find_node(&ArtifactId::new("bad"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("martian_kind") || err.contains("unknown node kind"));
}

#[test]
fn decode_error_for_edge_kind_when_raw_sql_writes_unknown_value() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("graph.db");
    {
        let mut store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
    }
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence) \
         VALUES ('e', 'a', 'b', 'mystery_kind', 'filesystem', 'fact', 'confirmed', 1.0)",
        [],
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&db_path).unwrap();
    let err = store.list_all_edges().unwrap_err().to_string();
    assert!(err.contains("mystery_kind") || err.contains("unknown edge kind"));
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
        EdgeSource::ExternalManifest,
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

#[test]
fn decoding_a_corrupted_line_number_returns_an_error_not_silent_wrap() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("graph.db");
    {
        let mut store = Store::open(&db_path).expect("open store");
        store.migrate().expect("migrate");
        store
            .upsert_node(&Node::new(
                ArtifactId::new("req::REQ-CORRUPT"),
                NodeKind::Requirement,
            ))
            .unwrap();
    }
    // Open the DB directly to inject values SQLite is happy to store but that
    // do not fit in u32 — simulates a buggy producer writing the graph file.
    {
        let raw = rusqlite::Connection::open(&db_path).unwrap();
        raw.execute(
            "UPDATE nodes SET start_line = ?1, end_line = ?2 WHERE id = 'req::REQ-CORRUPT'",
            rusqlite::params![-1_i64, (u32::MAX as i64) + 1],
        )
        .unwrap();
    }

    let store = Store::open(&db_path).expect("reopen store");
    let err = store
        .find_node(&ArtifactId::new("req::REQ-CORRUPT"))
        .expect_err("decoder must reject out-of-range line numbers");
    let msg = format!("{err:#}");
    let msg_lc = msg.to_lowercase();
    assert!(
        msg_lc.contains("line") || msg_lc.contains("range") || msg_lc.contains("u32"),
        "unexpected error: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Fulltext (FTS5) content layer — the search engine's BM25-ranked body index.
// ---------------------------------------------------------------------------

#[test]
fn fulltext_rebuild_then_match_ranks_bodies_by_bm25() {
    let (_tmp, mut store) = fresh_store();
    assert!(
        store.fulltext_available().expect("availability probe"),
        "after migrate() the FTS5 table must exist"
    );
    store
        .rebuild_fulltext(&[
            FulltextRow {
                node_id: "rust::a.rs::parse_sql_tables".into(),
                body: "advance past whole parens byte boundary panic fix".into(),
            },
            FulltextRow {
                node_id: "rust::b.rs::unrelated".into(),
                body: "completely different words about routing".into(),
            },
            FulltextRow {
                node_id: "doc::guide.md#搜索".into(),
                body: "错位 位竞 竞争 strategy notes".into(),
            },
        ])
        .expect("rebuild fulltext");

    // Single-token match hits exactly the body that contains it, best first.
    let hits = store
        .fulltext_match("\"boundary\"", 10)
        .expect("match boundary");
    assert_eq!(hits.len(), 1, "exactly one body mentions `boundary`");
    assert_eq!(hits[0].node_id, "rust::a.rs::parse_sql_tables");

    // AND expression must require all tokens.
    let hits = store
        .fulltext_match("\"byte\" \"boundary\" \"panic\"", 10)
        .expect("AND match");
    assert_eq!(hits.len(), 1);

    // Pre-tokenised CJK bigrams round-trip.
    let hits = store
        .fulltext_match("\"错位\" \"竞争\"", 10)
        .expect("cjk bigram match");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_id, "doc::guide.md#搜索");

    // Rebuild replaces all prior rows (no stale leftovers).
    store
        .rebuild_fulltext(&[FulltextRow {
            node_id: "rust::c.rs::fresh".into(),
            body: "fresh only".into(),
        }])
        .expect("second rebuild");
    let hits = store.fulltext_match("\"boundary\"", 10).expect("match");
    assert!(hits.is_empty(), "old rows must be gone after rebuild");
}

#[test]
fn fulltext_match_is_unavailable_not_an_error_on_a_pre_fts_database() {
    // A database that never ran migration 003 (e.g. created by an older
    // binary) has no `node_fts`. Read commands must degrade ("no content
    // layer"), never crash. `Store::open` without `migrate()` gives exactly
    // that shape.
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("graph.db");
    let store = Store::open(&path).expect("open pre-fts db");
    assert!(
        !store.fulltext_available().expect("probe"),
        "node_fts absent → content layer unavailable"
    );
}

// ---------------------------------------------------------------------------
// Bulk write session (single-transaction indexing)
// ---------------------------------------------------------------------------

/// Indexing upserts hundreds of thousands of rows. In autocommit mode every
/// statement is its own WAL commit (pwrite + lock churn → the django 99s
/// profile was dominated by pread/pwrite/fsync). A bulk session must turn the
/// whole ingest into ONE transaction, while staying idempotent (double begin /
/// double commit are no-ops, not errors).
#[test]
fn bulk_session_wraps_upserts_in_one_transaction() {
    let (_tmp, mut store) = fresh_store();

    store.begin_bulk().expect("begin bulk");
    assert!(
        !store.connection().is_autocommit(),
        "begin_bulk must open a real transaction"
    );
    // Nested begin must be a no-op, not "cannot start a transaction".
    store.begin_bulk().expect("nested begin is a no-op");

    let node = Node::new(ArtifactId::new("sym::a.rs#alpha"), NodeKind::CFunction);
    store.upsert_node(&node).expect("upsert inside bulk");

    store.commit_bulk().expect("commit bulk");
    assert!(
        store.connection().is_autocommit(),
        "commit_bulk must return to autocommit"
    );
    store.commit_bulk().expect("double commit is a no-op");

    let found = store.find_node(&node.id).expect("query").expect("exists");
    assert_eq!(found.id, node.id);
}

/// The repository helpers that used to open their own `BEGIN` (clear,
/// fulltext rebuild, SCIP suppression) must compose inside a bulk session
/// instead of failing with "cannot start a transaction within a transaction".
#[test]
fn internal_write_helpers_compose_inside_bulk_session() {
    let (_tmp, mut store) = fresh_store();

    let mut node = Node::new(ArtifactId::new("sym::b.rs#beta"), NodeKind::CFunction);
    node.indexer = Some("tree_sitter".into());
    node.source_file = Some("b.rs".into());
    store.upsert_node(&node).expect("seed node");

    store.begin_bulk().expect("begin bulk");
    store
        .clear_indexer_outputs("tree_sitter")
        .expect("clear inside bulk session");
    store
        .rebuild_fulltext(&[FulltextRow {
            node_id: "sym::b.rs#beta".into(),
            body: "beta body".into(),
        }])
        .expect("fulltext rebuild inside bulk session");
    store
        .delete_precision_edges_for_files_except(&["b.rs".into()], "scip")
        .expect("suppression inside bulk session");
    store.commit_bulk().expect("commit");

    assert!(
        store.find_node(&node.id).expect("query").is_none(),
        "clear_indexer_outputs must have removed the seeded node"
    );
}

/// An error mid-session must not leave the connection stuck inside a failed
/// transaction: rollback_bulk restores autocommit so later writes succeed.
#[test]
fn rollback_bulk_discards_the_session_and_restores_autocommit() {
    let (_tmp, mut store) = fresh_store();

    store.begin_bulk().expect("begin");
    let node = Node::new(ArtifactId::new("sym::c.rs#gamma"), NodeKind::CFunction);
    store.upsert_node(&node).expect("upsert");
    store.rollback_bulk().expect("rollback");

    assert!(store.connection().is_autocommit(), "back to autocommit");
    assert!(
        store.find_node(&node.id).expect("query").is_none(),
        "rolled-back rows must be gone"
    );
    store.rollback_bulk().expect("rollback outside session is a no-op");
}

/// Large graphs (django: 229 MB graph.db) thrash the default 2 MB page cache:
/// profiles showed pread + BtreeTableMoveto dominating. Opening the store
/// must provision an indexing-grade page cache and mmap window.
#[test]
fn open_configures_page_cache_and_mmap_for_large_graphs() {
    let (_tmp, store) = fresh_store();
    let cache: i64 = store
        .connection()
        .query_row("PRAGMA cache_size", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cache, -65536, "expect 64 MiB page cache, got {cache}");
    let mmap: i64 = store
        .connection()
        .query_row("PRAGMA mmap_size", [], |r| r.get(0))
        .unwrap();
    assert!(
        mmap >= 1 << 28,
        "expect a ≥256 MiB mmap window for read paths, got {mmap}"
    );
}
