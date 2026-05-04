//! Pin / prefetch support for the L4 read cache.
//!
//! Some files are part of the agent's *startup context* and re-reading
//! them every session burns thousands of tokens for no benefit:
//!
//!   * OpenClaw bundle: `MEMORY.md`, `AGENTS.md`, `SOUL.md`, `USER.md`,
//!     `TOOLS.md`, `IDENTITY.md`, `HEARTBEAT.md`
//!   * Claude Code bundle: `CLAUDE.md`
//!
//! `prefetch_pinned` resolves the user-configured filenames against the
//! project root and a list of extra search dirs (typically
//! `~/.openclaw`, `~/.claude`), inserts a cache row for each hit, and
//! flips `pinned = 1`. Once pinned, the row is treated like any other
//! cache row by the rest of the manager, except future eviction
//! policies must skip it.
//!
//! This module owns *only* the pin-flag state machine; the actual
//! cache decision pipeline lives in `lib.rs`.

use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};

use crux_core::error::Result;
use crux_core::paths::expand_user_path;
use crux_core::telemetry;

use crate::ReadCacheManager;

/// Outcome of a `pin` / `unpin` mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinReport {
    /// Number of cache rows whose `pinned` flag flipped.
    pub changed: usize,
    /// Number of rows that already had the desired flag.
    pub unchanged: usize,
}

/// Outcome of a `prefetch_pinned` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrefetchReport {
    /// Files that existed on disk and were either inserted or updated.
    pub pinned: Vec<PathBuf>,
    /// Files we looked for but didn't find under any candidate dir.
    pub missing: Vec<String>,
    /// Bytes pre-warmed into the cache (sum of `body_size` written).
    pub bytes_cached: u64,
}

impl<'c> ReadCacheManager<'c> {
    /// Mark an existing cache entry pinned. If the file isn't yet
    /// cached, returns `PinReport { changed: 0, unchanged: 0 }`. Use
    /// [`Self::prefetch_pinned`] to insert + pin in one shot.
    pub fn pin(
        &self,
        agent_id: &str,
        session_id: &str,
        project_root: &Path,
        file_path: &Path,
    ) -> Result<PinReport> {
        self.set_pinned(agent_id, session_id, project_root, file_path, true)
    }

    /// Clear the pinned flag on a cache entry. No-op if the row is
    /// missing.
    pub fn unpin(
        &self,
        agent_id: &str,
        session_id: &str,
        project_root: &Path,
        file_path: &Path,
    ) -> Result<PinReport> {
        self.set_pinned(agent_id, session_id, project_root, file_path, false)
    }

    fn set_pinned(
        &self,
        agent_id: &str,
        session_id: &str,
        project_root: &Path,
        file_path: &Path,
        flag: bool,
    ) -> Result<PinReport> {
        let pr = project_root.to_string_lossy().to_string();
        let fp = absolutize_pin(file_path).to_string_lossy().to_string();
        let target: i64 = if flag { 1 } else { 0 };
        let current: Option<i64> = self
            .conn()
            .query_row(
                "SELECT pinned FROM read_cache
                 WHERE agent_id = ? AND session_id = ? AND project_root = ?
                   AND file_path = ?",
                params![agent_id, session_id, &pr, &fp],
                |r| r.get(0),
            )
            .optional()?;
        let mut changed = 0usize;
        let mut unchanged = 0usize;
        match current {
            Some(now) if now == target => unchanged += 1,
            Some(_) => {
                self.conn().execute(
                    "UPDATE read_cache SET pinned = ?, updated_at_epoch = ?
                     WHERE agent_id = ? AND session_id = ? AND project_root = ?
                       AND file_path = ?",
                    params![
                        target,
                        chrono::Utc::now().timestamp(),
                        agent_id,
                        session_id,
                        &pr,
                        &fp,
                    ],
                )?;
                changed += 1;
            }
            None => {}
        }
        Ok(PinReport { changed, unchanged })
    }

