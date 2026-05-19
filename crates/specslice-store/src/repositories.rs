//! Repository helpers built on top of [`Store`].
//!
//! All operations are idempotent via `INSERT ... ON CONFLICT DO UPDATE`. The
//! conflict targets always match the schema's primary keys.

use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use specslice_core::{
    ArtifactId, EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus, Evidence,
    EvidenceKind, Node, NodeKind, SymbolRange,
};

use crate::{Store, StoreError, StoreResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIndexEntry {
    pub path: String,
    pub hash: String,
    pub kind: String,
    pub indexed_at: String,
    pub index_generation: i64,
}

const SELECT_NODE_COLS: &str = "id, kind, path, name, start_line, end_line, content_hash, stable_key, source_file, source_hash, indexer, index_generation, metadata_json";

const SELECT_EDGE_COLS: &str = "id, from_id, to_id, kind, source, certainty, status, confidence, evidence_json, source_file, source_hash, indexer, index_generation, metadata_json";

const SELECT_EVIDENCE_COLS: &str =
    "id, artifact_id, kind, path, start_line, end_line, snippet, hash, metadata_json";

const SELECT_RANGE_COLS: &str =
    "file_path, symbol_id, start_line, end_line, symbol_kind, qualified_name, parent_symbol_id";

impl Store {
    pub fn upsert_node(&mut self, node: &Node) -> StoreResult<()> {
        let sql = "INSERT INTO nodes (id, kind, path, name, start_line, end_line, content_hash, stable_key, source_file, source_hash, indexer, index_generation, metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, path=excluded.path, name=excluded.name, start_line=excluded.start_line, end_line=excluded.end_line, content_hash=excluded.content_hash, stable_key=excluded.stable_key, source_file=excluded.source_file, source_hash=excluded.source_hash, indexer=excluded.indexer, index_generation=excluded.index_generation, metadata_json=excluded.metadata_json";
        self.conn
            .execute(
                sql,
                params![
                    node.id.as_str(),
                    node.kind.as_str(),
                    node.path,
                    node.name,
                    node.start_line,
                    node.end_line,
                    node.content_hash,
                    node.stable_key,
                    node.source_file,
                    node.source_hash,
                    node.indexer,
                    node.index_generation,
                    node.metadata_json,
                ],
            )
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn find_node(&self, id: &ArtifactId) -> StoreResult<Option<Node>> {
        let sql = format!("SELECT {SELECT_NODE_COLS} FROM nodes WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql).map_err(StoreError::sqlite)?;
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
        let sql = format!("SELECT {SELECT_NODE_COLS} FROM nodes WHERE kind = ?1 ORDER BY id");
        let mut stmt = self.conn.prepare(&sql).map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(params![kind.as_str()], node_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    pub fn list_all_nodes(&self) -> StoreResult<Vec<Node>> {
        let sql = format!("SELECT {SELECT_NODE_COLS} FROM nodes ORDER BY id");
        let mut stmt = self.conn.prepare(&sql).map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map([], node_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    pub fn upsert_edge(&mut self, edge: &EdgeAssertion) -> StoreResult<()> {
        let sql = "INSERT INTO edge_assertions (id, from_id, to_id, kind, source, certainty, status, confidence, evidence_json, source_file, source_hash, indexer, index_generation, metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) ON CONFLICT(id) DO UPDATE SET from_id=excluded.from_id, to_id=excluded.to_id, kind=excluded.kind, source=excluded.source, certainty=excluded.certainty, status=excluded.status, confidence=excluded.confidence, evidence_json=excluded.evidence_json, source_file=excluded.source_file, source_hash=excluded.source_hash, indexer=excluded.indexer, index_generation=excluded.index_generation, metadata_json=excluded.metadata_json";
        self.conn
            .execute(
                sql,
                params![
                    edge.id.as_str(),
                    edge.from_id.as_str(),
                    edge.to_id.as_str(),
                    edge.kind.as_str(),
                    edge.source.as_str(),
                    edge.certainty.as_str(),
                    edge.status.as_str(),
                    edge.confidence as f64,
                    edge.evidence_json,
                    edge.source_file,
                    edge.source_hash,
                    edge.indexer,
                    edge.index_generation,
                    edge.metadata_json,
                ],
            )
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn list_edges_by_kind(&self, kind: EdgeKind) -> StoreResult<Vec<EdgeAssertion>> {
        self.query_edges("WHERE kind = ?1 ORDER BY id", params![kind.as_str()])
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
        let sql = format!("SELECT {SELECT_EDGE_COLS} FROM edge_assertions {suffix}");
        let mut stmt = self.conn.prepare(&sql).map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(params, edge_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    pub fn upsert_evidence(&mut self, ev: &Evidence) -> StoreResult<()> {
        let sql = "INSERT INTO evidence (id, artifact_id, kind, path, start_line, end_line, snippet, hash, metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(id) DO UPDATE SET artifact_id=excluded.artifact_id, kind=excluded.kind, path=excluded.path, start_line=excluded.start_line, end_line=excluded.end_line, snippet=excluded.snippet, hash=excluded.hash, metadata_json=excluded.metadata_json";
        self.conn
            .execute(
                sql,
                params![
                    ev.id.as_str(),
                    ev.artifact_id.as_str(),
                    ev.kind.as_str(),
                    ev.path,
                    ev.start_line,
                    ev.end_line,
                    ev.snippet,
                    ev.hash,
                    ev.metadata_json,
                ],
            )
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn list_evidence_for_artifact(&self, artifact: &ArtifactId) -> StoreResult<Vec<Evidence>> {
        let sql = format!(
            "SELECT {SELECT_EVIDENCE_COLS} FROM evidence WHERE artifact_id = ?1 ORDER BY id"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(params![artifact.as_str()], evidence_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    pub fn upsert_symbol_range(&mut self, range: &SymbolRange) -> StoreResult<()> {
        let sql = "INSERT INTO symbol_ranges (file_path, symbol_id, start_line, end_line, symbol_kind, qualified_name, parent_symbol_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(file_path, symbol_id) DO UPDATE SET start_line=excluded.start_line, end_line=excluded.end_line, symbol_kind=excluded.symbol_kind, qualified_name=excluded.qualified_name, parent_symbol_id=excluded.parent_symbol_id";
        self.conn
            .execute(
                sql,
                params![
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
                ],
            )
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn list_symbol_ranges_for_file(&self, file_path: &str) -> StoreResult<Vec<SymbolRange>> {
        let sql = format!("SELECT {SELECT_RANGE_COLS} FROM symbol_ranges WHERE file_path = ?1 ORDER BY start_line, symbol_id");
        let mut stmt = self.conn.prepare(&sql).map_err(StoreError::sqlite)?;
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
        let sql = format!("SELECT {SELECT_RANGE_COLS} FROM symbol_ranges WHERE file_path = ?1 AND start_line <= ?3 AND end_line >= ?2 ORDER BY start_line, end_line, symbol_id");
        let mut stmt = self.conn.prepare(&sql).map_err(StoreError::sqlite)?;
        let rows = stmt
            .query_map(params![file_path, start, end], symbol_range_from_row)
            .map_err(StoreError::sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::sqlite)
    }

    pub fn upsert_file_index(&mut self, entry: &FileIndexEntry) -> StoreResult<()> {
        let sql = "INSERT INTO file_index (path, hash, kind, indexed_at, index_generation) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(path) DO UPDATE SET hash=excluded.hash, kind=excluded.kind, indexed_at=excluded.indexed_at, index_generation=excluded.index_generation";
        self.conn
            .execute(
                sql,
                params![
                    entry.path,
                    entry.hash,
                    entry.kind,
                    entry.indexed_at,
                    entry.index_generation,
                ],
            )
            .map(|_| ())
            .map_err(StoreError::sqlite)
    }

    pub fn get_file_hash(&self, path: &str) -> StoreResult<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT hash FROM file_index WHERE path = ?1")
            .map_err(StoreError::sqlite)?;
        let mut rows = stmt.query(params![path]).map_err(StoreError::sqlite)?;
        if let Some(row) = rows.next().map_err(StoreError::sqlite)? {
            Ok(Some(row.get::<_, String>(0).map_err(StoreError::sqlite)?))
        } else {
            Ok(None)
        }
    }

    /// Delete every node, edge, evidence and symbol range produced by the
    /// given indexer name. Used to re-index without leaving stale rows.
    pub fn clear_indexer_outputs(&mut self, indexer: &str) -> StoreResult<()> {
        let tx = self.conn.transaction().map_err(StoreError::sqlite)?;
        tx.execute("DELETE FROM nodes WHERE indexer = ?1", params![indexer])
            .map_err(StoreError::sqlite)?;
        tx.execute(
            "DELETE FROM edge_assertions WHERE indexer = ?1",
            params![indexer],
        )
        .map_err(StoreError::sqlite)?;
        tx.execute(
            "DELETE FROM evidence WHERE artifact_id NOT IN (SELECT id FROM nodes)",
            params![],
        )
        .map_err(StoreError::sqlite)?;
        tx.execute(
            "DELETE FROM symbol_ranges WHERE symbol_id NOT IN (SELECT id FROM nodes)",
            params![],
        )
        .map_err(StoreError::sqlite)?;
        tx.commit().map_err(StoreError::sqlite)?;
        Ok(())
    }
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

fn opt_i64(row: &Row<'_>, idx: usize) -> Result<Option<i64>, rusqlite::Error> {
    row.get(idx)
}

fn node_from_row(row: &Row<'_>) -> Result<Node, rusqlite::Error> {
    let kind_str: String = row.get(1)?;
    let kind = match kind_str.as_str() {
        "file" => NodeKind::File,
        "requirement" => NodeKind::Requirement,
        "acceptance_criterion" => NodeKind::AcceptanceCriterion,
        "adr" => NodeKind::Adr,
        "doc_section" => NodeKind::DocSection,
        "dart_class" => NodeKind::DartClass,
        "dart_method" => NodeKind::DartMethod,
        "dart_function" => NodeKind::DartFunction,
        "dart_constructor" => NodeKind::DartConstructor,
        "test_case" => NodeKind::TestCase,
        "test_group" => NodeKind::TestGroup,
        other => return Err(decode_error(1, format!("unknown node kind {other}"))),
    };
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
        source_hash: row.get(9)?,
        indexer: row.get(10)?,
        index_generation: opt_i64(row, 11)?,
        metadata_json: row.get(12)?,
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
        // rounding is the desired behaviour for confidences in [0,1].
        #[allow(clippy::cast_possible_truncation)]
        confidence: row.get::<_, f64>(7)? as f32,
        evidence_json: row.get(8)?,
        source_file: row.get(9)?,
        source_hash: row.get(10)?,
        indexer: row.get(11)?,
        index_generation: opt_i64(row, 12)?,
        metadata_json: row.get(13)?,
    })
}

fn evidence_from_row(row: &Row<'_>) -> Result<Evidence, rusqlite::Error> {
    let kind_str: String = row.get(2)?;
    let kind = match kind_str.as_str() {
        "doc_section" => EvidenceKind::DocSection,
        "dart_doc_comment" => EvidenceKind::DartDocComment,
        "dart_test_call" => EvidenceKind::DartTestCall,
        "dart_group_call" => EvidenceKind::DartGroupCall,
        "import" => EvidenceKind::Import,
        "git_diff" => EvidenceKind::GitDiff,
        other => return Err(decode_error(2, format!("unknown evidence kind {other}"))),
    };
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
    let symbol_kind = match kind_str.as_str() {
        "dart_class" => NodeKind::DartClass,
        "dart_method" => NodeKind::DartMethod,
        "dart_function" => NodeKind::DartFunction,
        "dart_constructor" => NodeKind::DartConstructor,
        "test_case" => NodeKind::TestCase,
        "test_group" => NodeKind::TestGroup,
        "doc_section" => NodeKind::DocSection,
        other => return Err(decode_error(4, format!("unsupported symbol kind {other}"))),
    };
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
    Ok(match raw {
        "contains" => EdgeKind::Contains,
        "imports" => EdgeKind::Imports,
        "documents" => EdgeKind::Documents,
        "declares_implementation" => EdgeKind::DeclaresImplementation,
        "declares_verification" => EdgeKind::DeclaresVerification,
        "references" => EdgeKind::References,
        "calls" => EdgeKind::Calls,
        other => return Err(decode_error(3, format!("unknown edge kind {other}"))),
    })
}

fn parse_edge_source(raw: &str) -> Result<EdgeSource, rusqlite::Error> {
    Ok(match raw {
        "filesystem" => EdgeSource::Filesystem,
        "language_adapter" => EdgeSource::LanguageAdapter,
        "markdown" => EdgeSource::Markdown,
        "external_manifest" => EdgeSource::ExternalManifest,
        "git_diff" => EdgeSource::GitDiff,
        other => return Err(decode_error(4, format!("unknown edge source {other}"))),
    })
}

fn parse_edge_certainty(raw: &str) -> Result<EdgeCertainty, rusqlite::Error> {
    Ok(match raw {
        "fact" => EdgeCertainty::Fact,
        "declared" => EdgeCertainty::Declared,
        other => return Err(decode_error(5, format!("unknown edge certainty {other}"))),
    })
}

fn parse_edge_status(raw: &str) -> Result<EdgeStatus, rusqlite::Error> {
    Ok(match raw {
        "confirmed" => EdgeStatus::Confirmed,
        "deprecated" => EdgeStatus::Deprecated,
        other => return Err(decode_error(6, format!("unknown edge status {other}"))),
    })
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
        let kinds = [
            "file",
            "requirement",
            "acceptance_criterion",
            "adr",
            "doc_section",
            "dart_class",
            "dart_method",
            "dart_function",
            "dart_constructor",
            "test_case",
            "test_group",
        ];
        for (i, k) in kinds.iter().enumerate() {
            store
                .conn
                .execute(
                    "INSERT INTO nodes (id, kind) VALUES (?1, ?2)",
                    params![format!("nk-{i}"), k],
                )
                .unwrap();
        }
        let nodes = store.list_all_nodes().unwrap();
        assert_eq!(nodes.len(), kinds.len());

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
        node.source_hash = Some("hh".into());
        node.indexer = Some("dart_lightweight".into());
        node.index_generation = Some(3);
        node.metadata_json = Some("{}".into());
        store.upsert_node(&node).unwrap();

        let mut edge = EdgeAssertion::fact(
            ArtifactId::new("a"),
            ArtifactId::new("b"),
            EdgeKind::Contains,
            EdgeSource::LanguageAdapter,
        );
        edge.indexer = Some("dart_lightweight".into());
        edge.index_generation = Some(7);
        edge.metadata_json = Some("{}".into());
        store.upsert_edge(&edge).unwrap();

        let found = store.find_node(&node.id).unwrap().unwrap();
        assert_eq!(found.start_line, Some(10));
        assert_eq!(found.index_generation, Some(3));
        let edges = store.list_edges_from(&edge.from_id).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].index_generation, Some(7));
    }
}
