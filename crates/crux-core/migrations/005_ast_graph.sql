-- Layer 5: code structure graph (tree-sitter AST → SQLite).
--
-- Schema adapted from tirth8205/code-review-graph. Node = a logical
-- code unit (file/class/function/method/type/test/module/constant).
-- Edge = a directed relationship between two qualified names
-- (CALLS / IMPORTS_FROM / INHERITS / IMPLEMENTS / CONTAINS /
--  TESTED_BY / DEPENDS_ON / REFERENCES).
--
-- `confidence_tier` reflects how the edge was derived:
--   EXTRACTED — straight from the AST (highest)
--   RESOLVED  — resolved via name lookup
--   INFERRED  — best-effort guess

CREATE TABLE IF NOT EXISTS ast_nodes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    project_root    TEXT NOT NULL,
    kind            TEXT NOT NULL,        -- File/Class/Function/Method/Type/Test/Module/Constant
    name            TEXT NOT NULL,
    qualified_name  TEXT NOT NULL,
    file_path       TEXT NOT NULL,
    line_start      INTEGER NOT NULL DEFAULT 0,
    line_end        INTEGER NOT NULL DEFAULT 0,
    language        TEXT,
    parent_qn       TEXT,
    signature       TEXT,
    is_test         INTEGER NOT NULL DEFAULT 0,
    file_hash       TEXT NOT NULL DEFAULT '',
    extra           TEXT NOT NULL DEFAULT '{}',
    updated_at_epoch INTEGER NOT NULL,
    UNIQUE(project_root, qualified_name)
);

CREATE INDEX IF NOT EXISTS idx_nodes_file ON ast_nodes(project_root, file_path);
CREATE INDEX IF NOT EXISTS idx_nodes_kind ON ast_nodes(project_root, kind);
CREATE INDEX IF NOT EXISTS idx_nodes_name ON ast_nodes(project_root, name);

CREATE TABLE IF NOT EXISTS ast_edges (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    project_root    TEXT NOT NULL,
    kind            TEXT NOT NULL,        -- CALLS/IMPORTS_FROM/...
    source_qn       TEXT NOT NULL,
    target_qn       TEXT NOT NULL,
    file_path       TEXT NOT NULL,
    line            INTEGER NOT NULL DEFAULT 0,
    confidence      REAL NOT NULL DEFAULT 1.0,
    confidence_tier TEXT NOT NULL DEFAULT 'EXTRACTED',
    extra           TEXT NOT NULL DEFAULT '{}',
    updated_at_epoch INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_edges_source     ON ast_edges(project_root, source_qn);
CREATE INDEX IF NOT EXISTS idx_edges_target     ON ast_edges(project_root, target_qn);
CREATE INDEX IF NOT EXISTS idx_edges_target_kind
    ON ast_edges(project_root, target_qn, kind);
CREATE INDEX IF NOT EXISTS idx_edges_source_kind
    ON ast_edges(project_root, source_qn, kind);
CREATE INDEX IF NOT EXISTS idx_edges_file       ON ast_edges(project_root, file_path);