    /// Resolve `pinned_files` (filename basenames) against the project
    /// root + each entry of `extra_search_dirs`, insert a cache row for
    /// every match, and flip `pinned = 1`. Repeat invocations are
    /// idempotent — already-cached rows just have their `pinned` flag
    /// asserted and last-access bumped, with no body re-write.
    ///
    /// `extra_search_dirs` are interpreted with [`expand_user_path`] so
    /// callers can pass `"~/.openclaw"` directly.
    pub fn prefetch_pinned(
        &self,
        agent_id: &str,
        session_id: &str,
        project_root: &Path,
        pinned_files: &[String],
        extra_search_dirs: &[String],
    ) -> Result<PrefetchReport> {
        let mut report = PrefetchReport::default();
        if pinned_files.is_empty() {
            return Ok(report);
        }

        // Build the search path list once: project_root + each expanded extra.
        let mut dirs: Vec<PathBuf> = Vec::with_capacity(extra_search_dirs.len() + 1);
        dirs.push(project_root.to_path_buf());
        for raw in extra_search_dirs {
            if let Some(p) = expand_user_path(raw) {
                // Resolve project-relative search dirs against project_root
                // so `.openclaw` finds `<project>/.openclaw`.
                let resolved = if p.is_absolute() {
                    p
                } else {
                    project_root.join(&p)
                };
                dirs.push(resolved);
            }
        }

        let now_epoch = chrono::Utc::now().timestamp();
        for name in pinned_files {
            // Defense-in-depth: refuse anything that smells like a path
            // traversal attempt. `pinned_files` is supposed to be a list
            // of pure basenames; complex paths would let a misconfigured
            // entry pull in arbitrary disk content.
            if name.contains('/') || name.contains('\\') || name == ".." {
                continue;
            }

            let mut found = false;
            for dir in &dirs {
                let candidate = dir.join(name);
                if !candidate.is_file() {
                    continue;
                }
                let abs = absolutize_pin(&candidate);
                let bytes =
                    self.upsert_pinned_row(agent_id, session_id, project_root, &abs, now_epoch)?;
                report.bytes_cached += bytes as u64;
                report.pinned.push(abs);
                found = true;
                break; // first hit wins
            }
            if !found {
                report.missing.push(name.clone());
            }
        }

        // One telemetry event per prefetch invocation so dashboards can
        // see "session warm-up cached N files / X bytes".
        if !report.pinned.is_empty() {
            let detail = format!(
                "prefetch:{}_files,{}_bytes",
                report.pinned.len(),
                report.bytes_cached
            );
            let project_root_s = project_root.to_string_lossy().to_string();
            let _ = telemetry::record(
                self.conn(),
                &telemetry::Event {
                    project_root: Some(&project_root_s),
                    layer: "l4",
                    feature: "pinned_prefetch",
                    agent_id: Some(agent_id),
                    session_id: Some(session_id),
                    command_pattern: Some("Read"),
                    original_tokens: 0,
                    compressed_tokens: 0,
                    exec_time_ms: None,
                    quality_preserved: true,
                    detail: Some(&detail),
                },
            );
        }

        Ok(report)
    }

    /// Insert (or refresh) a pinned cache row for a known-existing file.
    /// Returns the cached body size (0 if no body was stored).
    fn upsert_pinned_row(
        &self,
        agent_id: &str,
        session_id: &str,
        project_root: &Path,
        abs: &Path,
        now_epoch: i64,
    ) -> Result<i64> {
        let project_root_s = project_root.to_string_lossy().to_string();
        let file_path_s = abs.to_string_lossy().to_string();

        let mtime = std::fs::metadata(abs)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let body = std::fs::read(abs).ok();
        let body_size = body.as_ref().map(|b| b.len() as i64).unwrap_or(0);
        let tokens_est = std::fs::metadata(abs)
            .map(|m| crux_core::tokens::estimate_from_bytes(m.len()) as i64)
            .unwrap_or(0);

        // Try update first (covers the idempotent re-prefetch path); if
        // the row doesn't exist yet, fall back to insert. Two queries is
        // simpler than a CTE here and SQLite handles both fast.
        let updated = self.conn().execute(
            "UPDATE read_cache
             SET mtime_epoch = ?, tokens_est = ?, pinned = 1,
                 last_access_epoch = ?, updated_at_epoch = ?,
                 body = COALESCE(body, ?), body_size = MAX(body_size, ?)
             WHERE agent_id = ? AND session_id = ? AND project_root = ?
               AND file_path = ? AND offset = 0 AND limit_lines = 0",
            params![
                mtime,
                tokens_est,
                now_epoch as f64,
                now_epoch,
                body.as_deref(),
                body_size,
                agent_id,
                session_id,
                &project_root_s,
                &file_path_s,
            ],
        )?;
        if updated > 0 {
            return Ok(body_size);
        }

        self.conn().execute(
            r#"INSERT INTO read_cache
               (agent_id, session_id, project_root, file_path, mtime_epoch,
                offset, limit_lines, tokens_est, read_count, last_access_epoch,
                created_at_epoch, updated_at_epoch, body, body_size, pinned)
               VALUES (?, ?, ?, ?, ?, 0, 0, ?, 1, ?, ?, ?, ?, ?, 1)"#,
            params![
                agent_id,
                session_id,
                &project_root_s,
                &file_path_s,
                mtime,
                tokens_est,
                now_epoch as f64,
                now_epoch,
                now_epoch,
                body.as_deref(),
                body_size,
            ],
        )?;
        Ok(body_size)
    }

