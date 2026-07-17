-- 005: schema cleanup (#151 / #152 / #188 / #190).
--
-- `source_hash` and `index_generation` were defined on `nodes` and
-- `edge_assertions` from day one (001) but no indexer ever wrote them: every
-- row carried NULL, pure write amplification plus a schema that advertised a
-- capability (content-hash tracking, generation-fenced clears) the code never
-- delivered (#152 / #188 / #190). `slice_cache` was created by 001 yet never
-- read or written by any path — a dead table (#151). This migration removes
-- all three.
--
-- SQLite has no `ALTER TABLE … DROP COLUMN` on the versions we target, so each
-- column drop is a four-step table rebuild: create the new shape, copy the kept
-- columns, drop the old table, rename. `DROP TABLE` also drops every index on
-- the table, so the adjacency / ingest indexes introduced by 002 and 004 are
-- recreated afterwards with `IF NOT EXISTS`. `ensure_query_indexes` would
-- restore them on the next open anyway, but recreating them here leaves a
-- freshly-migrated DB consistent without a reopen and keeps the migration
-- self-contained.
--
-- `node_fts` (003) is independent of the `nodes` shape: it stores its own
-- `node_id` / `body` rows behind no foreign key and is rebuilt wholesale on
-- every `groundgraph index`. The rebuild preserves every `nodes.id`, so
-- `node_fts` stays consistent and is left untouched.

-- nodes: 13 → 11 columns (drop source_hash, index_generation).
CREATE TABLE nodes_new (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    path TEXT,
    name TEXT,
    start_line INTEGER,
    end_line INTEGER,
    content_hash TEXT,
    stable_key TEXT,
    source_file TEXT,
    indexer TEXT,
    metadata_json TEXT
);

INSERT INTO nodes_new (
    id, kind, path, name, start_line, end_line,
    content_hash, stable_key, source_file, indexer, metadata_json
)
SELECT
    id, kind, path, name, start_line, end_line,
    content_hash, stable_key, source_file, indexer, metadata_json
FROM nodes;

DROP TABLE nodes;
ALTER TABLE nodes_new RENAME TO nodes;

-- edge_assertions: 14 → 12 columns (drop source_hash, index_generation).
CREATE TABLE edge_assertions_new (
    id TEXT PRIMARY KEY,
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    source TEXT NOT NULL,
    certainty TEXT NOT NULL,
    status TEXT NOT NULL,
    confidence REAL NOT NULL,
    evidence_json TEXT,
    source_file TEXT,
    indexer TEXT,
    metadata_json TEXT
);

INSERT INTO edge_assertions_new (
    id, from_id, to_id, kind, source, certainty, status, confidence,
    evidence_json, source_file, indexer, metadata_json
)
SELECT
    id, from_id, to_id, kind, source, certainty, status, confidence,
    evidence_json, source_file, indexer, metadata_json
FROM edge_assertions;

DROP TABLE edge_assertions;
ALTER TABLE edge_assertions_new RENAME TO edge_assertions;

-- Recreate the indexes that lived on the two rebuilt tables (002 + 004): the
-- rebuild's DROP TABLE removed them along with the old tables.
CREATE INDEX IF NOT EXISTS idx_nodes_indexer ON nodes(indexer);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_source_file ON edge_assertions(source_file);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_indexer ON edge_assertions(indexer);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_from_ord ON edge_assertions(from_id, id);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_to_ord ON edge_assertions(to_id, id);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_kind_ord ON edge_assertions(kind, id);

-- slice_cache was created by 001 but never read or written (#151).
DROP TABLE IF EXISTS slice_cache;
