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
];

pub(crate) fn apply_all(conn: &mut Connection) -> StoreResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (\
            version INTEGER PRIMARY KEY,\
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))\
         )",
    )
    .map_err(|source| StoreError::Migration { version: 0, source })?;

    for migration in MIGRATIONS {
        let already_applied: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_version WHERE version = ?1)",
                params![migration.version],
                |row| row.get::<_, i32>(0).map(|v| v == 1),
            )
            .map_err(|source| StoreError::Migration {
                version: migration.version,
                source,
            })?;
        if already_applied {
            continue;
        }

        let tx = conn.transaction().map_err(|source| StoreError::Migration {
            version: migration.version,
            source,
        })?;
        tx.execute_batch(migration.sql)
            .map_err(|source| StoreError::Migration {
                version: migration.version,
                source,
            })?;
        tx.execute(
            "INSERT INTO schema_version(version) VALUES (?1)",
            params![migration.version],
        )
        .map_err(|source| StoreError::Migration {
            version: migration.version,
            source,
        })?;
        tx.commit().map_err(|source| StoreError::Migration {
            version: migration.version,
            source,
        })?;
    }

    Ok(())
}
