//! SQLite schema migrations.
//!
//! The migration list is ordered and append-only. Each entry is identified by
//! a stable integer `version`. `schema_version(version PRIMARY KEY, applied_at)`
//! records which migrations have run, making the apply step idempotent.

use rusqlite::{params, Connection};

use crate::{StoreError, StoreResult};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Migration {
    pub(crate) version: i32,
    pub(crate) sql: &'static str,
}

/// Full migration list for MVP-0. New schema changes append a new entry; do
/// not edit or remove existing entries — that would break already-initialised
/// repositories.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("./migrations_sql/001_initial.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("./migrations_sql/002_edge_indexes.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("./migrations_sql/003_fulltext.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("./migrations_sql/004_edge_order_indexes.sql"),
    },
];

pub(crate) fn apply_all(conn: &mut Connection) -> StoreResult<()> {
    apply_list(conn, MIGRATIONS)
}

/// Apply an explicit, ordered migration list. `apply_all` calls this with the
/// real [`MIGRATIONS`]; tests call it with synthetic lists (including a
/// deliberately-broken entry) to exercise the rollback / partial-apply paths
/// without mutating the shipping migration set (#235).
pub(crate) fn apply_list(conn: &mut Connection, migrations: &[Migration]) -> StoreResult<()> {
    // #202: store every timestamp as RFC3339 UTC (`…T…Z`), not
    // `datetime('now')`'s space-separated form. The default covers fresh DBs;
    // the INSERT below sets it explicitly so even a DB created by an older
    // binary (whose column default was the space form) records new rows in the
    // canonical format.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (\
            version INTEGER PRIMARY KEY,\
            applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))\
         )",
    )
    .map_err(|source| StoreError::Migration { version: 0, source })?;

    // Forward-compat guard: if this database records a version newer than the
    // newest migration this build knows, it was written by a future binary.
    // Its schema may have shapes (columns, tables) we don't understand, so a
    // downgraded binary must fail loudly instead of silently operating on a
    // future schema and corrupting data or mis-reading rows (#153).
    let supported = migrations.iter().map(|m| m.version).max().unwrap_or(0);
    let db_max: Option<i32> = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, Option<i32>>(0)
        })
        .map_err(|source| StoreError::Migration { version: 0, source })?;
    if let Some(found) = db_max {
        if found > supported {
            return Err(StoreError::SchemaTooNew { found, supported });
        }
    }

    for migration in migrations {
        let version = migration.version;
        // Fast path: skip versions already recorded without taking a write
        // lock, so read-mostly opens (and the common "nothing to do" case) stay
        // cheap.
        if version_applied(conn, version)? {
            continue;
        }

        // #216: take the write lock *before* applying. Two `groundgraph index`
        // processes opening the same fresh DB both pass the read-only check
        // above, then race on the `INSERT … schema_version`; with a plain
        // DEFERRED transaction the loser hit a PRIMARY KEY conflict (surfaced as
        // a `Migration` error) or re-ran the DDL. `BEGIN IMMEDIATE` acquires the
        // write lock up front, so the two are serialised by SQLite's
        // `busy_timeout`; whoever loses the race then re-checks inside the
        // transaction, sees the version already recorded, and skips — no
        // conflict, no double-applied DDL.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|source| StoreError::Migration { version, source })?;
        if version_applied(&tx, version)? {
            // A concurrent process applied it while we waited for the lock.
            // Nothing was written in this transaction, so a commit is a no-op.
            tx.commit()
                .map_err(|source| StoreError::Migration { version, source })?;
            continue;
        }
        tx.execute_batch(migration.sql)
            .map_err(|source| StoreError::Migration { version, source })?;
        tx.execute(
            "INSERT INTO schema_version(version, applied_at) \
             VALUES (?1, strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            params![version],
        )
        .map_err(|source| StoreError::Migration { version, source })?;
        tx.commit()
            .map_err(|source| StoreError::Migration { version, source })?;
    }

    Ok(())
}

