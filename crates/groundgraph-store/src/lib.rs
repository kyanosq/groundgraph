//! GroundGraph graph store (SQLite).
//!
//! MVP-0 surface:
//! - [`Store::open`] — create or open the SQLite database at the given path.
//! - [`Store::migrate`] — idempotently apply schema migrations.
//! - [`Store::connection`] — borrow the underlying connection (read access).

mod migrations;
mod repositories;

pub use repositories::{FileIndexEntry, FulltextHit, FulltextRow};

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

    #[error(
        "database schema version {found} is newer than this build supports (max {supported}); \
         upgrade groundgraph, or delete .groundgraph/graph.db to rebuild the index"
    )]
    SchemaTooNew { found: i32, supported: i32 },

    /// `SQLITE_BUSY` / `SQLITE_LOCKED`: another connection holds the write lock
    /// past `busy_timeout`. Transient — a caller may back off and retry (#215).
    #[error("database is busy (locked by another process): {0}")]
    Busy(#[source] rusqlite::Error),

    /// `SQLITE_CORRUPT` / `SQLITE_NOTADB`: the file is malformed. Not
    /// retryable — the (rebuildable) index must be deleted and re-indexed (#215).
    #[error("database file is corrupt; delete .groundgraph/graph.db and re-index: {0}")]
    Corrupt(#[source] rusqlite::Error),

    /// `SQLITE_READONLY` / `SQLITE_CANTOPEN` / `SQLITE_PERM` / `SQLITE_AUTH`:
    /// the process cannot write — a permission or mount problem, not transient (#215).
    #[error("database is read-only or inaccessible (check permissions): {0}")]
    ReadOnly(#[source] rusqlite::Error),

    /// `SQLITE_FULL`: the disk filled while writing (#215).
    #[error("disk is full while writing the database: {0}")]
    DiskFull(#[source] rusqlite::Error),

    // Catch-all. `{0}` inlines the rusqlite detail on purpose: decode failures
    // wrap a *meaningful* message (e.g. "unknown edge kind X") in a rusqlite
    // error, and bare `{}` formatting must surface it (see repositories
    // decode_tests). The mild anyhow `{:#}` double-print is the standard
    // thiserror trade-off and accepted here (#165). Operationally-actionable
    // result codes are split into the typed variants above (#215).
    #[error("sqlite error: {0}")]
    Sqlite(#[source] rusqlite::Error),
}

/// Operational class of a `rusqlite::Error`, decided by its primary result
/// code. Kept local (not part of the public surface) so the variant routing
/// lives in one place (#215).
enum SqliteKind {
    Busy,
    Corrupt,
    ReadOnly,
    DiskFull,
    Other,
}

/// Map a rusqlite error to its operational class. Only `SqliteFailure` (a real
/// SQLite result code) is classified; decode / type errors — which carry the
/// meaningful inline messages — stay `Other` so their text is preserved (#215).
fn classify_sqlite(err: &rusqlite::Error) -> SqliteKind {
    let rusqlite::Error::SqliteFailure(e, _) = err else {
        return SqliteKind::Other;
    };
    match e.code {
        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => SqliteKind::Busy,
        rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
            SqliteKind::Corrupt
        }
        rusqlite::ErrorCode::ReadOnly
        | rusqlite::ErrorCode::CannotOpen
        | rusqlite::ErrorCode::PermissionDenied
        | rusqlite::ErrorCode::AuthorizationForStatementDenied => SqliteKind::ReadOnly,
        rusqlite::ErrorCode::DiskFull => SqliteKind::DiskFull,
        _ => SqliteKind::Other,
    }
}

impl StoreError {
    /// Wrap a rusqlite error, routing operationally-distinct result codes into
    /// typed variants so callers can react (retry on [`Busy`](StoreError::Busy),
    /// stop+report on [`Corrupt`](StoreError::Corrupt), fix permissions on
    /// [`ReadOnly`](StoreError::ReadOnly), free space on
    /// [`DiskFull`](StoreError::DiskFull)) instead of scraping message strings (#215).
    pub(crate) fn sqlite(err: rusqlite::Error) -> Self {
        match classify_sqlite(&err) {
            SqliteKind::Busy => Self::Busy(err),
            SqliteKind::Corrupt => Self::Corrupt(err),
            SqliteKind::ReadOnly => Self::ReadOnly(err),
            SqliteKind::DiskFull => Self::DiskFull(err),
            SqliteKind::Other => Self::Sqlite(err),
        }
    }

    /// Whether retrying the operation might succeed. Only [`Busy`](StoreError::Busy)
    /// (contended lock) is transient; corruption, permissions and a full disk
    /// all need operator action first (#215).
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Busy(_))
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        // `?` conversions classify too, so a `BUSY`/`CORRUPT` surfaced via the
        // blanket `From` lands in the right variant, not the catch-all (#215).
        Self::sqlite(err)
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Handle to the GroundGraph SQLite graph store.
///
/// **Threading (#169)**: single-threaded *by design*, and the type system
/// already enforces it. `rusqlite::Connection` is `Send` but **not** `Sync`, so
/// `Store` is `Send + !Sync` (auto-derived). Because `Connection: !Sync`, a
/// `&Store` — and the `&Connection` handed out by [`Store::connection`] — is
/// itself `!Send`, so the compiler *rejects* sharing a store (or its raw
/// connection) across threads. There is no silent footgun: a store can be
/// *moved* to another thread (it is `Send`) but never *shared*. To use one from
/// several threads, wrap it in `Arc<Mutex<Store>>` — do **not** reach for
/// `unsafe impl Sync` (that would violate SQLite's single-handle contract).
pub struct Store {
    pub(crate) conn: Connection,
    path: PathBuf,
}

// #169: lock the documented threading contract in at compile time. `Store` must
// stay `Send` (movable across threads, the basis of the `Arc<Mutex<Store>>`
// recommendation); a regression that adds a non-`Send` field (e.g. an `Rc`)
// would fail to compile here rather than silently breaking downstream users.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    let _ = assert_send::<Store>;
};

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
        // cache_size/mmap_size: graphs for large repos reach hundreds of MB
        // (django: 229 MB). The SQLite default 2 MiB page cache thrashes —
        // profiles showed pread + BtreeTableMoveto dominating index time. A
        // 64 MiB cache plus a 256 MiB mmap window serves hot B-tree pages
        // from memory; both are per-connection settings, not persisted.
        //
        // Every pragma is a *performance* knob, not a correctness requirement:
        // SQLite stays fully functional under its defaults. Applied one by one,
        // best-effort, so an environment that rejects a single pragma (e.g.
        // mmap-restricted containers) degrades to a slower-but-correct store
        // instead of failing — and a failed pragma cannot skip the later ones
        // the way a single aborted batch did.
        for pragma in [
            "PRAGMA journal_mode=WAL;",
            "PRAGMA synchronous=NORMAL;",
            "PRAGMA busy_timeout=5000;",
            "PRAGMA cache_size=-65536;",
            "PRAGMA mmap_size=268435456;",
        ] {
            let _ = conn.execute_batch(pragma);
        }
        // The repository layer funnels every statement through
        // `prepare_cached`; the working set is a few dozen distinct SQL
        // strings, so re-parsing them per call (autocommit ingest does 10^5+
        // calls) is pure overhead.
        conn.set_prepared_statement_cache_capacity(64);
        let store = Self { conn, path };
        // Read-only commands open the store but never run `migrate()`. After a
        // binary upgrade adds adjacency indexes, those commands would scan
        // unindexed tables until the next `index`. Restore the perf indexes on
        // every open — idempotently, and only once the tables exist. WAL above
        // already requires a writable directory, so this adds no new constraint.
        store.ensure_query_indexes()?;
        Ok(store)
    }

    /// Idempotently (re)create the query-path performance indexes when the
    /// underlying tables already exist. Safe to call on every `open`: it is a
    /// no-op before the schema is migrated (no tables) and a cheap catalog
    /// check once the indexes are present. It never changes data shape — only
    /// migrations do that — so it is safe on read-only command paths.
    pub fn ensure_query_indexes(&self) -> StoreResult<()> {
        if !self.table_exists("edge_assertions")? {
            return Ok(());
        }
        self.conn
            .execute_batch(include_str!("./migrations_sql/query_indexes.sql"))
            .map_err(StoreError::sqlite)
    }

    fn table_exists(&self, name: &str) -> StoreResult<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Idempotently apply all schema migrations.
    pub fn migrate(&mut self) -> StoreResult<()> {
        migrations::apply_all(&mut self.conn)
    }

    /// Open a bulk write session: one explicit transaction covering an entire
    /// ingest. In autocommit mode every upsert is its own WAL commit; an
    /// indexing run does 10^5+ of them, and the per-commit lock/WAL-frame
    /// churn dominated large-repo profiles. Idempotent — calling inside an
    /// open session is a no-op, so nested scopes compose.
    pub fn begin_bulk(&self) -> StoreResult<()> {
        if self.conn.is_autocommit() {
            // Suspend WAL auto-checkpointing for the session: a full reindex
            // writes hundreds of MB of WAL frames, and the default 1000-page
            // threshold checkpoints mid-transaction — copying frames into the
            // main db while still appending more (double IO; spring profiles
            // showed pwrite dominating). One explicit TRUNCATE checkpoint at
            // commit writes everything exactly once and resets the WAL file.
            self.conn
                .execute_batch("PRAGMA wal_autocheckpoint=0; BEGIN IMMEDIATE")
                .map_err(StoreError::sqlite)?;
        }
        Ok(())
    }

    /// Commit the bulk session opened by [`Store::begin_bulk`]. No-op when no
    /// session is open. If the process dies before this, SQLite rolls the
    /// open transaction back on connection close — the index is rebuildable,
    /// so losing an unfinished ingest is the correct outcome.
    pub fn commit_bulk(&self) -> StoreResult<()> {
        if !self.conn.is_autocommit() {
            // COMMIT alone decides success. The two PRAGMAs after it are
            // housekeeping: if the checkpoint fails (disk pressure, a
            // concurrent reader pinning the WAL) the data is still safely
            // committed — reporting that as an error would make callers
            // re-run a whole ingest for nothing, and bailing between the
            // statements used to leave `wal_autocheckpoint=0` behind so
            // the WAL grew without bound (issues2.md #55).
            if let Err(e) = self.conn.execute_batch("COMMIT") {
                // COMMIT failed (e.g. SQLITE_BUSY): SQLite leaves the
                // transaction *active*, so the connection is still non-
                // autocommit. Without an explicit ROLLBACK the next
                // `begin_bulk` would see `is_autocommit() == false`, skip its
                // `BEGIN IMMEDIATE`, and silently append the next ingest into
                // this dead transaction — corrupting the commit/rollback
                // boundary (#254). Roll back to return the connection to
                // autocommit; then restore `wal_autocheckpoint` (suspended in
                // begin_bulk) so the WAL can't grow unbounded (the failure twin
                // of issues2.md #55, #218). Both are best-effort — the original
                // COMMIT error is what we report.
                let _ = self.conn.execute_batch("ROLLBACK");
                let _ = self.conn.execute_batch("PRAGMA wal_autocheckpoint=1000;");
                return Err(StoreError::sqlite(e));
            }
            let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            let _ = self.conn.execute_batch("PRAGMA wal_autocheckpoint=1000;");
        }
        Ok(())
    }

    /// Abort the bulk session, discarding all its writes. No-op outside a
    /// session, so error paths can call it unconditionally.
    pub fn rollback_bulk(&self) -> StoreResult<()> {
        if !self.conn.is_autocommit() {
            self.conn
                .execute_batch("ROLLBACK")
                .map_err(StoreError::sqlite)?;
        }
        Ok(())
    }

    /// Run `f` inside a write transaction. When a bulk session (or any outer
    /// transaction) is already open, joins it instead of nesting — SQLite has
    /// no nested BEGIN. Otherwise opens its own transaction; on error the
    /// `Transaction` guard rolls back on drop.
    pub(crate) fn with_write_tx<T>(
        &mut self,
        f: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> StoreResult<T> {
        if !self.conn.is_autocommit() {
            return f(&self.conn);
        }
        let tx = self.conn.transaction().map_err(StoreError::sqlite)?;
        let out = f(&tx)?;
        tx.commit().map_err(StoreError::sqlite)?;
        Ok(out)
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
            // #140: composite `(<col>, id)` covering indexes (the old
            // single-column `idx_edge_assertions_{from,to,kind}` were renamed and
            // dropped by migration 004).
            "idx_edge_assertions_from_ord",
            "idx_edge_assertions_to_ord",
            "idx_edge_assertions_kind_ord",
            "idx_evidence_artifact",
            // Ingest-path indexes: SCIP suppression deletes per source_file
            // (django: 3026 files × full scan of 96k edges dominated the
            // profile) and clear_indexer_outputs deletes per indexer.
            "idx_edge_assertions_source_file",
            "idx_edge_assertions_indexer",
            "idx_nodes_indexer",
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

    /// Read-only commands (`search`/`slice`/`dead-code`/…) open the store but
    /// never call `migrate()`. After a binary upgrade that adds adjacency
    /// indexes, those commands must still get fast, index-backed queries — so
    /// `open` itself idempotently restores the perf indexes when the tables
    /// already exist. Here we drop the indexes (simulating a pre-002 DB) and
    /// assert a plain `open` brings them back, without a migration.
    #[test]
    fn open_restores_query_indexes_for_a_pre_index_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let (store, _d) = {
                let mut s = Store::open(&path).unwrap();
                s.migrate().unwrap();
                (s, ())
            };
            store
                .connection()
                .execute_batch(
                    "DROP INDEX idx_edge_assertions_from_ord;\
                     DROP INDEX idx_edge_assertions_to_ord;\
                     DROP INDEX idx_edge_assertions_kind_ord;\
                     DROP INDEX idx_evidence_artifact;",
                )
                .unwrap();
        }
        // A read-command path: open only, no migrate.
        let store = Store::open(&path).unwrap();
        let count: i64 = store
            .connection()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' \
                 AND name IN ('idx_edge_assertions_from_ord','idx_edge_assertions_to_ord',\
                 'idx_edge_assertions_kind_ord','idx_evidence_artifact')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 4,
            "open must restore all 4 perf indexes for read commands"
        );
    }

    /// `open` runs before `migrate` on first init, when no tables exist yet —
    /// ensuring indexes must be a safe no-op, never an error.
    #[test]
    fn open_on_tableless_db_is_a_noop_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Store::open(dir.path().join("fresh.db")).is_ok());
    }

    /// `commit_bulk` must always restore `wal_autocheckpoint` and leave
    /// the connection in autocommit, whatever happens to the optional
    /// checkpoint housekeeping (issues2.md #55).
    #[test]
    fn commit_bulk_restores_autocheckpoint_and_autocommit() {
        let (store, _dir) = migrated_store();
        store.begin_bulk().unwrap();
        let mid: i64 = store
            .connection()
            .query_row("PRAGMA wal_autocheckpoint", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mid, 0, "bulk session suspends autocheckpoint");
        store.commit_bulk().unwrap();
        assert!(store.connection().is_autocommit());
        let after: i64 = store
            .connection()
            .query_row("PRAGMA wal_autocheckpoint", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 1000, "autocheckpoint must come back after commit");
    }
}
