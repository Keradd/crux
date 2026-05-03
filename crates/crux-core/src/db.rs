//! SQLite wrapper with WAL mode and a numbered-migration runner.
//!
//! Migrations live in `crates/crux-core/migrations/NNN_name.sql` and are
//! embedded into the binary at compile time. They run in numeric order and
//! are tracked in the `crux_migrations` table so a reopened DB only applies
//! new ones.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};
use tracing::debug;

use crate::error::{CruxError, Result};

/// Single embedded migration definition.
struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

/// Embedded list of migrations applied in order. Append new entries at the
/// bottom; never edit or reorder existing ones.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("../migrations/001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "delta_cache",
        sql: include_str!("../migrations/002_delta_cache.sql"),
    },
    Migration {
        version: 3,
        name: "memory",
        sql: include_str!("../migrations/003_memory.sql"),
    },
    Migration {
        version: 4,
        name: "coach",
        sql: include_str!("../migrations/004_coach.sql"),
    },
    Migration {
        version: 5,
        name: "ast_graph",
        sql: include_str!("../migrations/005_ast_graph.sql"),
    },
    Migration {
        version: 6,
        name: "hybrid_search",
        sql: include_str!("../migrations/006_hybrid_search.sql"),
    },
    Migration {
        version: 7,
        name: "file_snapshots",
        sql: include_str!("../migrations/007_file_snapshots.sql"),
    },
    Migration {
        version: 8,
        name: "file_snapshots_scope",
        sql: include_str!("../migrations/008_file_snapshots_scope.sql"),
    },
    Migration {
        version: 9,
        name: "ast_file_signatures",
        sql: include_str!("../migrations/009_ast_file_signatures.sql"),
    },
    Migration {
        version: 10,
        name: "turn_log",
        sql: include_str!("../migrations/010_turn_log.sql"),
    },
];

/// Open the database at `path` (creating parent dirs and the file as
/// needed), apply pending migrations, and return a ready-to-use connection.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CruxError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;

    apply_pragmas(&conn)?;
    apply_migrations(&conn)?;
    Ok(conn)
}

/// Open an in-memory database, primarily for tests.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    apply_pragmas(&conn)?;
    apply_migrations(&conn)?;
    Ok(conn)
}

fn apply_pragmas(conn: &Connection) -> Result<()> {
    // WAL improves concurrent reader/writer performance and is what every
    // reference repo (rtk, crg, alex, token-savior) settled on.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn apply_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS crux_migrations (
            version    INTEGER PRIMARY KEY,
            name       TEXT NOT NULL,
            applied_at INTEGER NOT NULL
        );
        "#,
    )?;

    let current: u32 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM crux_migrations",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    for m in MIGRATIONS {
        if m.version <= current {
            continue;
        }
        debug!(version = m.version, name = m.name, "applying migration");
        conn.execute_batch(m.sql)
            .map_err(|e| CruxError::Migration {
                version: m.version,
                message: e.to_string(),
            })?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO crux_migrations (version, name, applied_at) VALUES (?, ?, ?)",
            params![m.version, m.name, now],
        )?;
    }

    Ok(())
}

/// Convenience getter for the resolved DB path according to global config.
/// Callers may override (`Config.general.db_path`); this is the default.
pub fn default_db_path() -> Result<PathBuf> {
    crate::paths::db_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once() {
        let conn = open_in_memory().unwrap();
        let count: u32 = conn
            .query_row("SELECT COUNT(*) FROM crux_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, MIGRATIONS.len() as u32);
    }

    #[test]
    fn read_cache_table_exists() {
        let conn = open_in_memory().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='read_cache'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn open_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("crux.sqlite");
        let conn = open(&p).unwrap();
        drop(conn);
        assert!(p.exists());
    }
}
