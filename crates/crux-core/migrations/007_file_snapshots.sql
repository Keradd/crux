-- Layer 6: per-file content snapshots for Merkle-style incremental reindex.
--
-- `crux reindex` used to re-walk every project file on every call and
-- rely on the `chunks.content_hash` dedup to short-circuit downstream
-- inserts. That still produced chunks for every unchanged file, parsed
-- prose, and re-read every source body. This table lets the reindexer
-- diff the on-disk state against the last committed snapshot so only
-- added/modified paths produce chunks and removed paths get their
-- chunks purged.
--
-- One row per (project_root, file_path). `content_hash` is SHA-256 of
-- the file bytes at index time. `size_bytes` + `mtime_epoch` are
-- captured for diagnostics and to let future callers early-skip the
-- hash step when both are unchanged.

CREATE TABLE IF NOT EXISTS file_snapshots (
    project_root     TEXT NOT NULL,
    file_path        TEXT NOT NULL,
    content_hash     TEXT NOT NULL,
    size_bytes       INTEGER NOT NULL,
    mtime_epoch      INTEGER NOT NULL,
    indexed_at_epoch INTEGER NOT NULL,
    PRIMARY KEY (project_root, file_path)
);

CREATE INDEX IF NOT EXISTS idx_file_snapshots_project
    ON file_snapshots(project_root);
