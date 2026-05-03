-- Layer 5.12.5: persist per-file `FileTypes` signatures so `crux index`
-- can skip the phase-1 signature re-parse when a file's content hash
-- is unchanged.
--
-- Background: L5.12 introduced a project-wide signature aggregate
-- (`ProjectFileTypes`) used by phase 2 to resolve cross-file receiver
-- typing. Building that aggregate requires parsing every file — even
-- unchanged ones — which pushed no-op `crux index` from ~170 ms to
-- ~560 ms on the CRUX repo. This table lets phase 1 deserialize a
-- previously-parsed file's `FileTypes` straight from the DB when the
-- on-disk SHA-256 matches the stored `content_hash`.
--
-- `payload` is a bincode-encoded [`FileTypes`] blob. `schema_version`
-- guards against layout drift: if the constant changes in the next
-- CRUX release, rows with an older version fail the equality check in
-- `SELECT` and the indexer falls back to re-parsing + re-writing the
-- entry. No destructive migration is required when the schema bumps.
--
-- One row per (project_root, file_path); paired with the
-- `scope = 'ast'` rows in `file_snapshots` but kept in its own table
-- because Layer 6 doesn't need the payload column and the blob would
-- bloat every snapshot query.

CREATE TABLE IF NOT EXISTS ast_file_signatures (
    project_root     TEXT    NOT NULL,
    file_path        TEXT    NOT NULL,
    content_hash     TEXT    NOT NULL,
    schema_version   INTEGER NOT NULL,
    payload          BLOB    NOT NULL,
    indexed_at_epoch INTEGER NOT NULL,
    PRIMARY KEY (project_root, file_path)
);

CREATE INDEX IF NOT EXISTS idx_ast_file_signatures_project
    ON ast_file_signatures(project_root);
