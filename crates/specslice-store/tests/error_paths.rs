//! Exercise the error branches of the store.

use specslice_store::{Store, StoreError};

#[test]
fn open_returns_create_dir_error_when_parent_is_a_file() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    // Try to create a database at `<file>/sub/graph.db` — the parent path
    // resolves to a file, so create_dir_all should fail.
    let target = tmp.path().join("sub").join("graph.db");
    match Store::open(&target) {
        Ok(_) => panic!("expected create_dir failure"),
        Err(err) => {
            assert!(matches!(err, StoreError::CreateDir { .. }), "{err:?}");
            let msg = format!("{err}");
            assert!(msg.contains("failed to create database directory"));
        }
    }
}

#[test]
fn open_returns_open_db_error_when_path_is_a_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    // A directory is not openable as a SQLite database file.
    match Store::open(tmp.path()) {
        Ok(_) => panic!("expected open to fail on a directory"),
        Err(err) => {
            assert!(matches!(err, StoreError::OpenDb { .. }), "{err:?}");
            let msg = format!("{err}");
            assert!(msg.contains("failed to open SQLite database"));
        }
    }
}

#[test]
fn store_path_accessor_returns_input_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("graph.db");
    let store = Store::open(&path).unwrap();
    assert_eq!(store.path(), path);
    let _ = store.connection();
}

#[test]
fn migrate_is_callable_twice_on_same_handle() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap();
}

#[test]
fn sqlite_error_round_trips_through_from() {
    let raw = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(1), Some("synth".into()));
    let via_from: StoreError = raw.into();
    assert!(matches!(via_from, StoreError::Sqlite(_)));
    let msg = format!("{via_from}");
    assert!(msg.contains("sqlite error"));
}
