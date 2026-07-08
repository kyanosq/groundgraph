-- #140: composite covering indexes for the adjacency lookups.
--
-- `list_edges_from` / `list_edges_to` / `list_edges_by_kind` all run
--   WHERE <col> = ? ORDER BY id
-- and `edge_assertions.id` is TEXT, so the 002 single-column indexes
-- (`from_id`) / (`to_id`) / (`kind`) locate the matching rows but leave their
-- TEXT-id ordering to a `USE TEMP B-TREE FOR ORDER BY`. search / slice / impact
-- fan out thousands of these per query, so that per-call sort accumulates.
--
-- A composite `(<col>, id)` index stores entries ordered by the key then `id`,
-- so an equality on the key yields a contiguous run already in `id` order — the
-- sort is elided. The new `_ord` names make this a clean rename: the one-time
-- DROP removes the old single-column index from already-initialised databases,
-- and the self-heal (`ensure_query_indexes`, run on every open) only ever
-- recreates the `_ord` form, so there is no `IF NOT EXISTS` name clash that
-- could silently keep an old single-column index alive.
DROP INDEX IF EXISTS idx_edge_assertions_from;
DROP INDEX IF EXISTS idx_edge_assertions_to;
DROP INDEX IF EXISTS idx_edge_assertions_kind;

CREATE INDEX IF NOT EXISTS idx_edge_assertions_from_ord ON edge_assertions(from_id, id);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_to_ord ON edge_assertions(to_id, id);
CREATE INDEX IF NOT EXISTS idx_edge_assertions_kind_ord ON edge_assertions(kind, id);
