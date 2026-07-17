//! Integration tests for the SQLite migration layer.

use groundgraph_store::{Store, StoreError};
use tempfile::TempDir;

const EXPECTED_TABLES: &[&str] = &[
    "nodes",
    "edge_assertions",
    "evidence",
    "symbol_ranges",
    "file_index",
    "schema_version",
    // FTS5 content layer (migration 003). It is a virtual table but still
    // surfaces in `sqlite_master` as type='table'; guarding it here means a
    // dropped/renamed full-text migration fails CI instead of silently
    // breaking search.
    "node_fts",
    // `slice_cache` is intentionally absent: created by 001 but never read or
    // written, it was dropped by migration 005 (#151). Its absence is asserted
    // in `migration_creates_all_expected_tables` below.
];

fn table_names(store: &Store) -> Vec<String> {
    let conn = store.connection();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .expect("prepare sqlite_master query");
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query table names")
        .map(|r| r.expect("row"))
        .collect();
    names
}

#[test]
fn migration_creates_all_expected_tables() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("graph.db");

    let mut store = Store::open(&db_path).expect("open store");
    store.migrate().expect("apply migrations");

    let tables = table_names(&store);
    for expected in EXPECTED_TABLES {
        assert!(
            tables.iter().any(|t| t == expected),
            "expected table `{expected}` to exist after migration, got: {tables:?}",
        );
    }
    // #151: `slice_cache` was created by 001 but never read or written, so
    // migration 005 dropped it. Asserting its absence means a future revival
    // that re-adds the table without a migration fails CI.
    assert!(
        !tables.iter().any(|t| t == "slice_cache"),
        "slice_cache must be gone after migration 005, got: {tables:?}",
    );
}

/// The 7 adjacency / foreign-key indexes a fully-migrated DB must carry
/// (migration 002 introduced them; 004 replaced the three adjacency ones with
/// composite `_ord` covering indexes — see #140). Asserting names (not just
/// table names) means a dropped or renamed index migration fails CI instead of
/// silently regressing search/slice/impact to full scans.
const EXPECTED_INDEXES: &[&str] = &[
    // #140: composite `(<col>, id)` covering indexes; migration 004 renamed and
    // dropped the old single-column `idx_edge_assertions_{from,to,kind}`.
    "idx_edge_assertions_from_ord",
    "idx_edge_assertions_to_ord",
    "idx_edge_assertions_kind_ord",
    "idx_evidence_artifact",
    "idx_edge_assertions_source_file",
    "idx_edge_assertions_indexer",
    "idx_nodes_indexer",
];

#[test]
fn migration_creates_expected_indexes() {
    // #235: the table-name guard above never checked indexes. NB the schema
    // intentionally has *no* triggers — `node_fts` is rebuilt wholesale each
    // index run rather than synced via triggers — so there is nothing to assert
    // on that front; the meaningful guard is the index set from migration 002.
    let tmp = TempDir::new().expect("tempdir");
    let mut store = Store::open(tmp.path().join("graph.db")).expect("open store");
    store.migrate().expect("apply migrations");

    let conn = store.connection();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index' ORDER BY name")
        .expect("prepare index query");
    let indexes: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query index names")
        .map(|r| r.expect("row"))
        .collect();

    for expected in EXPECTED_INDEXES {
        assert!(
            indexes.iter().any(|i| i == expected),
            "expected index `{expected}` after migration, got: {indexes:?}",
        );
    }

    let trigger_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger'",
            [],
            |row| row.get(0),
        )
        .expect("count triggers");
    assert_eq!(
        trigger_count, 0,
        "schema is trigger-free by design (FTS rebuilt wholesale); \
         a new trigger here is unexpected and should be reviewed"
    );
}

#[test]
fn migration_rejects_a_future_schema_version() {
    // A downgraded binary opening an index written by a newer groundgraph must
    // fail loudly, not silently operate on an unknown future schema (#153).
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("graph.db");
    {
        let mut store = Store::open(&db_path).expect("open");
        store.migrate().expect("migrate");
        // Simulate a future binary having recorded a newer migration.
        store
            .connection()
            .execute("INSERT INTO schema_version(version) VALUES (9999)", [])
            .expect("insert future version");
    }
    let mut store = Store::open(&db_path).expect("reopen");
    let err = store.migrate().expect_err("must reject future schema");
    assert!(
        matches!(err, StoreError::SchemaTooNew { found: 9999, .. }),
        "expected SchemaTooNew, got {err:?}"
    );
}

#[test]
fn migration_is_idempotent() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("graph.db");

    {
        let mut store = Store::open(&db_path).expect("first open");
        store.migrate().expect("first migration");
    }
    {
        let mut store = Store::open(&db_path).expect("second open");
        store.migrate().expect("second migration must not fail");
        let tables = table_names(&store);
        for expected in EXPECTED_TABLES {
            assert!(
                tables.iter().any(|t| t == expected),
                "expected table `{expected}` to survive second migration, got: {tables:?}",
            );
        }
    }
}
