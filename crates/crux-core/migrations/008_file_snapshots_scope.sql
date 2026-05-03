-- Partition `file_snapshots` by scope so multiple layers can share the
-- table without stepping on each other.
--
-- Before 008 the table only served Layer 6's chunk reindex. Layer 5 now
-- wants the same Merkle-style skip-unchanged behaviour for
-- `crux index`, but both layers walk overlapping file sets with
-- different retention semantics. Adding a `scope` discriminator lets
-- each layer own its own row per file.
--
-- SQLite cannot alter a PRIMARY KEY in place, so we recreate the table
-- and copy every existing row under the legacy scope `'chunks'`.

CREATE TABLE file_snapshots_new (
    project_root     TEXT NOT NULL,
    scope            TEXT NOT NULL DEFAULT 'chunks',
    file_path        TEXT NOT NULL,
    content_hash     TEXT NOT NULL,
    size_bytes       INTEGER NOT NULL,
    mtime_epoch      INTEGER NOT NULL,
    indexed_at_epoch INTEGER NOT NULL,
    PRIMARY KEY (project_root, scope, file_path)
);

INSERT INTO file_snapshots_new
    (project_root, scope, file_path, content_hash,
     size_bytes, mtime_epoch, indexed_at_epoch)
SELECT project_root, 'chunks', file_path, content_hash,
       size_bytes, mtime_epoch, indexed_at_epoch
  FROM file_snapshots;

DROP TABLE file_snapshots;
ALTER TABLE file_snapshots_new RENAME TO file_snapshots;

CREATE INDEX IF NOT EXISTS idx_file_snapshots_project_scope
    ON file_snapshots(project_root, scope);