    /// Iterate pinned cache rows for a session — useful for diagnostics
    /// and for the future eviction policy. Returns absolute file paths.
    pub fn list_pinned(
        &self,
        agent_id: &str,
        session_id: &str,
        project_root: &Path,
    ) -> Result<Vec<PathBuf>> {
        let pr = project_root.to_string_lossy().to_string();
        let mut stmt = self.conn().prepare(
            "SELECT file_path FROM read_cache
             WHERE agent_id = ? AND session_id = ? AND project_root = ? AND pinned = 1
             ORDER BY file_path",
        )?;
        let rows = stmt.query_map(params![agent_id, session_id, &pr], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(PathBuf::from(r?));
        }
        Ok(out)
    }
}

fn absolutize_pin(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::ReadCacheManager;

    fn fixture_db() -> rusqlite::Connection {
        crux_core::db::open_in_memory().unwrap()
    }

    #[test]
    fn prefetch_finds_files_in_project_root() {
        let conn = fixture_db();
        let mgr = ReadCacheManager::new(&conn);
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(dir.path().join("MEMORY.md"), "# project memory\n").unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# agents\n").unwrap();

        let pinned = vec!["MEMORY.md".into(), "AGENTS.md".into(), "MISSING.md".into()];
        let report = mgr
            .prefetch_pinned("a", "s", dir.path(), &pinned, &[])
            .unwrap();

        assert_eq!(report.pinned.len(), 2);
        assert_eq!(report.missing, vec!["MISSING.md".to_string()]);
        assert!(report.bytes_cached > 0);

        let listed = mgr.list_pinned("a", "s", dir.path()).unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn prefetch_falls_back_to_extra_search_dirs() {
        let conn = fixture_db();
        let mgr = ReadCacheManager::new(&conn);
        let project = tempfile::tempdir().unwrap();
        let openclaw = tempfile::tempdir().unwrap();

        // File only exists in the simulated ~/.openclaw, NOT under the project.
        std::fs::write(openclaw.path().join("SOUL.md"), "soul\n").unwrap();

        let extras = vec![openclaw.path().to_string_lossy().to_string()];
        let report = mgr
            .prefetch_pinned("a", "s", project.path(), &["SOUL.md".into()], &extras)
            .unwrap();

        assert_eq!(report.pinned.len(), 1);
        assert!(report.pinned[0].ends_with("SOUL.md"));
        assert!(report.missing.is_empty());
    }

    #[test]
    fn prefetch_is_idempotent() {
        let conn = fixture_db();
        let mgr = ReadCacheManager::new(&conn);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MEMORY.md"), "v1\n").unwrap();

        let pinned = vec!["MEMORY.md".into()];
        let r1 = mgr
            .prefetch_pinned("a", "s", dir.path(), &pinned, &[])
            .unwrap();
        let r2 = mgr
            .prefetch_pinned("a", "s", dir.path(), &pinned, &[])
            .unwrap();
        assert_eq!(r1.pinned.len(), 1);
        assert_eq!(r2.pinned.len(), 1);
        // Still exactly one row in the table.
        assert_eq!(mgr.count().unwrap(), 1);
    }

    #[test]
    fn pin_unpin_round_trips() {
        let conn = fixture_db();
        let mgr = ReadCacheManager::new(&conn);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("CLAUDE.md");
        std::fs::write(&p, "x\n").unwrap();

        // Insert via a regular read so an entry exists.
        let ev = crate::ReadEvent {
            agent_id: "a",
            session_id: "s",
            project_root: dir.path(),
            file_path: &p,
            offset: 0,
            limit: 0,
        };
        mgr.check(&ev).unwrap();

        let r = mgr.pin("a", "s", dir.path(), &p).unwrap();
        assert_eq!(r.changed, 1);
        let again = mgr.pin("a", "s", dir.path(), &p).unwrap();
        assert_eq!(again.changed, 0);
        assert_eq!(again.unchanged, 1);

        let off = mgr.unpin("a", "s", dir.path(), &p).unwrap();
        assert_eq!(off.changed, 1);
    }

    #[test]
    fn prefetch_rejects_path_traversal_basenames() {
        let conn = fixture_db();
        let mgr = ReadCacheManager::new(&conn);
        let dir = tempfile::tempdir().unwrap();

        let pinned = vec!["../etc/passwd".into(), "..".into(), "ok.md".into()];
        std::fs::write(dir.path().join("ok.md"), "ok\n").unwrap();

        let r = mgr
            .prefetch_pinned("a", "s", dir.path(), &pinned, &[])
            .unwrap();
        // Only `ok.md` should be cached; the two malformed entries are
        // silently skipped (and not reported as `missing`, since they're
        // ill-formed rather than absent).
        assert_eq!(r.pinned.len(), 1);
        assert!(r.pinned[0].ends_with("ok.md"));
    }
}
