//! SpecSlice graph store (SQLite).
//!
//! MVP-0 surface:
//! - [`Store::open`] — create or open the SQLite database at the given path.
//! - [`Store::migrate`] — idempotently apply schema migrations.
//! - [`Store::connection`] — borrow the underlying connection (read access).

mod migrations;

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
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Handle to the SpecSlice SQLite graph store.
pub struct Store {
    conn: Connection,
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
