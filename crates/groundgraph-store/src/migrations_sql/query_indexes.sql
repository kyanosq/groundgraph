-- Self-heal index set, run on every `Store::open` (read-only command paths
-- never call `migrate`, but still need index-backed adjacency lookups after a
-- binary upgrade). DROP-free and `IF NOT EXISTS`-guarded so it is a cheap
-- catalog check once the indexes exist, and never momentarily removes a live
-- index a concurrent reader depends on.
--
-- This is the canonical, post-#140 shape: composite `(<col>, id)` adjacency
-- indexes (see 004_edge_order_indexes.sql) plus the evidence foreign-key and
-- ingest-path indexes that 002 introduced and 004 left untouched. The one-time
-- migration 004 drops the old single-column adjacency indexes; this self-heal
-- only ever recreates the `_ord` form, so the two never disagree.
CREATE INDEX IF NOT EXISTS idx_edge_assertions_from_ord ON edge_assertions(from_id, id);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_to_ord ON edge_assertions(to_id, id);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_kind_ord ON edge_assertions(kind, id);

CREATE INDEX IF NOT EXISTS idx_evidence_artifact ON evidence(artifact_id);

CREATE INDEX IF NOT EXISTS idx_edge_assertions_source_file ON edge_assertions(source_file);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_indexer ON edge_assertions(indexer);
CREATE INDEX IF NOT EXISTS idx_nodes_indexer ON nodes(indexer);
