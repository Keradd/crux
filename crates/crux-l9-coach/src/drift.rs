//! CLAUDE.md drift tracker.
//!
//! Goal: notice when the project's CLAUDE.md silently grows or swaps
//! rules between sessions. We hash the file on every coach run, store
//! a history row, and report the delta vs the most-recent prior hash.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crux_core::error::{CruxError, Result};
use crux_core::tokens;

use crate::types::DriftCheckResult;

pub struct DriftTracker<'c> {
    conn: &'c Connection,
}

impl<'c> DriftTracker<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Read `<project>/CLAUDE.md`, hash it, append a history row when
    /// the hash changed. Returns the comparison vs the previous entry.
    pub fn check(&self, project_root: &Path) -> Result<Option<DriftCheckResult>> {
        let path = project_root.join("CLAUDE.md");
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(CruxError::Io {
                    path: path.clone(),
                    source: e,
                })
            }
        };

        let hash = sha256_hex(&bytes);
        let tokens_est = tokens::estimate(std::str::from_utf8(&bytes).unwrap_or_default()) as u32;
        let byte_size = bytes.len() as u64;

        let project_key = project_root.to_string_lossy().to_string();

        let previous: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT content_hash, created_at_epoch FROM claude_md_history
                 WHERE project_root = ? ORDER BY created_at_epoch DESC LIMIT 1",
                params![project_key],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;

        let history_depth: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM claude_md_history WHERE project_root = ?",
            params![project_key],
            |r| r.get::<_, i64>(0).map(|n| n as u32),
        )?;

        let changed = match &previous {
            Some((prev, _)) => *prev != hash,
            None => true,
        };

        if changed {
            let now = chrono::Utc::now().timestamp();
            self.conn.execute(
                r#"INSERT INTO claude_md_history
                     (project_root, content_hash, byte_size, tokens_est, created_at_epoch)
                   VALUES (?, ?, ?, ?, ?)
                   ON CONFLICT(project_root, content_hash) DO NOTHING"#,
                params![project_key, hash, byte_size as i64, tokens_est, now],
            )?;
        }

        Ok(Some(DriftCheckResult {
            changed,
            previous_hash: previous.map(|(h, _)| h),
            current_hash: hash,
            tokens_est,
            byte_size,
            history_depth: if changed {
                history_depth + 1
            } else {
                history_depth
            },
        }))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Connection) {
        (
            tempfile::tempdir().unwrap(),
            crux_core::db::open_in_memory().unwrap(),
        )
    }

    #[test]
    fn missing_file_returns_none() {
        let (dir, conn) = fixture();
        let t = DriftTracker::new(&conn);
        assert!(t.check(dir.path()).unwrap().is_none());
    }

    #[test]
    fn first_check_is_changed() {
        let (dir, conn) = fixture();
        std::fs::write(dir.path().join("CLAUDE.md"), "hello").unwrap();
        let r = DriftTracker::new(&conn).check(dir.path()).unwrap().unwrap();
        assert!(r.changed);
        assert!(r.previous_hash.is_none());
        assert_eq!(r.history_depth, 1);
    }

    #[test]
    fn same_file_reports_unchanged() {
        let (dir, conn) = fixture();
        std::fs::write(dir.path().join("CLAUDE.md"), "hello").unwrap();
        let t = DriftTracker::new(&conn);
        let _ = t.check(dir.path()).unwrap().unwrap();
        let r2 = t.check(dir.path()).unwrap().unwrap();
        assert!(!r2.changed);
        assert!(r2.previous_hash.is_some());
    }

    #[test]
    fn edit_is_reported_as_changed() {
        let (dir, conn) = fixture();
        let p = dir.path().join("CLAUDE.md");
        std::fs::write(&p, "hello").unwrap();
        let t = DriftTracker::new(&conn);
        let r1 = t.check(dir.path()).unwrap().unwrap();
        std::fs::write(&p, "hello world").unwrap();
        let r2 = t.check(dir.path()).unwrap().unwrap();
        assert!(r2.changed);
        assert_eq!(r2.previous_hash.as_ref().unwrap(), &r1.current_hash);
    }
}
