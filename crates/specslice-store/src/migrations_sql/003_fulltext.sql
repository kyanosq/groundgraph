-- 003: full-text content layer (FTS5).
--
-- `node_fts` holds one row per content-bearing node (doc section bodies, code
-- symbol spans). `body` is PRE-TOKENISED by the engine (ASCII identifiers
-- split camel/snake + lowercased; CJK runs expanded into overlapping bigrams)
-- and space-joined, so the stock unicode61 tokenizer only has to split on
-- whitespace — bilingual search without a custom SQLite tokenizer extension.
--
-- The table is rebuilt wholesale on every `specslice index` run (the content
-- pass is cheap), so it never needs per-indexer ownership/cleanup like the
-- `nodes` table does.
CREATE VIRTUAL TABLE IF NOT EXISTS node_fts USING fts5(
    node_id UNINDEXED,
    body,
    tokenize = 'unicode61'
);
