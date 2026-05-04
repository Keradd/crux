use rusqlite::{params, Connection, OptionalExtension};

use crux_core::error::{CruxError, Result};

use crate::extract::FileTypes;

pub const SCHEMA_VERSION: u32 = 2;

pub fn load(
    conn: &Connection,
    project_root: &str,
    file_path: &str,
    content_hash: &str,
) -> Result<Option<FileTypes>> {
    let row: Option<Vec<u8>> = conn
        .query_row(
            "SELECT payload FROM ast_file_signatures
              WHERE project_root = ?
                AND file_path = ?
                AND content_hash = ?
                AND schema_version = ?",
            params![project_root, file_path, content_hash, SCHEMA_VERSION],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .optional()?;

    match row {
        Some(bytes) => match bincode::deserialize::<FileTypes>(&bytes) {
            Ok(ft) => Ok(Some(ft)),
            Err(e) => {
                tracing::warn!(
                    file = %file_path,
                    error = %e,
                    "ast_file_signatures payload failed to deserialize; purging row"
                );
                let _ = conn.execute(
                    "DELETE FROM ast_file_signatures
                      WHERE project_root = ? AND file_path = ?",
                    params![project_root, file_path],
                );
                Ok(None)
            }
        },
        None => Ok(None),
    }
}

pub fn store(
    conn: &Connection,
    project_root: &str,
    file_path: &str,
    content_hash: &str,
    file_types: &FileTypes,
) -> Result<()> {
    let payload = bincode::serialize(file_types).map_err(|e| {
        CruxError::other(format!(
            "failed to serialize FileTypes for {file_path}: {e}"
        ))
    })?;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO ast_file_signatures
             (project_root, file_path, content_hash, schema_version,
              payload, indexed_at_epoch)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(project_root, file_path) DO UPDATE SET
             content_hash     = excluded.content_hash,
             schema_version   = excluded.schema_version,
             payload          = excluded.payload,
             indexed_at_epoch = excluded.indexed_at_epoch",
        params![
            project_root,
            file_path,
            content_hash,
            SCHEMA_VERSION,
            payload,
            now,
        ],
    )?;
    Ok(())
}

pub fn purge_project(conn: &Connection, project_root: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM ast_file_signatures WHERE project_root = ?",
        params![project_root],
    )?;
    Ok(())
}

pub fn purge_files(conn: &Connection, project_root: &str, file_paths: &[String]) -> Result<()> {
    if file_paths.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "DELETE FROM ast_file_signatures
          WHERE project_root = ? AND file_path = ?",
    )?;
    for path in file_paths {
        stmt.execute(params![project_root, path])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_then_load_round_trips_empty_signatures() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let project = "/proj";
        let file = "src/lib.rs";
        let hash = "abc123";
        let ft = FileTypes::default();

        assert!(load(&conn, project, file, hash).unwrap().is_none());
        store(&conn, project, file, hash, &ft).unwrap();
        let loaded = load(&conn, project, file, hash).unwrap();
        assert!(loaded.is_some(), "round-tripped FileTypes should load back");
    }

    #[test]
    fn load_misses_when_content_hash_changes() {
        let conn = crux_core::db::open_in_memory().unwrap();
        store(&conn, "/p", "a.rs", "h1", &FileTypes::default()).unwrap();
        assert!(load(&conn, "/p", "a.rs", "h1").unwrap().is_some());
        assert!(
            load(&conn, "/p", "a.rs", "h2").unwrap().is_none(),
            "different hash should miss"
        );
    }

    #[test]
    fn store_overwrites_on_conflict() {
        let conn = crux_core::db::open_in_memory().unwrap();
        store(&conn, "/p", "a.rs", "h1", &FileTypes::default()).unwrap();
        store(&conn, "/p", "a.rs", "h2", &FileTypes::default()).unwrap();
        assert!(load(&conn, "/p", "a.rs", "h2").unwrap().is_some());
        assert!(
            load(&conn, "/p", "a.rs", "h1").unwrap().is_none(),
            "old hash row should be replaced by the UPSERT"
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ast_file_signatures WHERE project_root='/p'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn purge_project_drops_only_that_project() {
        let conn = crux_core::db::open_in_memory().unwrap();
        store(&conn, "/p1", "a.rs", "h", &FileTypes::default()).unwrap();
        store(&conn, "/p2", "b.rs", "h", &FileTypes::default()).unwrap();

        purge_project(&conn, "/p1").unwrap();

        assert!(load(&conn, "/p1", "a.rs", "h").unwrap().is_none());
        assert!(load(&conn, "/p2", "b.rs", "h").unwrap().is_some());
    }

    #[test]
    fn purge_files_drops_only_listed_paths() {
        let conn = crux_core::db::open_in_memory().unwrap();
        store(&conn, "/p", "a.rs", "h", &FileTypes::default()).unwrap();
        store(&conn, "/p", "b.rs", "h", &FileTypes::default()).unwrap();
        store(&conn, "/p", "c.rs", "h", &FileTypes::default()).unwrap();

        purge_files(&conn, "/p", &["a.rs".to_string(), "c.rs".to_string()]).unwrap();

        assert!(load(&conn, "/p", "a.rs", "h").unwrap().is_none());
        assert!(load(&conn, "/p", "b.rs", "h").unwrap().is_some());
        assert!(load(&conn, "/p", "c.rs", "h").unwrap().is_none());
    }
}
