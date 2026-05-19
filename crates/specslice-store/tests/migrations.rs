//! Integration tests for the SQLite migration layer.

use specslice_store::Store;
use tempfile::TempDir;

const EXPECTED_TABLES: &[&str] = &[
    "nodes",
    "edge_assertions",
    "evidence",
    "symbol_ranges",
    "file_index",
    "slice_cache",
    "schema_version",
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
