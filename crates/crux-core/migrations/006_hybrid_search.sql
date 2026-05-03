-- Layer 6: hybrid search.
--
-- A `chunks` row is the unit of retrieval: a code symbol, a prose
-- paragraph, an auto-memory entry, etc. Two parallel FTS5 tables
-- (porter, trigram) feed the BM25 path; vectors live in a plain table
-- of f32 BLOBs that the engine scores via brute-force cosine.
--
-- We deliberately avoid the `sqlite-vec` extension so the binary stays
-- pure-Rust and portable. A future migration can add a vec0 virtual
-- table once the loadable extension is wired up.

CREATE TABLE IF NOT EXISTS chunks (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    project_root     TEXT NOT NULL,
    source_id        INTEGER,                    -- optional FK to ast_nodes.id
    file_path        TEXT NOT NULL DEFAULT '',
    language         TEXT,
    content_type     TEXT NOT NULL DEFAULT 'code', -- code | prose | symbol | memory
    title            TEXT,
    content          TEXT NOT NULL,
    line_start       INTEGER NOT NULL DEFAULT 0,
    line_end         INTEGER NOT NULL DEFAULT 0,
    tokens_est       INTEGER NOT NULL DEFAULT 0,
    content_hash     TEXT NOT NULL,
    created_at_epoch INTEGER NOT NULL,
    UNIQUE(project_root, content_hash)
);

CREATE INDEX IF NOT EXISTS idx_chunks_project ON chunks(project_root);
CREATE INDEX IF NOT EXISTS idx_chunks_file    ON chunks(project_root, file_path);
CREATE INDEX IF NOT EXISTS idx_chunks_source  ON chunks(source_id);
CREATE INDEX IF NOT EXISTS idx_chunks_kind    ON chunks(project_root, content_type);

-- Two FTS5 tokenizers running in parallel. The engine merges their
-- ranked results via Reciprocal Rank Fusion with the dense path.
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts_porter USING fts5(
    content,
    title,
    content='chunks',
    content_rowid='id',
    tokenize='porter unicode61'
);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts_trigram USING fts5(
    content,
    title,
    content='chunks',
    content_rowid='id',
    tokenize='trigram'
);

-- Keep the two FTS shadows in sync with `chunks`.
CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts_porter (rowid, content, title)
        VALUES (new.id, new.content, COALESCE(new.title, ''));
    INSERT INTO chunks_fts_trigram (rowid, content, title)
        VALUES (new.id, new.content, COALESCE(new.title, ''));
END;

CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts_porter  (chunks_fts_porter,  rowid, content, title)
        VALUES ('delete', old.id, old.content, COALESCE(old.title, ''));
    INSERT INTO chunks_fts_trigram (chunks_fts_trigram, rowid, content, title)
        VALUES ('delete', old.id, old.content, COALESCE(old.title, ''));
END;

CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
    INSERT INTO chunks_fts_porter  (chunks_fts_porter,  rowid, content, title)
        VALUES ('delete', old.id, old.content, COALESCE(old.title, ''));
    INSERT INTO chunks_fts_trigram (chunks_fts_trigram, rowid, content, title)
        VALUES ('delete', old.id, old.content, COALESCE(old.title, ''));
    INSERT INTO chunks_fts_porter  (rowid, content, title)
        VALUES (new.id, new.content, COALESCE(new.title, ''));
    INSERT INTO chunks_fts_trigram (rowid, content, title)
        VALUES (new.id, new.content, COALESCE(new.title, ''));
END;

-- Pure-rusqlite vector storage. `vector` is a tightly packed little-endian
-- f32 array; the engine validates `dim` matches the column metadata.
CREATE TABLE IF NOT EXISTS chunk_embeddings (
    chunk_id    INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
    provider    TEXT NOT NULL,
    model       TEXT NOT NULL,
    dim         INTEGER NOT NULL,
    vector      BLOB NOT NULL,
    norm        REAL NOT NULL,
    updated_at_epoch INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_chunk_embeddings_provider
    ON chunk_embeddings(provider, model);
