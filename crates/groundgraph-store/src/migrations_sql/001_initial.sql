-- GroundGraph MVP-0 initial schema.
-- Mirrors PRD §5. Tables use IF NOT EXISTS for defensive idempotency, even
-- though `schema_version` already guards against double-apply.

CREATE TABLE IF NOT EXISTS nodes (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    path TEXT,
    name TEXT,
    start_line INTEGER,
    end_line INTEGER,
    content_hash TEXT,
    stable_key TEXT,
    source_file TEXT,
    source_hash TEXT,
    indexer TEXT,
    index_generation INTEGER,
    metadata_json TEXT
);

CREATE TABLE IF NOT EXISTS edge_assertions (
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
    source_hash TEXT,
    indexer TEXT,
    index_generation INTEGER,
    metadata_json TEXT
);

CREATE TABLE IF NOT EXISTS evidence (
    id TEXT PRIMARY KEY,
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    path TEXT,
    start_line INTEGER,
    end_line INTEGER,
    snippet TEXT,
    hash TEXT,
    metadata_json TEXT
);

CREATE TABLE IF NOT EXISTS symbol_ranges (
    file_path TEXT NOT NULL,
    symbol_id TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    symbol_kind TEXT,
    qualified_name TEXT,
    parent_symbol_id TEXT,
    PRIMARY KEY (file_path, symbol_id)
);

CREATE TABLE IF NOT EXISTS file_index (
    path TEXT PRIMARY KEY,
    hash TEXT NOT NULL,
    kind TEXT NOT NULL,
    indexed_at TEXT NOT NULL,
    index_generation INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS slice_cache (
    root_id TEXT PRIMARY KEY,
    input_hash TEXT NOT NULL,
    index_generation INTEGER NOT NULL,
    slice_json TEXT NOT NULL,
    generated_at TEXT NOT NULL
);
