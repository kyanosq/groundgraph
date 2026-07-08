//! `groundgraph export` behaviour.
//!
//! MVP-0 ships the JSONL backend. The export bundle lives at
//! `<repo_root>/.groundgraph/export/<table>.jsonl`. Each row is serialised as a
//! single JSON object on its own line. An empty graph still produces empty
//! `<table>.jsonl` files so downstream tools can rely on the layout.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use groundgraph_store::Store;
use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::config::{EngineConfig, DEFAULT_STORAGE_DIR};

#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    Jsonl,
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub repo_root: PathBuf,
    pub format: ExportFormat,
}

#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub bundle_dir: PathBuf,
    pub files: Vec<PathBuf>,
}

const EXPORTED_TABLES: &[&str] = &["nodes", "edge_assertions", "evidence"];

pub fn export(options: ExportOptions) -> Result<ExportOutcome> {
    let config = load_config(&options.repo_root)?;
    let db_path = resolve_storage_path(&options.repo_root, &config);

    let mut store = Store::open(&db_path)
        .with_context(|| format!("opening SQLite database at {}", db_path.display()))?;
    store
        .migrate()
        .with_context(|| format!("running migrations on {}", db_path.display()))?;

    let bundle_dir = options.repo_root.join(DEFAULT_STORAGE_DIR).join("export");
    std::fs::create_dir_all(&bundle_dir)
        .with_context(|| format!("creating export directory {}", bundle_dir.display()))?;

    let mut files = Vec::with_capacity(EXPORTED_TABLES.len());
    for table in EXPORTED_TABLES {
        let path = bundle_dir.join(format!("{table}.jsonl"));
        export_table(store.connection(), table, &path)
            .with_context(|| format!("exporting table {table} to {}", path.display()))?;
        files.push(path);
    }

    Ok(ExportOutcome { bundle_dir, files })
}

fn export_table(conn: &Connection, table: &str, dest: &Path) -> Result<()> {
    // Stream into a sibling temp file and rename at the end, so an
    // interrupted export can never leave a truncated `.jsonl` where a
    // previous good bundle stood (same policy as the CLI's write_atomic).
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temp file beside {}", dest.display()))?;
    let mut writer = BufWriter::new(tmp);

    let mut stmt = conn
        .prepare(&format!("SELECT * FROM {table}"))
        .with_context(|| format!("preparing select for table {table}"))?;
    let column_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let column_count = column_names.len();

    let mut rows = stmt
        .query([])
        .with_context(|| format!("running select for table {table}"))?;
    while let Some(row) = rows
        .next()
        .with_context(|| format!("reading row from table {table}"))?
    {
        let mut map = Map::with_capacity(column_count);
        for (idx, name) in column_names.iter().enumerate() {
            let value: rusqlite::types::Value = row
                .get::<_, rusqlite::types::Value>(idx)
                .with_context(|| format!("reading column {name} from {table}"))?;
            map.insert(name.clone(), sqlite_value_to_json(value));
        }
        let line = serde_json::to_string(&Value::Object(map)).context("serialising row to JSON")?;
        writeln!(writer, "{line}").context("writing JSONL line")?;
    }

    writer.flush().context("flushing JSONL writer")?;
    let tmp = writer
        .into_inner()
        .context("finalising buffered JSONL writer")?;
    tmp.persist(dest)
        .with_context(|| format!("moving export into place at {}", dest.display()))?;
    Ok(())
}

fn sqlite_value_to_json(value: rusqlite::types::Value) -> Value {
    use rusqlite::types::Value as Sql;
    match value {
        Sql::Null => Value::Null,
        Sql::Integer(i) => Value::from(i),
        Sql::Real(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Sql::Text(s) => Value::String(s),
        Sql::Blob(b) => Value::Array(b.into_iter().map(|byte| Value::from(byte as i64)).collect()),
    }
}

fn load_config(repo_root: &Path) -> Result<EngineConfig> {
    crate::config::load_config(repo_root)
}

fn resolve_storage_path(repo_root: &Path, config: &EngineConfig) -> PathBuf {
    crate::config::resolve_storage_path(repo_root, config)
}
