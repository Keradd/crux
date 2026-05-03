-- Layer 4 delta-mode storage.
--
-- We add a `body` column on `read_cache` so the manager can compute a
-- line-diff against the previous body when mtime changes. zstd-compressed
-- bytes; null when the file exceeded the per-entry budget on the previous
-- read. `body_size` is the original (uncompressed) byte length.

ALTER TABLE read_cache ADD COLUMN body BLOB;
ALTER TABLE read_cache ADD COLUMN body_size INTEGER NOT NULL DEFAULT 0;
