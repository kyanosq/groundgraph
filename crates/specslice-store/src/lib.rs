//! SpecSlice graph store (SQLite).
//!
//! MVP-0 surface:
//! - [`Store::open`] — create or open the SQLite database at the given path.
//! - [`Store::migrate`] — idempotently apply schema migrations.
//! - [`Store::connection`] — borrow the underlying connection (read access).

mod migrations;
mod repositories;

pub use repositories::FileIndexEntry;

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("failed to create database directory {path:?}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to open SQLite database at {path:?}: {source}")]
    OpenDb {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    #[error("migration {version} failed: {source}")]
    Migration {
        version: i32,
        #[source]
        source: rusqlite::Error,
    },

    #[error("sqlite error: {0}")]
    Sqlite(#[source] rusqlite::Error),
}

impl StoreError {
    pub(crate) fn sqlite(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Handle to the SpecSlice SQLite graph store.
pub struct Store {
    pub(crate) conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open (or create) the SQLite database at `path`.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| StoreError::CreateDir {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        let conn = Connection::open(&path).map_err(|source| StoreError::OpenDb {
            path: path.clone(),
            source,
        })?;
        // Write-ahead logging + synchronous=NORMAL: indexing upserts tens of
        // thousands of rows in autocommit mode; the SQLite default (rollback
        // journal + synchronous=FULL) fsyncs on every statement, which made a
        // ~100-file repo take minutes (disk-bound, near-idle CPU). WAL+NORMAL
        // drops the per-commit fsync while staying durable across app crashes —
        // acceptable for a rebuildable index cache. busy_timeout avoids spurious
        // "database is locked" under the WAL reader/writer split.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;",
        )
        .map_err(|source| StoreError::OpenDb {
            path: path.clone(),
            source,
        })?;
        Ok(Self { conn, path })
    }

    /// Idempotently apply all schema migrations.
    pub fn migrate(&mut self) -> StoreResult<()> {
        migrations::apply_all(&mut self.conn)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Borrow the underlying SQLite connection. Useful for read-only queries
    /// and integration tests; higher-level repositories will land later.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_enables_wal_and_normal_synchronous_for_bulk_write_throughput() {
        // Indexing performs tens of thousands of node/edge upserts. Under the
        // SQLite defaults (rollback journal + synchronous=FULL) every autocommit
        // statement fsyncs twice, making a 100-file repo take minutes. WAL +
        // synchronous=NORMAL removes the per-commit fsync (durable across app
        // crashes, only at-risk on OS crash — acceptable for a rebuildable index
        // cache), which is the single highest-leverage write-throughput fix.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("graph.db")).unwrap();
        let journal: String = store
            .connection()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal.to_ascii_lowercase(), "wal", "WAL journal expected");
        let sync: i64 = store
            .connection()
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "synchronous=NORMAL (1) expected, got {sync}");
    }

    fn migrated_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    /// Adjacency lookups (`list_edges_from/to`, `list_edges_by_kind`) and
    /// per-artifact evidence lookups must be index-backed; without these the
    /// search neighbour-boost / slice / impact fan-out degrade to full table
    /// scans of `edge_assertions` (the 230s multi-token `search` blow-up).
    #[test]
    fn migrate_creates_edge_and_evidence_adjacency_indexes() {
        let (store, _dir) = migrated_store();
        let names: Vec<String> = {
            let mut stmt = store
                .connection()
                .prepare("SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name")
                .unwrap();
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        for expected in [
            "idx_edge_assertions_from",
            "idx_edge_assertions_to",
            "idx_edge_assertions_kind",
            "idx_evidence_artifact",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing adjacency index `{expected}`; have {names:?}",
            );
        }
    }

    /// Migrations are idempotent: applying twice is a no-op, not an error.
    #[test]
    fn migrate_is_idempotent() {
        let (mut store, _dir) = migrated_store();
        store.migrate().unwrap();
    }
}
