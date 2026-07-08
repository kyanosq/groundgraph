//! Exercise the error branches of the store.

use groundgraph_store::{Store, StoreError};

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

/// #215: operationally-distinct SQLite result codes route into typed variants
/// (BUSY/CORRUPT/READONLY/FULL) so callers can react, rather than collapsing
/// into a single opaque `Sqlite`. Driven straight off the primary result codes
/// — and an extended code (`SQLITE_BUSY_SNAPSHOT = 517`, whose low byte is
/// `SQLITE_BUSY`) to prove extended codes are folded to their primary class.
#[test]
fn sqlite_errors_classify_by_result_code() {
    use rusqlite::ffi;

    fn classify(code: i32) -> StoreError {
        rusqlite::Error::SqliteFailure(ffi::Error::new(code), Some("synth".into())).into()
    }

    assert!(matches!(classify(ffi::SQLITE_BUSY), StoreError::Busy(_)));
    assert!(matches!(classify(ffi::SQLITE_LOCKED), StoreError::Busy(_)));
    // Extended busy code (517) must still classify as Busy.
    assert!(matches!(
        classify(ffi::SQLITE_BUSY_SNAPSHOT),
        StoreError::Busy(_)
    ));
    assert!(matches!(
        classify(ffi::SQLITE_CORRUPT),
        StoreError::Corrupt(_)
    ));
    assert!(matches!(
        classify(ffi::SQLITE_NOTADB),
        StoreError::Corrupt(_)
    ));
    assert!(matches!(
        classify(ffi::SQLITE_READONLY),
        StoreError::ReadOnly(_)
    ));
    assert!(matches!(
        classify(ffi::SQLITE_CANTOPEN),
        StoreError::ReadOnly(_)
    ));
    assert!(matches!(
        classify(ffi::SQLITE_PERM),
        StoreError::ReadOnly(_)
    ));
    assert!(matches!(
        classify(ffi::SQLITE_FULL),
        StoreError::DiskFull(_)
    ));
    // A generic SQLITE_ERROR (1) has no operational class → catch-all.
    assert!(matches!(classify(1), StoreError::Sqlite(_)));

    // Only Busy is retryable.
    assert!(classify(ffi::SQLITE_BUSY).is_retryable());
    assert!(!classify(ffi::SQLITE_CORRUPT).is_retryable());
    assert!(!classify(1).is_retryable());
}
