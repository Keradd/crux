-- CRUX initial schema (v1).
-- Designed in `docs/CRUX-DESIGN.md` Section 5.
--
-- This migration creates the core tables for layers that exist in the
-- foundation/Phase-1 build:
--   * read_cache         (Layer 4)
--   * telemetry          (Layer 9, but used immediately for measurement)
--   * feature_flags      (cross-layer)
--   * state              (cross-layer key/value)
--
-- Layers 5/6/8 schemas land in later migrations as those crates ship,
-- so legacy DBs upgrade cleanly.

CREATE TABLE IF NOT EXISTS read_cache (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id           TEXT NOT NULL,
    session_id         TEXT NOT NULL,
    project_root       TEXT NOT NULL,
    file_path          TEXT NOT NULL,
    mtime_epoch        REAL NOT NULL,
    offset             INTEGER NOT NULL DEFAULT 0,
    limit_lines        INTEGER NOT NULL DEFAULT 0,
    tokens_est         INTEGER NOT NULL DEFAULT 0,
    read_count         INTEGER NOT NULL DEFAULT 1,
    digest             TEXT,
    last_access_epoch  REAL NOT NULL,
    created_at_epoch   INTEGER NOT NULL,
    updated_at_epoch   INTEGER NOT NULL,
    UNIQUE(agent_id, session_id, project_root, file_path, offset, limit_lines)
);

CREATE INDEX IF NOT EXISTS idx_read_cache_lru
    ON read_cache(last_access_epoch);

CREATE INDEX IF NOT EXISTS idx_read_cache_project
    ON read_cache(project_root, file_path);

CREATE TABLE IF NOT EXISTS telemetry (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    project_root        TEXT,
    layer               TEXT NOT NULL,        -- l1..l10
    feature             TEXT NOT NULL,        -- e.g. "delta_read", "bash_filter:git-status"
    agent_id            TEXT,
    session_id          TEXT,
    command_pattern     TEXT,
    original_tokens     INTEGER NOT NULL DEFAULT 0,
    compressed_tokens   INTEGER NOT NULL DEFAULT 0,
    savings             INTEGER NOT NULL DEFAULT 0,
    exec_time_ms        INTEGER,
    quality_preserved   INTEGER NOT NULL DEFAULT 1,
    detail              TEXT,
    created_at_epoch    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_telemetry_layer
    ON telemetry(layer, created_at_epoch DESC);

CREATE INDEX IF NOT EXISTS idx_telemetry_project
    ON telemetry(project_root, created_at_epoch DESC);

CREATE TABLE IF NOT EXISTS feature_flags (
    id                  TEXT PRIMARY KEY,
    enabled             INTEGER NOT NULL DEFAULT 0,
    risk                TEXT,                 -- low/medium/high
    status              TEXT,                 -- shipped/beta/deferred
    config_json         TEXT,
    updated_at_epoch    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS state (
    key                 TEXT PRIMARY KEY,
    value               TEXT NOT NULL,
    updated_at_epoch    INTEGER NOT NULL
);
