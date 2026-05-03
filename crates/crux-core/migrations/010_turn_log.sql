-- Layer 11: conversation digest.
--
-- Tracks per-tool-call "turn events" produced by the agent (via the
-- `crux hook post-tool` path or direct MCP dispatch). Periodically rolls
-- them up into compact `turn_digests` so old turns can be archived from
-- the conversation context window and re-fetched on demand instead.
--
-- Both tables are session-scoped; project_root is also stored so a
-- multi-project DB can scope queries cleanly.

CREATE TABLE IF NOT EXISTS turn_events (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id         TEXT NOT NULL,
    project_root       TEXT NOT NULL,
    agent_id           TEXT,
    tool_name          TEXT NOT NULL,
    target             TEXT,                -- file path / command / query
    status             TEXT NOT NULL DEFAULT 'ok'
                           CHECK (status IN ('ok','err','timeout','blocked','skipped')),
    original_tokens    INTEGER NOT NULL DEFAULT 0,
    compressed_tokens  INTEGER NOT NULL DEFAULT 0,
    summary            TEXT NOT NULL,       -- one-liner human description
    rolled_up_into     INTEGER REFERENCES turn_digests(id) ON DELETE SET NULL,
    created_at_epoch   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_turn_events_session
    ON turn_events(session_id, created_at_epoch);
CREATE INDEX IF NOT EXISTS idx_turn_events_project
    ON turn_events(project_root, created_at_epoch DESC);
CREATE INDEX IF NOT EXISTS idx_turn_events_pending
    ON turn_events(session_id, rolled_up_into) WHERE rolled_up_into IS NULL;

CREATE TABLE IF NOT EXISTS turn_digests (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id         TEXT NOT NULL,
    project_root       TEXT NOT NULL,
    agent_id           TEXT,
    ts_start_epoch     INTEGER NOT NULL,
    ts_end_epoch       INTEGER NOT NULL,
    event_count        INTEGER NOT NULL,
    original_tokens    INTEGER NOT NULL DEFAULT 0,
    compressed_tokens  INTEGER NOT NULL DEFAULT 0,
    summary            TEXT NOT NULL,
    observation_id     INTEGER REFERENCES observations(id) ON DELETE SET NULL,
    created_at_epoch   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_turn_digests_session
    ON turn_digests(session_id, created_at_epoch DESC);
CREATE INDEX IF NOT EXISTS idx_turn_digests_project
    ON turn_digests(project_root, created_at_epoch DESC);
