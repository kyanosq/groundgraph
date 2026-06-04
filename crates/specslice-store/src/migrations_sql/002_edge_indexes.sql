-- Adjacency indexes for edge_assertions and the evidence foreign key.
--
-- The MVP-0 schema (001) only declared the `id` primary key, so every
-- `list_edges_from` / `list_edges_to` (`WHERE from_id = ?` / `WHERE to_id = ?`)
-- and `list_edges_by_kind` (`WHERE kind = ?`) was a full table scan. On a
-- real repo (tailorx: ~61k edges) the search neighbour-boost pass issued one
-- such scan per hit, so a common multi-token query (thousands of hits) did
-- hundreds of millions of row reads (~230s). slice / impact / dead-code fan
-- out over the same `list_edges_from/to` calls and pay the same tax.
--
-- These mirror CodeGraph's `idx_edges_source_kind` / `idx_edges_target_kind`
-- adjacency indexes. IF NOT EXISTS keeps the migration idempotent and safe to
-- apply to already-initialised databases (the index is built once on next
-- store open).
CREATE INDEX IF NOT EXISTS idx_edge_assertions_from ON edge_assertions(from_id);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_to ON edge_assertions(to_id);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_kind ON edge_assertions(kind);

-- Evidence is fetched per artifact (`WHERE artifact_id = ?`) by outbound
-- evidence-quality scoring and focus cards; index the foreign key so those
-- stay O(matches) instead of O(table).
CREATE INDEX IF NOT EXISTS idx_evidence_artifact ON evidence(artifact_id);
