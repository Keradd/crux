-- Layer 9 coach state: quality scores + loop-detection working set
-- + CLAUDE.md drift history.

CREATE TABLE IF NOT EXISTS quality_scores (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    project_root      TEXT NOT NULL,
    session_id        TEXT,
    score             INTEGER NOT NULL,
    grade             TEXT NOT NULL,
    patterns_good     TEXT,                 -- JSON
    patterns_bad      TEXT,                 -- JSON
    snapshot          TEXT,                 -- JSON
    created_at_epoch  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_qs_project ON quality_scores(project_root, created_at_epoch DESC);

CREATE TABLE IF NOT EXISTS loop_state (
    session_id         TEXT PRIMARY KEY,
    last_user_msgs     TEXT NOT NULL,       -- JSON array, last 4
    last_tool_results  TEXT NOT NULL,       -- JSON array, last 5
    notes_emitted      INTEGER NOT NULL DEFAULT 0,
    updated_at_epoch   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS claude_md_history (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    project_root      TEXT NOT NULL,
    content_hash      TEXT NOT NULL,
    byte_size         INTEGER NOT NULL,
    tokens_est        INTEGER NOT NULL,
    created_at_epoch  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_cmd_project
    ON claude_md_history(project_root, created_at_epoch DESC);

CREATE UNIQUE INDEX IF NOT EXISTS idx_cmd_project_hash
    ON claude_md_history(project_root, content_hash);
