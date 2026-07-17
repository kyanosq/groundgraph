//! Repository helpers built on top of [`Store`].
//!
//! All operations are idempotent via `INSERT ... ON CONFLICT DO UPDATE`. The
//! conflict targets always match the schema's primary keys.

use groundgraph_core::{
    ArtifactId, Confidence, EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus,
    Evidence, EvidenceKind, Node, NodeKind, SymbolRange,
};
use rusqlite::{params, params_from_iter, Row};
use serde::{Deserialize, Serialize};

use crate::{Store, StoreError, StoreResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIndexEntry {
    pub path: String,
    pub hash: String,
    pub kind: String,
    pub indexed_at: String,
    pub index_generation: i64,
}

// Macro (not `const`) so `concat!` can fold the column list into the full
// read SQL at compile time — `find_node` runs thousands of times per BFS, and
// `format!`-building the same string each call is pure allocation (issues3.md
// #157).
macro_rules! select_node_cols {
    () => {
        "id, kind, path, name, start_line, end_line, content_hash, stable_key, source_file, indexer, metadata_json"
    };
}
const FIND_NODE_SQL: &str = concat!("SELECT ", select_node_cols!(), " FROM nodes WHERE id = ?1");
const LIST_NODES_BY_KIND_SQL: &str = concat!(
    "SELECT ",
    select_node_cols!(),
    " FROM nodes WHERE kind = ?1 ORDER BY id"
);
const LIST_ALL_NODES_SQL: &str = concat!("SELECT ", select_node_cols!(), " FROM nodes ORDER BY id");

const SELECT_EDGE_COLS: &str = "id, from_id, to_id, kind, source, certainty, status, confidence, evidence_json, source_file, indexer, metadata_json";

const SELECT_EVIDENCE_COLS: &str =
    "id, artifact_id, kind, path, start_line, end_line, snippet, hash, metadata_json";

const SELECT_RANGE_COLS: &str =
    "file_path, symbol_id, start_line, end_line, symbol_kind, qualified_name, parent_symbol_id";

/// #205: anti-downgrade guard applied to `edge_assertions.certainty` on UPSERT
/// conflict. An incoming `fact` always wins (upgrade, or same-level refresh);
/// an incoming `declared` only refreshes when the existing row is also
/// `declared` — it must NOT overwrite an existing `fact`. The two values
/// (`'declared'` < `'fact'`) happen to sort in semantic order, but the CASE
/// states the rule explicitly instead of leaning on that coincidence.
const CERTAINTY_CASE: &str = "CASE WHEN excluded.certainty='fact' OR edge_assertions.certainty='declared' THEN excluded.certainty ELSE edge_assertions.certainty END";

/// #205: `confidence` rides the *same* condition as `certainty`. They are the
/// pair that says "how sure we are about this assertion", so when a no-downgrade
/// keeps the existing certainty it must also keep its confidence — otherwise a
/// row could end up `fact`-certainty carrying a `declared`-grade score. The
/// other SET columns (status / source_file / indexer / metadata_json) are
/// provenance independent of certainty and refresh normally.
const CONFIDENCE_CASE: &str = "CASE WHEN excluded.certainty='fact' OR edge_assertions.certainty='declared' THEN excluded.confidence ELSE edge_assertions.confidence END";

/// Owned-`Value` constructors for the multi-row upsert paths, where borrowed
/// `ToSql` references cannot outlive the per-row temporaries.
fn val_text(s: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(s.to_owned())
}

fn val_opt_text(o: &Option<String>) -> rusqlite::types::Value {
    match o {
        Some(s) => rusqlite::types::Value::Text(s.clone()),
        None => rusqlite::types::Value::Null,
    }
}

fn val_opt_u32(o: Option<u32>) -> rusqlite::types::Value {
    match o {
        Some(v) => rusqlite::types::Value::Integer(i64::from(v)),
        None => rusqlite::types::Value::Null,
    }
}

/// `(?1, ?2, …), (?8, ?9, …), …` placeholder text for a multi-row VALUES.
fn values_placeholders(rows: usize, cols: usize) -> String {
    let groups: Vec<String> = (0..rows)
        .map(|i| {
            let base = i * cols;
            let nums: Vec<String> = (1..=cols).map(|c| format!("?{}", base + c)).collect();
            format!("({})", nums.join(", "))
        })
        .collect();
    groups.join(", ")
}

/// Execute one multi-row VALUES chunk. Full-size chunks share a single SQL
/// shape per table and go through the statement cache; tail chunks (up to
/// 511 distinct shapes) are prepared uncached so they cannot evict the hot
/// cached statements (the cache only holds 64 entries).
fn execute_chunk(
    conn: &rusqlite::Connection,
    sql: &str,
    values: Vec<rusqlite::types::Value>,
    cacheable: bool,
) -> Result<(), StoreError> {
    if cacheable {
        conn.prepare_cached(sql)
            .map_err(StoreError::sqlite)?
            .execute(rusqlite::params_from_iter(values))
            .map(|_| ())
            .map_err(StoreError::sqlite)
    } else {
        conn.prepare(sql)
            .map_err(StoreError::sqlite)?
            .execute(rusqlite::params_from_iter(values))
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }
}

impl Store {
    pub fn upsert_node(&mut self, node: &Node) -> StoreResult<()> {
        // #168: enforce node invariants at the write boundary — the all-pub
        // struct fields cannot express them by type.
        node.validate().map_err(StoreError::InvalidNode)?;
        let sql = "INSERT INTO nodes (id, kind, path, name, start_line, end_line, content_hash, stable_key, source_file, indexer, metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, path=excluded.path, name=excluded.name, start_line=excluded.start_line, end_line=excluded.end_line, content_hash=excluded.content_hash, stable_key=excluded.stable_key, source_file=excluded.source_file, indexer=excluded.indexer, metadata_json=excluded.metadata_json";
        self.conn
            .prepare_cached(sql)
            .map_err(StoreError::sqlite)?
            .execute(params![
                node.id.as_str(),
                node.kind.as_str(),
                node.path,
                node.name,
                node.start_line,
                node.end_line,
                node.content_hash,
                node.stable_key,
                node.source_file,
                node.indexer,
                node.metadata_json,
            ])
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn find_node(&self, id: &ArtifactId) -> StoreResult<Option<Node>> {
        // Read paths use `prepare_cached` too: `search` fan-out calls
        // `find_node` / `list_edges_from/to` thousands of times per query,
        // and re-parsing the SQL each call is pure overhead.
        let mut stmt = self
            .conn
            .prepare_cached(FIND_NODE_SQL)
            .map_err(StoreError::sqlite)?;
        let mut rows = stmt
            .query(params![id.as_str()])
            .map_err(StoreError::sqlite)?;
        if let Some(row) = rows.next().map_err(StoreError::sqlite)? {
            Ok(Some(node_from_row(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_nodes_by_kind(&self, kind: NodeKind) -> StoreResult<Vec<Node>> {
        let mut stmt = self
            .conn
            .prepare_cached(LIST_NODES_BY_KIND_SQL)
            .map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(params![kind.as_str()], node_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    pub fn list_all_nodes(&self) -> StoreResult<Vec<Node>> {
        let mut stmt = self
            .conn
            .prepare_cached(LIST_ALL_NODES_SQL)
            .map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map([], node_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    /// Multi-row upsert: one `INSERT … VALUES (…),(…) ON CONFLICT DO UPDATE`
    /// per chunk instead of one VDBE dispatch per row. Semantically identical
    /// to calling [`Self::upsert_node`] per element — this is the ingest hot
    /// path for tree-sitter batches (spring-framework: 84k symbol rows).
    ///
    /// Runs inside a write transaction: joins the caller's bulk session when
    /// one is open, otherwise opens its own — so a mid-batch failure can
    /// never leave a partially-committed batch behind.
    pub fn upsert_nodes_bulk(&mut self, nodes: &[Node]) -> StoreResult<()> {
        // #168: same write-boundary invariant check as `upsert_node`, done up
        // front so a bad node fails the whole batch before any statement runs.
        for node in nodes {
            node.validate().map_err(StoreError::InvalidNode)?;
        }
        // 11 columns × 512 rows = 5632 bound variables, far under SQLite's
        // 32k limit while keeping the hot (full-chunk) statement cacheable.
        const CHUNK: usize = 512;
        const COLS: usize = 11;
        self.with_write_tx(|conn| {
            for chunk in nodes.chunks(CHUNK) {
                let sql = format!(
                    "INSERT INTO nodes (id, kind, path, name, start_line, end_line, content_hash, stable_key, source_file, indexer, metadata_json) VALUES {} ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, path=excluded.path, name=excluded.name, start_line=excluded.start_line, end_line=excluded.end_line, content_hash=excluded.content_hash, stable_key=excluded.stable_key, source_file=excluded.source_file, indexer=excluded.indexer, metadata_json=excluded.metadata_json",
                    values_placeholders(chunk.len(), COLS)
                );
                let mut values: Vec<rusqlite::types::Value> =
                    Vec::with_capacity(chunk.len() * COLS);
                for node in chunk {
                    values.push(val_text(node.id.as_str()));
                    values.push(val_text(node.kind.as_str()));
                    values.push(val_opt_text(&node.path));
                    values.push(val_opt_text(&node.name));
                    values.push(val_opt_u32(node.start_line));
                    values.push(val_opt_u32(node.end_line));
                    values.push(val_opt_text(&node.content_hash));
                    values.push(val_opt_text(&node.stable_key));
                    values.push(val_opt_text(&node.source_file));
                    values.push(val_opt_text(&node.indexer));
                    values.push(val_opt_text(&node.metadata_json));
                }
                execute_chunk(conn, &sql, values, chunk.len() == CHUNK)?;
            }
            Ok(())
        })
    }

    /// Multi-row edge upsert; see [`Self::upsert_nodes_bulk`].
    pub fn upsert_edges_bulk(&mut self, edges: &[EdgeAssertion]) -> StoreResult<()> {
        const CHUNK: usize = 512;
        const COLS: usize = 12;
        self.with_write_tx(|conn| {
            for chunk in edges.chunks(CHUNK) {
                let sql = format!(
                    "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence, evidence_json, source_file, indexer, metadata_json) VALUES {} ON CONFLICT(id) DO UPDATE SET from_id=excluded.from_id, to_id=excluded.to_id, kind=excluded.kind, source=excluded.source, certainty={CERTAINTY_CASE}, status=excluded.status, confidence={CONFIDENCE_CASE}, evidence_json=excluded.evidence_json, source_file=excluded.source_file, indexer=excluded.indexer, metadata_json=excluded.metadata_json",
                    values_placeholders(chunk.len(), COLS)
                );
                let mut values: Vec<rusqlite::types::Value> =
                    Vec::with_capacity(chunk.len() * COLS);
                for edge in chunk {
                    values.push(val_text(edge.id.as_str()));
                    values.push(val_text(edge.from_id.as_str()));
                    values.push(val_text(edge.to_id.as_str()));
                    values.push(val_text(edge.kind.as_str()));
                    values.push(val_text(edge.source.as_str()));
                    values.push(val_text(edge.certainty.as_str()));
                    values.push(val_text(edge.status.as_str()));
                    values.push(rusqlite::types::Value::Real(f64::from(edge.confidence)));
                    values.push(val_opt_text(&edge.evidence_json));
                    values.push(val_opt_text(&edge.source_file));
                    values.push(val_opt_text(&edge.indexer));
                    values.push(val_opt_text(&edge.metadata_json));
                }
                execute_chunk(conn, &sql, values, chunk.len() == CHUNK)?;
            }
            Ok(())
        })
    }

    pub fn upsert_edge(&mut self, edge: &EdgeAssertion) -> StoreResult<()> {
        let sql = format!(
            "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence, evidence_json, source_file, indexer, metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) ON CONFLICT(id) DO UPDATE SET from_id=excluded.from_id, to_id=excluded.to_id, kind=excluded.kind, source=excluded.source, certainty={CERTAINTY_CASE}, status=excluded.status, confidence={CONFIDENCE_CASE}, evidence_json=excluded.evidence_json, source_file=excluded.source_file, indexer=excluded.indexer, metadata_json=excluded.metadata_json"
        );
        self.conn
            .prepare_cached(&sql)
            .map_err(StoreError::sqlite)?
            .execute(params![
                edge.id.as_str(),
                edge.from_id.as_str(),
                edge.to_id.as_str(),
                edge.kind.as_str(),
                edge.source.as_str(),
                edge.certainty.as_str(),
                edge.status.as_str(),
                f64::from(edge.confidence),
                edge.evidence_json,
                edge.source_file,
                edge.indexer,
                edge.metadata_json,
            ])
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn list_edges_by_kind(&self, kind: EdgeKind) -> StoreResult<Vec<EdgeAssertion>> {
        self.query_edges("WHERE kind = ?1 ORDER BY id", params![kind.as_str()])
    }

    /// All edges whose `kind` is in `kinds`, ordered by id. Used by `connect`
    /// to collapse an N+1 (one `list_edges_to` per requirement node, issues.md
    /// #158) into a single query the caller buckets in memory by `to_id`.
    ///
    /// The prepared SQL is keyed by placeholder count, so a fixed `kinds`
    /// length (e.g. `connect`'s three evidence kinds) reuses one cached
    /// statement instead of recompiling per call.
    pub fn list_edges_by_kinds(&self, kinds: &[EdgeKind]) -> StoreResult<Vec<EdgeAssertion>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: String = (1..=kinds.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let suffix = format!("WHERE kind IN ({placeholders}) ORDER BY id");
        let params = params_from_iter(kinds.iter().map(|k| k.as_str()));
        self.query_edges(&suffix, params)
    }

    pub fn list_edges_from(&self, from: &ArtifactId) -> StoreResult<Vec<EdgeAssertion>> {
        self.query_edges("WHERE from_id = ?1 ORDER BY id", params![from.as_str()])
    }

    pub fn list_edges_to(&self, to: &ArtifactId) -> StoreResult<Vec<EdgeAssertion>> {
        self.query_edges("WHERE to_id = ?1 ORDER BY id", params![to.as_str()])
    }

    pub fn list_all_edges(&self) -> StoreResult<Vec<EdgeAssertion>> {
        self.query_edges("ORDER BY id", params![])
    }

    fn query_edges(
        &self,
        suffix: &str,
        params: impl rusqlite::Params,
    ) -> StoreResult<Vec<EdgeAssertion>> {
        // Only four fixed suffixes exist (`from`/`to`/`kind`/all), so the
        // cache holds at most four shapes — safe to cache.
        let sql = format!("SELECT {SELECT_EDGE_COLS} FROM edge_assertions {suffix}");
        let mut stmt = self.conn.prepare_cached(&sql).map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(params, edge_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    pub fn upsert_evidence(&mut self, ev: &Evidence) -> StoreResult<()> {
        let sql = "INSERT INTO evidence (id, artifact_id, kind, path, start_line, end_line, snippet, hash, metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(id) DO UPDATE SET artifact_id=excluded.artifact_id, kind=excluded.kind, path=excluded.path, start_line=excluded.start_line, end_line=excluded.end_line, snippet=excluded.snippet, hash=excluded.hash, metadata_json=excluded.metadata_json";
        self.conn
            .prepare_cached(sql)
            .map_err(StoreError::sqlite)?
            .execute(params![
                ev.id.as_str(),
                ev.artifact_id.as_str(),
                ev.kind.as_str(),
                ev.path,
                ev.start_line,
                ev.end_line,
                ev.snippet,
                ev.hash,
                ev.metadata_json,
            ])
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn list_evidence_for_artifact(&self, artifact: &ArtifactId) -> StoreResult<Vec<Evidence>> {
        let sql = format!(
            "SELECT {SELECT_EVIDENCE_COLS} FROM evidence WHERE artifact_id = ?1 ORDER BY id"
        );
        let mut stmt = self.conn.prepare_cached(&sql).map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(params![artifact.as_str()], evidence_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    /// Multi-row symbol-range upsert; see [`Self::upsert_nodes_bulk`].
    pub fn upsert_symbol_ranges_bulk(&mut self, ranges: &[SymbolRange]) -> StoreResult<()> {
        const CHUNK: usize = 512;
        const COLS: usize = 7;
        self.with_write_tx(|conn| {
            for chunk in ranges.chunks(CHUNK) {
                let sql = format!(
                    "INSERT INTO symbol_ranges (file_path, symbol_id, start_line, end_line, symbol_kind, qualified_name, parent_symbol_id) VALUES {} ON CONFLICT(file_path, symbol_id) DO UPDATE SET start_line=excluded.start_line, end_line=excluded.end_line, symbol_kind=excluded.symbol_kind, qualified_name=excluded.qualified_name, parent_symbol_id=excluded.parent_symbol_id",
                    values_placeholders(chunk.len(), COLS)
                );
                let mut values: Vec<rusqlite::types::Value> =
                    Vec::with_capacity(chunk.len() * COLS);
                for range in chunk {
                    values.push(val_text(&range.file_path));
                    values.push(val_text(range.symbol_id.as_str()));
                    values.push(rusqlite::types::Value::Integer(i64::from(
                        range.start_line,
                    )));
                    values.push(rusqlite::types::Value::Integer(i64::from(range.end_line)));
                    values.push(val_text(range.symbol_kind.as_str()));
                    values.push(val_text(&range.qualified_name));
                    values.push(match &range.parent_symbol_id {
                        Some(id) => val_text(id.as_str()),
                        None => rusqlite::types::Value::Null,
                    });
                }
                execute_chunk(conn, &sql, values, chunk.len() == CHUNK)?;
            }
            Ok(())
        })
    }

    pub fn upsert_symbol_range(&mut self, range: &SymbolRange) -> StoreResult<()> {
        let sql = "INSERT INTO symbol_ranges (file_path, symbol_id, start_line, end_line, symbol_kind, qualified_name, parent_symbol_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(file_path, symbol_id) DO UPDATE SET start_line=excluded.start_line, end_line=excluded.end_line, symbol_kind=excluded.symbol_kind, qualified_name=excluded.qualified_name, parent_symbol_id=excluded.parent_symbol_id";
        self.conn
            .prepare_cached(sql)
            .map_err(StoreError::sqlite)?
            .execute(params![
                range.file_path,
                range.symbol_id.as_str(),
                range.start_line,
                range.end_line,
                range.symbol_kind.as_str(),
                range.qualified_name,
                range
                    .parent_symbol_id
                    .as_ref()
                    .map(|id| id.as_str().to_string()),
            ])
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn list_symbol_ranges_for_file(&self, file_path: &str) -> StoreResult<Vec<SymbolRange>> {
        let sql = format!("SELECT {SELECT_RANGE_COLS} FROM symbol_ranges WHERE file_path = ?1 ORDER BY start_line, symbol_id");
        let mut stmt = self.conn.prepare_cached(&sql).map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(params![file_path], symbol_range_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    /// Symbols whose `[start_line, end_line]` intersects `[start, end]`.
    pub fn find_symbols_intersecting(
        &self,
        file_path: &str,
        start: u32,
        end: u32,
    ) -> StoreResult<Vec<SymbolRange>> {
        // Named params: the intersection mixes `start`/`end` across the two
        // columns (`start_line <= :end AND end_line >= :start`), so positional
        // `?2`/`?3` were easy to transpose by accident (#178).
        let sql = format!(
            "SELECT {SELECT_RANGE_COLS} FROM symbol_ranges \
             WHERE file_path = :file AND start_line <= :end AND end_line >= :start \
             ORDER BY start_line, end_line, symbol_id"
        );
        let mut stmt = self.conn.prepare_cached(&sql).map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(
                rusqlite::named_params! { ":file": file_path, ":start": start, ":end": end },
                symbol_range_from_row,
            )
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    pub fn upsert_file_index(&mut self, entry: &FileIndexEntry) -> StoreResult<()> {
        let sql = "INSERT INTO file_index (path, hash, kind, indexed_at, index_generation) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(path) DO UPDATE SET hash=excluded.hash, kind=excluded.kind, indexed_at=excluded.indexed_at, index_generation=excluded.index_generation";
        self.conn
            .prepare_cached(sql)
            .map_err(StoreError::sqlite)?
            .execute(params![
                entry.path,
                entry.hash,
                entry.kind,
                entry.indexed_at,
                entry.index_generation,
            ])
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn get_file_hash(&self, path: &str) -> StoreResult<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT hash FROM file_index WHERE path = ?1")
            .map_err(StoreError::sqlite)?;
        let mut rows = stmt.query(params![path]).map_err(StoreError::sqlite)?;
        if let Some(row) = rows.next().map_err(StoreError::sqlite)? {
            Ok(Some(row.get::<_, String>(0).map_err(StoreError::sqlite)?))
        } else {
            Ok(None)
        }
    }

    /// Delete the `nodes` and `edge_assertions` rows produced by the given
    /// indexer name. Used to re-index without leaving that indexer's stale rows
    /// behind.
    ///
    /// Scoped to the indexer's own rows only: `evidence` / `symbol_ranges` /
    /// `node_fts` carry no `indexer` column, so their orphan cleanup is deferred
    /// to [`Self::sweep_orphans`], which the ingest flow runs once at the end
    /// (#137) instead of re-anti-joining the whole `nodes` table on every
    /// per-indexer clear.
    ///
    /// Rows with `indexer IS NULL` are deliberately untouched: NULL marks
    /// unmanaged data (operator-confirmed links, external imports) that no
    /// indexer owns, so no indexer's re-run may delete it. Every built-in
    /// indexer tags its own writes — see e.g. `make_indexed_edge` in the
    /// docs indexer.
    pub fn clear_indexer_outputs(&mut self, indexer: &str) -> StoreResult<()> {
        self.with_write_tx(|tx| {
            tx.execute("DELETE FROM nodes WHERE indexer = ?1", params![indexer])
                .map_err(StoreError::sqlite)?;
            tx.execute(
                "DELETE FROM edge_assertions WHERE indexer = ?1",
                params![indexer],
            )
            .map_err(StoreError::sqlite)?;
            Ok(())
        })
    }

    /// #137: the single self-healing GC pass — reclaim every `evidence` /
    /// `symbol_ranges` / `node_fts` row whose referenced id no longer exists in
    /// `nodes`. This used to run inside `clear_indexer_outputs`, so an ingest
    /// that clears N indexers walked the whole `nodes` table N times; it now
    /// runs once at the end of each ingest entry (`index_repository` and
    /// `index_schema_into`). The scope is deliberately the same whole-table
    /// `NOT IN` sweep it always was — it doubles as self-healing cleanup of
    /// rows orphaned by a historical crash, so it must NOT be narrowed to the
    /// ids a single indexer just deleted. Returns the number of rows removed.
    pub fn sweep_orphans(&mut self) -> StoreResult<usize> {
        // `node_fts` only exists after migration 003; older databases skip its
        // sweep (they have no content layer to go stale).
        let has_fts = self.fulltext_available()?;
        self.with_write_tx(|tx| {
            let mut removed = 0usize;
            removed += tx
                .execute(
                    "DELETE FROM evidence WHERE artifact_id NOT IN (SELECT id FROM nodes)",
                    params![],
                )
                .map_err(StoreError::sqlite)?;
            removed += tx
                .execute(
                    "DELETE FROM symbol_ranges WHERE symbol_id NOT IN (SELECT id FROM nodes)",
                    params![],
                )
                .map_err(StoreError::sqlite)?;
            // Fulltext rows referencing deleted nodes would otherwise survive
            // (`node_id` is UNINDEXED, no FK) and `fulltext_match` would keep
            // returning ghost ids that `find_node` cannot resolve.
            if has_fts {
                removed += tx
                    .execute(
                        "DELETE FROM node_fts WHERE node_id NOT IN (SELECT id FROM nodes)",
                        params![],
                    )
                    .map_err(StoreError::sqlite)?;
            }
            Ok(removed)
        })
    }

    /// SCIP-authoritative suppression. For each `source_file` SCIP covered,
    /// delete its `Calls`/`References` edges whose `indexer` differs from
    /// `keep_indexer` (i.e. the heuristic/LSP precision edges), so a covered
    /// file carries exactly one precision source — its SCIP edges. Files SCIP
    /// did not cover are left untouched and keep their heuristic gap-fill.
    /// Structural edges (`Contains`/`Imports`/…) are never removed. Returns the
    /// number of edges deleted.
    pub fn delete_precision_edges_for_files_except(
        &mut self,
        source_files: &[String],
        keep_indexer: &str,
    ) -> StoreResult<usize> {
        if source_files.is_empty() {
            return Ok(0);
        }
        let calls = EdgeKind::Calls.as_str();
        let references = EdgeKind::References.as_str();
        self.with_write_tx(|tx| {
            let mut removed = 0usize;
            // `prepare_cached`: SCIP suppression calls this once per source file
            // (django: 3026), and the SQL is a single literal — re-parsing it
            // each call bypasses the 64-entry statement cache the rest of the
            // ingest path relies on (issues3.md #136).
            let mut stmt = tx
                .prepare_cached(
                    "DELETE FROM edge_assertions \
                     WHERE source_file = ?1 \
                       AND kind IN (?2, ?3) \
                       AND (indexer IS NULL OR indexer != ?4)",
                )
                .map_err(StoreError::sqlite)?;
            for file in source_files {
                removed += stmt
                    .execute(params![file, calls, references, keep_indexer])
                    .map_err(StoreError::sqlite)?;
            }
            Ok(removed)
        })
    }

    // -----------------------------------------------------------------------
    // Fulltext (FTS5) content layer
    // -----------------------------------------------------------------------

    /// Whether the FTS5 content table exists. Databases created before
    /// migration 003 (or opened without `migrate()`) lack it; callers degrade
    /// to structural-only search instead of erroring.
    pub fn fulltext_available(&self) -> StoreResult<bool> {
        self.table_exists("node_fts")
    }

    /// Replace the entire fulltext index with `rows`. Called once per
    /// `groundgraph index` run after every structural pass, so the content layer
    /// always mirrors the current node set — no per-indexer ownership needed.
    pub fn rebuild_fulltext(&mut self, rows: &[FulltextRow]) -> StoreResult<usize> {
        self.with_write_tx(|tx| {
            tx.execute("DELETE FROM node_fts", [])
                .map_err(StoreError::sqlite)?;
            let mut inserted = 0usize;
            let mut stmt = tx
                .prepare_cached("INSERT INTO node_fts (node_id, body) VALUES (?1, ?2)")
                .map_err(StoreError::sqlite)?;
            for row in rows {
                if row.body.trim().is_empty() {
                    continue;
                }
                stmt.execute(params![row.node_id, row.body])
                    .map_err(StoreError::sqlite)?;
                inserted += 1;
            }
            drop(stmt);
            // Each `DELETE`+`INSERT` batch leaves FTS5 b-tree segments behind;
            // without merging them, repeated `groundgraph index` runs (CI / nightly)
            // let segment count grow unbounded and `MATCH` / `bm25()` slow down.
            // FTS5's `'optimize'` command merges all segments into one — run it
            // once per rebuild, after every row is in (issues3.md #138).
            tx.execute("INSERT INTO node_fts(node_fts) VALUES('optimize')", [])
                .map_err(StoreError::sqlite)?;
            Ok(inserted)
        })
    }

    /// BM25-ranked fulltext match. `match_expr` must be a well-formed FTS5
    /// query (the engine builds it from quoted tokens — never raw user input).
    /// Best match first; at most `limit` hits.
    pub fn fulltext_match(&self, match_expr: &str, limit: usize) -> StoreResult<Vec<FulltextHit>> {
        if match_expr.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // `prepare_cached`: every `search` runs two passes (all-tokens +
        // any-token) and `checks` adds more — the SQL is a single literal, so
        // re-parsing it each call is pure overhead (issues3.md #155).
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT node_id, bm25(node_fts) AS rank FROM node_fts \
                 WHERE node_fts MATCH ?1 ORDER BY rank, node_id LIMIT ?2",
            )
            .map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(params![match_expr, limit as i64], |row| {
                Ok(FulltextHit {
                    node_id: row.get(0)?,
                    rank: row.get(1)?,
                })
            })
            .map_err(StoreError::sqlite)?;
        let mut hits = Vec::new();
        for row in rows {
            hits.push(row.map_err(StoreError::sqlite)?);
        }
        Ok(hits)
    }
}

/// One row of the fulltext content layer: a node id plus its pre-tokenised
/// body text (see `003_fulltext.sql` for the tokenisation contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FulltextRow {
    pub node_id: String,
    pub body: String,
}

/// One BM25-ranked fulltext hit. Lower `rank` = more relevant (SQLite's
/// `bm25()` returns negated scores).
#[derive(Debug, Clone, PartialEq)]
pub struct FulltextHit {
    pub node_id: String,
    pub rank: f64,
}

fn opt_u32(row: &Row<'_>, idx: usize) -> Result<Option<u32>, rusqlite::Error> {
    let v: Option<i64> = row.get(idx)?;
    match v {
        None => Ok(None),
        Some(n) => u32::try_from(n).map(Some).map_err(|_| {
            decode_error(
                idx,
                format!("line number {n} does not fit in u32 (column {idx})"),
            )
        }),
    }
}

fn required_u32(row: &Row<'_>, idx: usize) -> Result<u32, rusqlite::Error> {
    let n: i64 = row.get(idx)?;
    u32::try_from(n).map_err(|_| {
        decode_error(
            idx,
            format!("line number {n} does not fit in u32 (column {idx})"),
        )
    })
}

fn node_from_row(row: &Row<'_>) -> Result<Node, rusqlite::Error> {
    let kind_str: String = row.get(1)?;
    // Single source of truth lives in `groundgraph-core`; the store no longer
    // keeps a parallel (drift-prone) text→kind table.
    let kind = NodeKind::from_str(&kind_str)
        .ok_or_else(|| decode_error(1, format!("unknown node kind {kind_str}")))?;
    Ok(Node {
        id: ArtifactId::new(row.get::<_, String>(0)?),
        kind,
        path: row.get(2)?,
        name: row.get(3)?,
        start_line: opt_u32(row, 4)?,
        end_line: opt_u32(row, 5)?,
        content_hash: row.get(6)?,
        stable_key: row.get(7)?,
        source_file: row.get(8)?,
        indexer: row.get(9)?,
        metadata_json: row.get(10)?,
    })
}

fn edge_from_row(row: &Row<'_>) -> Result<EdgeAssertion, rusqlite::Error> {
    Ok(EdgeAssertion {
        id: ArtifactId::new(row.get::<_, String>(0)?),
        from_id: ArtifactId::new(row.get::<_, String>(1)?),
        to_id: ArtifactId::new(row.get::<_, String>(2)?),
        kind: parse_edge_kind(&row.get::<_, String>(3)?)?,
        source: parse_edge_source(&row.get::<_, String>(4)?)?,
        certainty: parse_edge_certainty(&row.get::<_, String>(5)?)?,
        status: parse_edge_status(&row.get::<_, String>(6)?)?,
        // SQLite stores REAL as f64; our domain type is f32. Closest-value
        // rounding is the desired behaviour for confidences in [0,1]. Sanitise
        // on read too (#63) so a row written by an older build / external tool
        // with NaN/±∞/out-of-range can never reach a downstream comparator;
        // `Confidence::new` folds exactly those cases into range (#168).
        #[allow(clippy::cast_possible_truncation)]
        confidence: Confidence::new(row.get::<_, f64>(7)? as f32),
        evidence_json: row.get(8)?,
        source_file: row.get(9)?,
        indexer: row.get(10)?,
        metadata_json: row.get(11)?,
    })
}

fn evidence_from_row(row: &Row<'_>) -> Result<Evidence, rusqlite::Error> {
    let kind_str: String = row.get(2)?;
    // Single source of truth lives in `groundgraph-core` (same pattern as
    // `node_from_row`); a new EvidenceKind variant can no longer be written
    // but fail to decode.
    let kind = EvidenceKind::from_str(&kind_str)
        .ok_or_else(|| decode_error(2, format!("unknown evidence kind {kind_str}")))?;
    Ok(Evidence {
        id: ArtifactId::new(row.get::<_, String>(0)?),
        artifact_id: ArtifactId::new(row.get::<_, String>(1)?),
        kind,
        path: row.get(3)?,
        start_line: opt_u32(row, 4)?,
        end_line: opt_u32(row, 5)?,
        snippet: row.get(6)?,
        hash: row.get(7)?,
        metadata_json: row.get(8)?,
    })
}

fn symbol_range_from_row(row: &Row<'_>) -> Result<SymbolRange, rusqlite::Error> {
    let kind_str: String = row.get(4)?;
    // Accept any valid node kind (previously this hand-rolled list silently
    // omitted Rust / Java / TypeScript / C / C++, so their symbol ranges could
    // be written but never read back — a real round-trip bug caught by the
    // P23.8 corpus tests).
    let symbol_kind = NodeKind::from_str(&kind_str)
        .ok_or_else(|| decode_error(4, format!("unsupported symbol kind {kind_str}")))?;
    let parent: Option<String> = row.get(6)?;
    Ok(SymbolRange {
        file_path: row.get(0)?,
        symbol_id: ArtifactId::new(row.get::<_, String>(1)?),
        start_line: required_u32(row, 2)?,
        end_line: required_u32(row, 3)?,
        symbol_kind,
        qualified_name: row.get(5)?,
        parent_symbol_id: parent.map(ArtifactId::new),
    })
}

fn parse_edge_kind(raw: &str) -> Result<EdgeKind, rusqlite::Error> {
    EdgeKind::from_str(raw).ok_or_else(|| decode_error(3, format!("unknown edge kind {raw}")))
}

fn parse_edge_source(raw: &str) -> Result<EdgeSource, rusqlite::Error> {
    EdgeSource::from_str(raw).ok_or_else(|| decode_error(4, format!("unknown edge source {raw}")))
}

fn parse_edge_certainty(raw: &str) -> Result<EdgeCertainty, rusqlite::Error> {
    EdgeCertainty::from_str(raw)
        .ok_or_else(|| decode_error(5, format!("unknown edge certainty {raw}")))
}

fn parse_edge_status(raw: &str) -> Result<EdgeStatus, rusqlite::Error> {
    EdgeStatus::from_str(raw).ok_or_else(|| decode_error(6, format!("unknown edge status {raw}")))
}

fn decode_error(col: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        col,
        rusqlite::types::Type::Text,
        Box::<dyn std::error::Error + Send + Sync>::from(message),
    )
}

#[cfg(test)]
mod decode_tests {
    //! Drives every `parse_*` / `*_from_row` branch — including the error
    //! arms — through the public Store APIs by inserting raw SQL with
    //! known-bad enum strings and observing the read-back error.

    use super::*;
    use crate::Store;
    use rusqlite::params;
    use tempfile::NamedTempFile;

    fn fresh_store() -> Store {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        std::mem::forget(file);
        let mut store = Store::open(&path).unwrap();
        store.migrate().unwrap();
        store
    }

    #[test]
    fn parse_edge_kind_recognises_every_known_value_and_rejects_unknown() {
        let store = fresh_store();
        // Insert one edge per known kind, then one with a bogus kind.
        let kinds = [
            "contains",
            "imports",
            "documents",
            "declares_implementation",
            "declares_verification",
            "references",
            "calls",
            "reads_provider",
            "navigates_to",
            "persists_to",
            "subscribes_stream",
            "derives_from",
        ];
        for (i, k) in kinds.iter().enumerate() {
            store.conn.execute(
                "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence) VALUES (?1, 'a', 'b', ?2, 'language_adapter', 'fact', 'confirmed', 1.0)",
                params![format!("ek-{i}"), k],
            ).unwrap();
        }
        let edges = store.list_all_edges().expect("known kinds must decode");
        assert_eq!(edges.len(), kinds.len());

        // Now insert a bogus kind and confirm read fails with a decode error.
        store
            .conn
            .execute(
                "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence) VALUES ('bad-kind', 'a', 'b', 'mystery', 'language_adapter', 'fact', 'confirmed', 1.0)",
                params![],
            )
            .unwrap();
        let err = store.list_all_edges().expect_err("bogus kind must error");
        assert!(
            format!("{err}").contains("unknown edge kind mystery"),
            "{err}"
        );
    }

    #[test]
    fn upsert_node_rejects_inverted_line_range() {
        // #168: `Node` fields are `pub`, so a caller can construct
        // `start_line > end_line`. The store is the write boundary: an
        // illegal row must be rejected before it can enter the graph.
        let mut store = fresh_store();
        let mut node = Node::new(ArtifactId::new("bad-range"), NodeKind::RustFunction);
        node.start_line = Some(10);
        node.end_line = Some(5);

        let err = store.upsert_node(&node).expect_err("inverted range");
        assert!(format!("{err}").contains("start_line 10"), "{err}");

        let err = store
            .upsert_nodes_bulk(std::slice::from_ref(&node))
            .expect_err("bulk path validates too");
        assert!(format!("{err}").contains("start_line 10"), "{err}");

        assert!(
            store
                .find_node(&ArtifactId::new("bad-range"))
                .unwrap()
                .is_none(),
            "rejected node must not be persisted"
        );
    }

    #[test]
    fn upsert_edge_sanitises_out_of_range_and_nan_confidence() {
        // #63: a caller can hand the store NaN / ±∞ / negative / >1 raw
        // values (today only via `Confidence::new`, which already folds them;
        // before #168 the field was a bare `pub f32`). The store must persist
        // a finite value in [0, 1] (read back via the normal decode path) so
        // no downstream comparator ever sees a non-total-ordered confidence.
        let mut store = fresh_store();
        let cases = [
            (f32::NAN, 1.0_f32),
            (f32::INFINITY, 1.0),
            (f32::NEG_INFINITY, 0.0),
            (-0.25, 0.0),
            (2.5, 1.0),
            (0.42, 0.42),
        ];
        for (i, (raw, _want)) in cases.iter().enumerate() {
            let mut edge = EdgeAssertion::declared(
                ArtifactId::new(format!("n{i}")),
                ArtifactId::new("b"),
                EdgeKind::Calls,
                EdgeSource::LanguageAdapter,
            );
            edge.id = ArtifactId::new(format!("conf-{i}"));
            edge.confidence = Confidence::new(*raw);
            // Exercise both write paths: single + bulk.
            if i % 2 == 0 {
                store.upsert_edge(&edge).unwrap();
            } else {
                store
                    .upsert_edges_bulk(std::slice::from_ref(&edge))
                    .unwrap();
            }
        }
        let edges = store.list_all_edges().expect("edges decode");
        for (i, (_, want)) in cases.iter().enumerate() {
            let got = edges
                .iter()
                .find(|e| e.id.as_str() == format!("conf-{i}"))
                .unwrap_or_else(|| panic!("conf-{i} missing"));
            assert!(
                got.confidence.get().is_finite() && (0.0..=1.0).contains(&got.confidence.get()),
                "conf-{i}: {} out of invariant",
                got.confidence
            );
            assert_eq!(got.confidence, *want, "conf-{i} sanitised value");
        }
    }

    #[test]
    fn parse_edge_source_rejects_unknown_value() {
        let store = fresh_store();
        store
            .conn
            .execute(
                "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence) VALUES ('bad-src', 'a', 'b', 'contains', 'wat', 'fact', 'confirmed', 1.0)",
                params![],
            )
            .unwrap();
        let err = store.list_all_edges().expect_err("bogus source must error");
        assert!(
            format!("{err}").contains("unknown edge source wat"),
            "{err}"
        );
    }

    #[test]
    fn parse_edge_certainty_rejects_unknown_value() {
        let store = fresh_store();
        store
            .conn
            .execute(
                "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence) VALUES ('bad-c', 'a', 'b', 'contains', 'filesystem', 'maybe', 'confirmed', 1.0)",
                params![],
            )
            .unwrap();
        let err = store
            .list_all_edges()
            .expect_err("bogus certainty must error");
        assert!(
            format!("{err}").contains("unknown edge certainty maybe"),
            "{err}"
        );
    }

    #[test]
    fn parse_edge_status_rejects_unknown_value() {
        let store = fresh_store();
        store
            .conn
            .execute(
                "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence) VALUES ('bad-st', 'a', 'b', 'contains', 'filesystem', 'fact', 'paused', 1.0)",
                params![],
            )
            .unwrap();
        let err = store.list_all_edges().expect_err("bogus status must error");
        assert!(
            format!("{err}").contains("unknown edge status paused"),
            "{err}"
        );
    }

    #[test]
    fn parse_edge_source_recognises_external_manifest_and_git_diff() {
        let store = fresh_store();
        for (i, s) in [
            "filesystem",
            "language_adapter",
            "markdown",
            "external_manifest",
            "git_diff",
        ]
        .iter()
        .enumerate()
        {
            store.conn.execute(
                "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence) VALUES (?1, 'a', 'b', 'contains', ?2, 'fact', 'confirmed', 1.0)",
                params![format!("src-{i}"), s],
            ).unwrap();
        }
        let edges = store.list_all_edges().unwrap();
        assert_eq!(edges.len(), 5);
        // Each enum variant must round-trip back to a recognisable string.
        let raw: std::collections::BTreeSet<_> = edges.iter().map(|e| e.source.as_str()).collect();
        assert!(raw.contains("external_manifest"));
        assert!(raw.contains("git_diff"));
        assert!(raw.contains("markdown"));
    }

    #[test]
    fn node_from_row_recognises_all_kinds_and_rejects_unknown() {
        let store = fresh_store();
        // Iterate the single source of truth instead of a hand-maintained
        // list — a new `NodeKind` variant is then automatically exercised
        // here, so a missing decode branch can never slip through silently.
        let kinds = groundgraph_core::NodeKind::ALL;
        assert!(kinds.len() >= 80, "NodeKind::ALL unexpectedly small");
        for (i, k) in kinds.iter().enumerate() {
            store
                .conn
                .execute(
                    "INSERT INTO nodes (id, kind) VALUES (?1, ?2)",
                    params![format!("nk-{i}"), k.as_str()],
                )
                .unwrap();
        }
        let nodes = store.list_all_nodes().unwrap();
        assert_eq!(nodes.len(), kinds.len());
        // Every variant round-trips back to a recognised kind.
        let seen: std::collections::BTreeSet<&str> =
            nodes.iter().map(|n| n.kind.as_str()).collect();
        for k in kinds {
            assert!(
                seen.contains(k.as_str()),
                "{} did not round-trip",
                k.as_str()
            );
        }

        store
            .conn
            .execute(
                "INSERT INTO nodes (id, kind) VALUES ('bad', 'cosmic')",
                params![],
            )
            .unwrap();
        let err = store.list_all_nodes().expect_err("bogus node kind");
        assert!(
            format!("{err}").contains("unknown node kind cosmic"),
            "{err}"
        );
    }

    #[test]
    fn evidence_from_row_recognises_all_kinds_and_rejects_unknown() {
        let store = fresh_store();
        store
            .conn
            .execute(
                "INSERT INTO nodes (id, kind) VALUES ('node-1', 'file')",
                params![],
            )
            .unwrap();
        for (i, k) in [
            "doc_section",
            "dart_doc_comment",
            "dart_test_call",
            "dart_group_call",
            "import",
            "git_diff",
        ]
        .iter()
        .enumerate()
        {
            store
                .conn
                .execute(
                    "INSERT INTO evidence (id, artifact_id, kind) VALUES (?1, 'node-1', ?2)",
                    params![format!("ev-{i}"), k],
                )
                .unwrap();
        }
        let evs = store
            .list_evidence_for_artifact(&ArtifactId::new("node-1"))
            .unwrap();
        assert_eq!(evs.len(), 6);

        store
            .conn
            .execute(
                "INSERT INTO evidence (id, artifact_id, kind) VALUES ('bad', 'node-1', 'movie')",
                params![],
            )
            .unwrap();
        let err = store
            .list_evidence_for_artifact(&ArtifactId::new("node-1"))
            .expect_err("bogus evidence kind");
        assert!(
            format!("{err}").contains("unknown evidence kind movie"),
            "{err}"
        );
    }

    #[test]
    fn symbol_range_decoder_rejects_unsupported_kind() {
        let store = fresh_store();
        for (i, k) in [
            "dart_class",
            "dart_method",
            "dart_function",
            "dart_constructor",
            "test_case",
            "test_group",
            "doc_section",
        ]
        .iter()
        .enumerate()
        {
            store
                .conn
                .execute(
                    "INSERT INTO symbol_ranges (file_path, symbol_id, start_line, end_line, symbol_kind, qualified_name) VALUES ('p.dart', ?1, 1, 2, ?2, ?1)",
                    params![format!("sr-{i}"), k],
                )
                .unwrap();
        }
        let ranges = store.list_symbol_ranges_for_file("p.dart").unwrap();
        assert_eq!(ranges.len(), 7);

        store
            .conn
            .execute(
                "INSERT INTO symbol_ranges (file_path, symbol_id, start_line, end_line, symbol_kind, qualified_name) VALUES ('p.dart', 'bad', 1, 2, 'plane', 'bad')",
                params![],
            )
            .unwrap();
        let err = store
            .list_symbol_ranges_for_file("p.dart")
            .expect_err("bogus symbol kind");
        assert!(
            format!("{err}").contains("unsupported symbol kind plane"),
            "{err}"
        );
    }

    #[test]
    fn opt_u32_and_required_u32_reject_out_of_range_values() {
        let store = fresh_store();
        // start_line = -1 → fails required_u32 conversion when reading
        // back via list_symbol_ranges_for_file.
        store
            .conn
            .execute(
                "INSERT INTO symbol_ranges (file_path, symbol_id, start_line, end_line, symbol_kind, qualified_name) VALUES ('x.dart', 'sym', -1, 1, 'dart_class', 'C')",
                params![],
            )
            .unwrap();
        let err = store
            .list_symbol_ranges_for_file("x.dart")
            .expect_err("negative start_line must error");
        assert!(format!("{err}").contains("line number"), "{err}");

        // opt_u32: nodes.start_line stored as -1 should fail decoding.
        store
            .conn
            .execute(
                "INSERT INTO nodes (id, kind, start_line) VALUES ('node-2', 'dart_class', -42)",
                params![],
            )
            .unwrap();
        let err = store
            .list_all_nodes()
            .expect_err("negative node start_line must error");
        assert!(format!("{err}").contains("does not fit in u32"), "{err}");
    }

    #[test]
    fn scip_suppression_clears_only_nonscip_precision_on_covered_files() {
        let mut store = fresh_store();
        let mk = |from: &str, kind: EdgeKind, indexer: &str, file: &str| {
            let mut e = EdgeAssertion::fact(
                ArtifactId::new(from),
                ArtifactId::new("t"),
                kind,
                EdgeSource::LanguageAdapter,
            );
            e.indexer = Some(indexer.to_string());
            e.source_file = Some(file.to_string());
            e
        };
        // covered.go: a SCIP call + a heuristic call (dup) + a heuristic CONTAINS (structural).
        store
            .upsert_edge(&mk("scip_call", EdgeKind::Calls, "scip", "covered.go"))
            .unwrap();
        store
            .upsert_edge(&mk(
                "heur_call",
                EdgeKind::Calls,
                "go_treesitter",
                "covered.go",
            ))
            .unwrap();
        store
            .upsert_edge(&mk(
                "heur_ref",
                EdgeKind::References,
                "go_treesitter",
                "covered.go",
            ))
            .unwrap();
        store
            .upsert_edge(&mk(
                "heur_contains",
                EdgeKind::Contains,
                "go_treesitter",
                "covered.go",
            ))
            .unwrap();
        // uncovered.go: heuristic-only gap-fill that must survive.
        store
            .upsert_edge(&mk(
                "gap_call",
                EdgeKind::Calls,
                "go_treesitter",
                "uncovered.go",
            ))
            .unwrap();

        let removed = store
            .delete_precision_edges_for_files_except(&["covered.go".to_string()], "scip")
            .unwrap();
        assert_eq!(removed, 2, "the heuristic Calls + References on covered.go");

        let froms: std::collections::BTreeSet<String> = store
            .list_all_edges()
            .unwrap()
            .iter()
            .map(|e| e.from_id.as_str().to_string())
            .collect();
        assert!(
            froms.contains("scip_call"),
            "SCIP edge on covered file kept"
        );
        assert!(
            froms.contains("heur_contains"),
            "structural CONTAINS never suppressed"
        );
        assert!(
            froms.contains("gap_call"),
            "heuristic gap-fill on uncovered file kept"
        );
        assert!(
            !froms.contains("heur_call"),
            "heuristic Calls dup on covered file removed"
        );
        assert!(
            !froms.contains("heur_ref"),
            "heuristic References on covered file removed"
        );
    }

    #[test]
    fn list_helpers_round_trip_full_node_and_edge() {
        // Cover non-error decoding for nodes and edges with every optional
        // field populated, so opt_i64 / opt_u32 / required_u32 happy paths
        // are exercised.
        let mut store = fresh_store();
        let mut node = Node::new(ArtifactId::new("file::lib/a.dart"), NodeKind::File);
        node.path = Some("lib/a.dart".into());
        node.name = Some("a.dart".into());
        node.start_line = Some(10);
        node.end_line = Some(20);
        node.content_hash = Some("h".into());
        node.stable_key = Some("k".into());
        node.source_file = Some("lib/a.dart".into());
        node.indexer = Some("dart_lightweight".into());
        node.metadata_json = Some("{}".into());
        store.upsert_node(&node).unwrap();

        let mut edge = EdgeAssertion::fact(
            ArtifactId::new("a"),
            ArtifactId::new("b"),
            EdgeKind::Contains,
            EdgeSource::LanguageAdapter,
        );
        edge.indexer = Some("dart_lightweight".into());
        edge.metadata_json = Some("{}".into());
        store.upsert_edge(&edge).unwrap();

        let found = store.find_node(&node.id).unwrap().unwrap();
        assert_eq!(found.start_line, Some(10));
        let edges = store.list_edges_from(&edge.from_id).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].indexer.as_deref(), Some("dart_lightweight"));
    }

    /// #205: two indexers asserting the *same* `(kind, from, to)` edge from
    /// *different* sources must coexist — the id now encodes `source`, so they
    /// no longer UPSERT over each other.
    #[test]
    fn distinct_sources_for_same_kind_from_to_coexist() {
        let mut store = fresh_store();
        let from = ArtifactId::new("doc::section");
        let to = ArtifactId::new("req::REQ-1");
        let markdown = EdgeAssertion::declared(
            from.clone(),
            to.clone(),
            EdgeKind::Documents,
            EdgeSource::Markdown,
        );
        let external = EdgeAssertion::declared(
            from.clone(),
            to.clone(),
            EdgeKind::Documents,
            EdgeSource::ExternalManifest,
        );
        store.upsert_edge(&markdown).unwrap();
        store.upsert_edge(&external).unwrap();

        let edges = store.list_edges_from(&from).unwrap();
        assert_eq!(
            edges.len(),
            2,
            "distinct sources must coexist, not overwrite; got {edges:?}"
        );
        assert!(edges.iter().any(|e| e.source == EdgeSource::Markdown));
        assert!(edges
            .iter()
            .any(|e| e.source == EdgeSource::ExternalManifest));
    }

    /// #205: when two edges resolve to the *same* id (same kind+source+from+to,
    /// differing only in mutable state), a later `declared` must NOT downgrade
    /// an existing `fact` — the ON CONFLICT update guards certainty.
    #[test]
    fn upsert_edge_does_not_downgrade_fact_to_declared() {
        let mut store = fresh_store();
        let from = ArtifactId::new("a");
        let to = ArtifactId::new("b");
        // Same id (certainty is not in the id), certainty Fact.
        let fact = EdgeAssertion::fact(
            from.clone(),
            to.clone(),
            EdgeKind::Contains,
            EdgeSource::Filesystem,
        );
        store.upsert_edge(&fact).unwrap();
        // Same id again, this time a Declared edge trying to overwrite.
        let declared =
            EdgeAssertion::declared(from, to, EdgeKind::Contains, EdgeSource::Filesystem);
        store.upsert_edge(&declared).unwrap();

        let edges = store.list_all_edges().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].certainty,
            EdgeCertainty::Fact,
            "a late Declared must not downgrade an existing Fact"
        );
    }

    /// #205: the guard is one-directional — a later `fact` still upgrades an
    /// existing `declared` (same id), and same-level writes refresh.
    #[test]
    fn upsert_edge_upgrades_declared_to_fact() {
        let mut store = fresh_store();
        let from = ArtifactId::new("a");
        let to = ArtifactId::new("b");
        let declared = EdgeAssertion::declared(
            from.clone(),
            to.clone(),
            EdgeKind::Contains,
            EdgeSource::Filesystem,
        );
        store.upsert_edge(&declared).unwrap();
        let fact = EdgeAssertion::fact(from, to, EdgeKind::Contains, EdgeSource::Filesystem);
        store.upsert_edge(&fact).unwrap();

        let edges = store.list_all_edges().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].certainty,
            EdgeCertainty::Fact,
            "a late Fact must upgrade an existing Declared"
        );
    }
}
