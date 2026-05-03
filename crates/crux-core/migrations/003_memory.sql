-- Layer 8: persistent memory.
-- Schema adapted from nopx/token-savior — simplified for Phase 6.
-- Adds observations + sessions + decay config + reasoning chains
-- (everything `crux remember`/`crux recall` needs).

CREATE TABLE IF NOT EXISTS sessions (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    project_root        TEXT NOT NULL,
    agent_id            TEXT,
    status              TEXT NOT NULL DEFAULT 'active'
                            CHECK (status IN ('active','completed','failed')),
    summary             TEXT,
    symbols_changed     TEXT,                  -- JSON array
    files_changed       TEXT,                  -- JSON array
    events_count        INTEGER NOT NULL DEFAULT 0,
    created_at_epoch    INTEGER NOT NULL,
    completed_at_epoch  INTEGER
);

CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_root);
CREATE INDEX IF NOT EXISTS idx_sessions_status  ON sessions(status);
CREATE INDEX IF NOT EXISTS idx_sessions_epoch   ON sessions(created_at_epoch DESC);

CREATE TABLE IF NOT EXISTS observations (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    project_root        TEXT NOT NULL,
    agent_id            TEXT,
    kind                TEXT NOT NULL CHECK (kind IN (
                          'user','feedback','project','reference',
                          'guardrail','error_pattern','decision','convention'
                        )),
    title               TEXT NOT NULL,
    content             TEXT NOT NULL,
    why                 TEXT,
    how_to_apply        TEXT,
    symbol              TEXT,
    file_path           TEXT,
    tags                TEXT,                  -- JSON array
    importance          INTEGER NOT NULL DEFAULT 5
                            CHECK (importance BETWEEN 1 AND 10),
    relevance_score     REAL NOT NULL DEFAULT 1.0,
    access_count        INTEGER NOT NULL DEFAULT 0,
    content_hash        TEXT NOT NULL,
    archived            INTEGER NOT NULL DEFAULT 0,
    private             INTEGER NOT NULL DEFAULT 0,
    last_accessed_epoch INTEGER,
    created_at_epoch    INTEGER NOT NULL,
    updated_at_epoch    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_obs_project    ON observations(project_root);
CREATE INDEX IF NOT EXISTS idx_obs_kind       ON observations(kind);
CREATE INDEX IF NOT EXISTS idx_obs_symbol     ON observations(symbol);
CREATE INDEX IF NOT EXISTS idx_obs_hash       ON observations(content_hash, project_root);
CREATE INDEX IF NOT EXISTS idx_obs_archived   ON observations(archived);
CREATE INDEX IF NOT EXISTS idx_obs_agent      ON observations(agent_id) WHERE agent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_obs_importance ON observations(importance DESC, relevance_score DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS observations_fts USING fts5(
    title,
    content,
    why,
    how_to_apply,
    tags,
    content='observations',
    content_rowid='id',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS obs_fts_insert AFTER INSERT ON observations BEGIN
  INSERT INTO observations_fts(rowid, title, content, why, how_to_apply, tags)
  VALUES (new.id, new.title, new.content, new.why, new.how_to_apply, new.tags);
END;

CREATE TRIGGER IF NOT EXISTS obs_fts_delete AFTER DELETE ON observations BEGIN
  INSERT INTO observations_fts(observations_fts, rowid, title, content, why, how_to_apply, tags)
  VALUES ('delete', old.id, old.title, old.content, old.why, old.how_to_apply, old.tags);
END;

CREATE TRIGGER IF NOT EXISTS obs_fts_update AFTER UPDATE ON observations BEGIN
  INSERT INTO observations_fts(observations_fts, rowid, title, content, why, how_to_apply, tags)
  VALUES ('delete', old.id, old.title, old.content, old.why, old.how_to_apply, old.tags);
  INSERT INTO observations_fts(rowid, title, content, why, how_to_apply, tags)
  VALUES (new.id, new.title, new.content, new.why, new.how_to_apply, new.tags);
END;

-- Per-kind decay configuration. Numbers come from the token-savior
-- defaults — battle-tested across thousands of sessions.
CREATE TABLE IF NOT EXISTS decay_config (
    kind            TEXT PRIMARY KEY,
    decay_rate      REAL NOT NULL DEFAULT 1.0,
    min_score       REAL NOT NULL DEFAULT 0.1,
    boost_on_access REAL NOT NULL DEFAULT 0.1
);

INSERT OR IGNORE INTO decay_config VALUES ('guardrail',     1.0,   1.0, 0.0);
INSERT OR IGNORE INTO decay_config VALUES ('user',          1.0,   0.8, 0.0);
INSERT OR IGNORE INTO decay_config VALUES ('convention',    1.0,   0.8, 0.0);
INSERT OR IGNORE INTO decay_config VALUES ('feedback',      0.999, 0.5, 0.1);
INSERT OR IGNORE INTO decay_config VALUES ('decision',      0.998, 0.3, 0.1);
INSERT OR IGNORE INTO decay_config VALUES ('error_pattern', 0.997, 0.2, 0.15);
INSERT OR IGNORE INTO decay_config VALUES ('reference',     0.995, 0.2, 0.1);
INSERT OR IGNORE INTO decay_config VALUES ('project',       0.99,  0.1, 0.2);

-- Observation→observation links (related/contradicts/supersedes/consolidation).
CREATE TABLE IF NOT EXISTS observation_links (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id     INTEGER NOT NULL REFERENCES observations(id) ON DELETE CASCADE,
    target_id     INTEGER NOT NULL REFERENCES observations(id) ON DELETE CASCADE,
    link_type     TEXT NOT NULL CHECK (link_type IN
                    ('related','contradicts','supersedes','consolidation')),
    auto_detected INTEGER NOT NULL DEFAULT 0,
    created_at_epoch INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_links_source ON observation_links(source_id);
CREATE INDEX IF NOT EXISTS idx_links_target ON observation_links(target_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique
    ON observation_links(source_id, target_id, link_type);

-- Compressed reasoning traces — agents can replay a prior chain
-- when goals match (saves the discovery work).
CREATE TABLE IF NOT EXISTS reasoning_chains (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    project_root      TEXT NOT NULL,
    goal              TEXT NOT NULL,
    goal_hash         TEXT NOT NULL,
    steps             TEXT NOT NULL,         -- JSON array of {tool,args,observation}
    conclusion        TEXT NOT NULL,
    confidence        REAL NOT NULL DEFAULT 0.8,
    evidence_hash     TEXT,
    access_count      INTEGER NOT NULL DEFAULT 0,
    created_at_epoch  INTEGER NOT NULL,
    expires_at_epoch  INTEGER
);

CREATE INDEX IF NOT EXISTS idx_rc_project ON reasoning_chains(project_root);
CREATE INDEX IF NOT EXISTS idx_rc_hash    ON reasoning_chains(goal_hash);