/// Whether `version` is already recorded in `schema_version`. Takes anything
/// that derefs to a [`Connection`] (a borrowed connection or an open
/// `Transaction`) so the same check serves the lock-free fast path and the
/// in-transaction recheck (#216).
fn version_applied(conn: &Connection, version: i32) -> StoreResult<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_version WHERE version = ?1)",
        params![version],
        |row| row.get::<_, i32>(0).map(|v| v == 1),
    )
    .map_err(|source| StoreError::Migration { version, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn applied_versions(conn: &Connection) -> Vec<i32> {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_version ORDER BY version")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, i32>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            params![name],
            |r| r.get::<_, i32>(0).map(|v| v == 1),
        )
        .unwrap()
    }

    #[test]
    fn apply_list_rolls_back_failed_migration_and_does_not_advance_version() {
        // #235: a migration whose batch fails mid-way must (a) leave its own
        // partial DDL rolled back, (b) NOT record its version, and (c) keep the
        // previously-applied migration intact — each migration runs in its own
        // transaction.
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = [
            Migration {
                version: 1,
                sql: "CREATE TABLE alpha (id TEXT);",
            },
            Migration {
                version: 2,
                // First statement succeeds, second references a missing table
                // so `execute_batch` errors → the whole v2 transaction rolls
                // back (beta must not survive).
                sql: "CREATE TABLE beta (id TEXT); INSERT INTO does_not_exist VALUES (1);",
            },
        ];

        let err = apply_list(&mut conn, &migrations).expect_err("v2 must fail");
        assert!(
            matches!(err, StoreError::Migration { version: 2, .. }),
            "expected Migration{{version:2}}, got {err:?}"
        );

        // (b) only v1 is recorded; the failed v2 did not advance the version.
        assert_eq!(applied_versions(&conn), vec![1]);
        // (c) v1's table survives; (a) v2's partial table was rolled back.
        assert!(
            table_exists(&conn, "alpha"),
            "committed v1 table must survive"
        );
        assert!(
            !table_exists(&conn, "beta"),
            "failed v2 DDL must be rolled back, not left half-applied"
        );
    }

    #[test]
    fn apply_list_resumes_without_reapplying_already_applied_versions() {
        // #235: version-jump / resume. A DB stuck at version 1 (only 001
        // applied) reopened by a newer binary must apply 2 and 3 and skip 1 —
        // and a third pass must be a no-op (idempotent).
        let mut conn = Connection::open_in_memory().unwrap();

        apply_list(&mut conn, &MIGRATIONS[..1]).unwrap();
        assert_eq!(applied_versions(&conn), vec![1]);

        apply_list(&mut conn, MIGRATIONS).unwrap();
        assert_eq!(applied_versions(&conn), vec![1, 2, 3, 4]);
        assert!(
            table_exists(&conn, "node_fts"),
            "003 FTS table applied on resume"
        );

        // Idempotent third pass.
        apply_list(&mut conn, MIGRATIONS).unwrap();
        assert_eq!(applied_versions(&conn), vec![1, 2, 3, 4]);
    }

    /// #216: two processes migrating the *same fresh database* at once must not
    /// conflict. Each thread opens its own connection (with a busy_timeout, as
    /// `Store::open` does) and races into `apply_all` behind a barrier. The fix
    /// (`BEGIN IMMEDIATE` + in-transaction recheck) is timing-independent, so
    /// every round must end with exactly the full migration set applied once —
    /// no `PRIMARY KEY` conflict, no half-applied schema. Looped to widen the
    /// window; before the fix this raced to a `Migration` error.
    #[test]
    fn concurrent_apply_all_on_a_fresh_db_does_not_conflict() {
        use std::sync::{Arc, Barrier};
        use std::time::Duration;

        const THREADS: usize = 6;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("graph.db");

        for round in 0..40 {
            // Start each round from a clean database file.
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_file(db.with_extension("db-wal"));
            let _ = std::fs::remove_file(db.with_extension("db-shm"));

            let barrier = Arc::new(Barrier::new(THREADS));
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    let db = db.clone();
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        let mut conn = Connection::open(&db).unwrap();
                        conn.busy_timeout(Duration::from_secs(10)).unwrap();
                        barrier.wait();
                        apply_all(&mut conn)
                    })
                })
                .collect();

            for h in handles {
                let r = h.join().unwrap();
                assert!(
                    r.is_ok(),
                    "round {round}: concurrent migrate must not conflict: {r:?}"
                );
            }

            // `version` is the PRIMARY KEY, so a duplicate INSERT would already
            // have errored above; this confirms the full set landed exactly once
            // and the last migration's schema object exists.
            let conn = Connection::open(&db).unwrap();
            assert_eq!(applied_versions(&conn), vec![1, 2, 3, 4], "round {round}");
            assert!(table_exists(&conn, "node_fts"), "round {round}");
        }
    }

    /// `YYYY-MM-DDTHH:MM:SSZ` — RFC3339 UTC, dependency-free shape check.
    fn is_rfc3339_utc(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 20
            && b[4] == b'-'
            && b[7] == b'-'
            && b[10] == b'T'
            && b[13] == b':'
            && b[16] == b':'
            && b[19] == b'Z'
            && b.iter()
                .enumerate()
                .all(|(i, &c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || c.is_ascii_digit())
    }

    #[test]
    fn apply_list_records_applied_at_as_rfc3339_utc() {
        // #202: every timestamp the store writes is RFC3339 UTC (`…T…Z`).
        // `applied_at` must match, not `datetime('now')`'s space-separated
        // `YYYY-MM-DD HH:MM:SS`, so the schema speaks one time format.
        let mut conn = Connection::open_in_memory().unwrap();
        apply_list(&mut conn, MIGRATIONS).unwrap();

        let stamps: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT applied_at FROM schema_version ORDER BY version")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(
            stamps.len(),
            MIGRATIONS.len(),
            "one stamp per applied migration"
        );
        for s in &stamps {
            assert!(
                is_rfc3339_utc(s),
                "applied_at {s:?} must be RFC3339 UTC like 2026-06-13T12:00:00Z"
            );
        }
    }
}
