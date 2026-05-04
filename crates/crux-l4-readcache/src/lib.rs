pub mod contextignore;
pub mod delta;
pub mod pin;

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crux_core::error::{CruxError, Result};
use crux_core::telemetry;

pub use contextignore::ContextIgnore;
pub use delta::{compute_delta, DeltaResult};
pub use pin::{PinReport, PrefetchReport};

#[derive(Debug, Clone, PartialEq)]
pub enum CacheDecision {
    Allow,
    Redundant {
        digest: String,
        read_count: i64,
    },
    Delta {
        summary: String,
        body: String,
        read_count: i64,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct ReadEvent<'a> {
    pub agent_id: &'a str,
    pub session_id: &'a str,
    pub project_root: &'a Path,
    pub file_path: &'a Path,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    pub contextignore: Option<ContextIgnore>,
    pub delta_max_bytes: Option<u64>,
}

pub struct ReadCacheManager<'c> {
    conn: &'c Connection,
}

impl<'c> ReadCacheManager<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    pub(crate) fn conn(&self) -> &Connection {
        self.conn
    }

    pub fn check(&self, ev: &ReadEvent<'_>) -> Result<CacheDecision> {
        self.check_with(ev, &CheckOptions::default())
    }

    pub fn check_with(&self, ev: &ReadEvent<'_>, opts: &CheckOptions) -> Result<CacheDecision> {
        let abs = absolutize(ev.file_path);

        if let Some(ci) = &opts.contextignore {
            if !ci.is_empty() && ci.matches(&abs) {
                let project_root = ev.project_root.to_string_lossy().to_string();
                let _ = telemetry::record(
                    self.conn,
                    &telemetry::Event {
                        project_root: Some(&project_root),
                        layer: "l4",
                        feature: "contextignore_block",
                        agent_id: Some(ev.agent_id),
                        session_id: Some(ev.session_id),
                        command_pattern: Some("Read"),
                        original_tokens: 0,
                        compressed_tokens: 0,
                        exec_time_ms: None,
                        quality_preserved: true,
                        detail: Some(abs.file_name().and_then(|s| s.to_str()).unwrap_or("?")),
                    },
                );
                return Ok(CacheDecision::Blocked {
                    reason: format!(
                        "matched .contextignore pattern (file: {})",
                        abs.file_name().and_then(|s| s.to_str()).unwrap_or("?")
                    ),
                });
            }
        }

        let mtime = match mtime_of(&abs) {
            Ok(t) => t,
            Err(_) => {
                return Ok(CacheDecision::Allow);
            }
        };

        let project_root = ev.project_root.to_string_lossy().to_string();
        let file_path = abs.to_string_lossy().to_string();

        let row = self.lookup(
            ev.agent_id,
            ev.session_id,
            &project_root,
            &file_path,
            ev.offset as i64,
            ev.limit as i64,
        )?;

        let now_epoch = chrono::Utc::now().timestamp();

        match row {
            Some(existing) if mtimes_equal(existing.mtime_epoch, mtime) => {
                let digest = self.ensure_digest(existing.id, &abs, existing.digest.as_deref())?;
                self.bump_access(existing.id, now_epoch)?;

                let read_count = existing.read_count + 1;
                let _ = telemetry::record(
                    self.conn,
                    &telemetry::Event {
                        project_root: Some(&project_root),
                        layer: "l4",
                        feature: "read_cache_hit",
                        agent_id: Some(ev.agent_id),
                        session_id: Some(ev.session_id),
                        command_pattern: Some("Read"),
                        original_tokens: existing.tokens_est,
                        compressed_tokens: crux_core::tokens::estimate(&digest) as i64,
                        exec_time_ms: None,
                        quality_preserved: true,
                        detail: Some(&format!("redundant_read_{}", read_count)),
                    },
                );

                Ok(CacheDecision::Redundant { digest, read_count })
            }
            Some(existing) => {
                let read_count = existing.read_count + 1;
                let is_full_read = ev.offset == 0 && ev.limit == 0;
                let delta = if is_full_read {
                    self.try_delta(&existing, &abs, opts)?
                } else {
                    None
                };

                let new_body = if is_full_read {
                    body_to_cache(&abs, opts.delta_max_bytes)
                } else {
                    None
                };
                let tokens_est = estimate_file_tokens(&abs);
                self.update_after_change(
                    existing.id,
                    mtime,
                    tokens_est,
                    new_body.as_deref(),
                    now_epoch,
                )?;

                if let Some(d) = delta {
                    let _ = telemetry::record(
                        self.conn,
                        &telemetry::Event {
                            project_root: Some(&project_root),
                            layer: "l4",
                            feature: "delta_read",
                            agent_id: Some(ev.agent_id),
                            session_id: Some(ev.session_id),
                            command_pattern: Some("Read"),
                            original_tokens: existing.tokens_est,
                            compressed_tokens: crux_core::tokens::estimate(&d.body) as i64,
                            exec_time_ms: None,
                            quality_preserved: !d.fallback,
                            detail: Some(&d.summary),
                        },
                    );
                    return Ok(CacheDecision::Delta {
                        summary: d.summary,
                        body: d.body,
                        read_count,
                    });
                }
                Ok(CacheDecision::Allow)
            }
            None => {
                let tokens_est = estimate_file_tokens(&abs);
                let body = body_to_cache(&abs, opts.delta_max_bytes);
                self.insert(
                    ev.agent_id,
                    ev.session_id,
                    &project_root,
                    &file_path,
                    mtime,
                    ev.offset as i64,
                    ev.limit as i64,
                    tokens_est,
                    body.as_deref(),
                    now_epoch,
                )?;
                Ok(CacheDecision::Allow)
            }
        }
    }

    fn try_delta(
        &self,
        existing: &CacheRow,
        abs: &Path,
        opts: &CheckOptions,
    ) -> Result<Option<DeltaResult>> {
        let Some(budget) = opts.delta_max_bytes else {
            return Ok(None);
        };
        let Some(body_bytes) = self.fetch_body(existing.id)? else {
            return Ok(None);
        };
        let old = match String::from_utf8(body_bytes) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };

        let stat = match std::fs::metadata(abs) {
            Ok(m) => m,
            Err(_) => return Ok(None),
        };
        if stat.len() > budget {
            return Ok(None);
        }
        let Ok(new) = std::fs::read_to_string(abs) else {
            return Ok(None);
        };
        let d = compute_delta(&old, &new);
        if d.fallback {
            return Ok(Some(d));
        }
        Ok(Some(d))
    }

    pub fn invalidate(
        &self,
        agent_id: &str,
        session_id: &str,
        project_root: &Path,
        file_path: &Path,
    ) -> Result<()> {
        let project_root = project_root.to_string_lossy();
        let file_path = absolutize(file_path).to_string_lossy().to_string();
        self.conn.execute(
            "DELETE FROM read_cache
             WHERE agent_id = ? AND session_id = ? AND project_root = ? AND file_path = ?",
            params![agent_id, session_id, &project_root.to_string(), &file_path],
        )?;
        Ok(())
    }

    pub fn count(&self) -> Result<i64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM read_cache", [], |r| r.get(0))?;
        Ok(n)
    }

    fn lookup(
        &self,
        agent_id: &str,
        session_id: &str,
        project_root: &str,
        file_path: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Option<CacheRow>> {
        let row = self
            .conn
            .query_row(
                r#"SELECT id, mtime_epoch, tokens_est, read_count, digest
                   FROM read_cache
                   WHERE agent_id = ? AND session_id = ? AND project_root = ?
                     AND file_path = ? AND offset = ? AND limit_lines = ?"#,
                params![agent_id, session_id, project_root, file_path, offset, limit],
                |row| {
                    Ok(CacheRow {
                        id: row.get(0)?,
                        mtime_epoch: row.get(1)?,
                        tokens_est: row.get(2)?,
                        read_count: row.get(3)?,
                        digest: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        agent_id: &str,
        session_id: &str,
        project_root: &str,
        file_path: &str,
        mtime_epoch: f64,
        offset: i64,
        limit: i64,
        tokens_est: i64,
        body: Option<&[u8]>,
        now_epoch: i64,
    ) -> Result<()> {
        let body_size = body.map(|b| b.len() as i64).unwrap_or(0);
        self.conn.execute(
            r#"INSERT INTO read_cache
               (agent_id, session_id, project_root, file_path, mtime_epoch,
                offset, limit_lines, tokens_est, read_count, last_access_epoch,
                created_at_epoch, updated_at_epoch, body, body_size)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?)"#,
            params![
                agent_id,
                session_id,
                project_root,
                file_path,
                mtime_epoch,
                offset,
                limit,
                tokens_est,
                now_epoch as f64,
                now_epoch,
                now_epoch,
                body,
                body_size
            ],
        )?;
        Ok(())
    }

    fn update_after_change(
        &self,
        id: i64,
        mtime_epoch: f64,
        tokens_est: i64,
        body: Option<&[u8]>,
        now_epoch: i64,
    ) -> Result<()> {
        let body_size = body.map(|b| b.len() as i64).unwrap_or(0);
        self.conn.execute(
            r#"UPDATE read_cache
               SET mtime_epoch = ?, tokens_est = ?, digest = NULL,
                   read_count = read_count + 1,
                   last_access_epoch = ?, updated_at_epoch = ?,
                   body = ?, body_size = ?
               WHERE id = ?"#,
            params![
                mtime_epoch,
                tokens_est,
                now_epoch as f64,
                now_epoch,
                body,
                body_size,
                id
            ],
        )?;
        Ok(())
    }

    fn fetch_body(&self, id: i64) -> Result<Option<Vec<u8>>> {
        let row: Option<Option<Vec<u8>>> = self
            .conn
            .query_row(
                "SELECT body FROM read_cache WHERE id = ?",
                params![id],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?;
        Ok(row.flatten())
    }

    fn bump_access(&self, id: i64, now_epoch: i64) -> Result<()> {
        self.conn.execute(
            r#"UPDATE read_cache
               SET read_count = read_count + 1,
                   last_access_epoch = ?,
                   updated_at_epoch = ?
               WHERE id = ?"#,
            params![now_epoch as f64, now_epoch, id],
        )?;
        Ok(())
    }

    fn ensure_digest(&self, id: i64, abs: &Path, existing: Option<&str>) -> Result<String> {
        if let Some(d) = existing {
            if !d.is_empty() {
                return Ok(d.to_string());
            }
        }
        let content = std::fs::read_to_string(abs).unwrap_or_default();
        let digest = structural_digest(abs, &content);
        self.conn.execute(
            "UPDATE read_cache SET digest = ? WHERE id = ?",
            params![&digest, id],
        )?;
        Ok(digest)
    }
}

#[derive(Debug)]
struct CacheRow {
    id: i64,
    mtime_epoch: f64,
    tokens_est: i64,
    read_count: i64,
    digest: Option<String>,
}

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

fn mtime_of(p: &Path) -> Result<f64> {
    let meta = std::fs::metadata(p).map_err(|e| CruxError::Io {
        path: p.to_path_buf(),
        source: e,
    })?;
    let m = meta
        .modified()
        .map_err(|e| CruxError::Io {
            path: p.to_path_buf(),
            source: e,
        })?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CruxError::other("file mtime is before unix epoch"))?;
    Ok(m.as_secs_f64())
}

fn mtimes_equal(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

fn estimate_file_tokens(p: &Path) -> i64 {
    std::fs::metadata(p)
        .map(|m| crux_core::tokens::estimate_from_bytes(m.len()) as i64)
        .unwrap_or(0)
}

fn body_to_cache(p: &Path, max_bytes: Option<u64>) -> Option<Vec<u8>> {
    let budget = max_bytes?;
    let stat = std::fs::metadata(p).ok()?;
    if stat.len() > budget {
        return None;
    }
    std::fs::read(p).ok()
}

pub fn structural_digest(path: &Path, content: &str) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "py" => digest_python(content),
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => digest_javascript(content),
        "rs" => digest_rust(content),
        _ => digest_fallback(content),
    }
}

const MAX_DIGEST_ENTRIES: usize = 50;

fn digest_python(content: &str) -> String {
    let mut out = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        if out.len() >= MAX_DIGEST_ENTRIES {
            break;
        }
        let line = raw.trim_start();
        if line.starts_with("class ") {
            let head = line.split('(').next().unwrap_or(line);
            let head = head.split(':').next().unwrap_or(head);
            out.push(format!("L{}: {}", i + 1, head.trim()));
        } else if line.starts_with("def ") || line.starts_with("async def ") {
            let head = line.split('(').next().unwrap_or(line);
            out.push(format!("L{}: {}", i + 1, head.trim()));
        } else if line.starts_with("import ") || line.starts_with("from ") {
            out.push(format!("L{}: {}", i + 1, line.trim()));
        }
    }
    if out.is_empty() {
        digest_fallback(content)
    } else {
        out.join("\n")
    }
}

fn digest_javascript(content: &str) -> String {
    let mut out = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        if out.len() >= MAX_DIGEST_ENTRIES {
            break;
        }
        let line = raw.trim_start();
        let is_export_struct = line.starts_with("export class ")
            || line.starts_with("export interface ")
            || line.starts_with("export type ")
            || line.starts_with("export enum ")
            || line.starts_with("class ")
            || line.starts_with("interface ")
            || line.starts_with("type ")
            || line.starts_with("enum ");
        let is_func = line.starts_with("export function ")
            || line.starts_with("export async function ")
            || line.starts_with("function ")
            || line.starts_with("async function ");
        let is_export_var = line.starts_with("export const ")
            || line.starts_with("export let ")
            || line.starts_with("export var ")
            || line.starts_with("export default ");

        if is_export_struct || is_func {
            let head = line.split('{').next().unwrap_or(line);
            out.push(format!("L{}: {}", i + 1, head.trim()));
        } else if is_export_var {
            let head = line.split('=').next().unwrap_or(line);
            out.push(format!("L{}: {}", i + 1, head.trim()));
        }
    }
    if out.is_empty() {
        digest_fallback(content)
    } else {
        out.join("\n")
    }
}

fn digest_rust(content: &str) -> String {
    let mut out = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        if out.len() >= MAX_DIGEST_ENTRIES {
            break;
        }
        let line = raw.trim_start();
        let interesting = line.starts_with("pub fn ")
            || line.starts_with("fn ")
            || line.starts_with("pub async fn ")
            || line.starts_with("async fn ")
            || line.starts_with("pub struct ")
            || line.starts_with("struct ")
            || line.starts_with("pub enum ")
            || line.starts_with("enum ")
            || line.starts_with("pub trait ")
            || line.starts_with("trait ")
            || line.starts_with("impl ")
            || line.starts_with("pub mod ")
            || line.starts_with("mod ")
            || line.starts_with("use ");
        if interesting {
            let head = line.split('{').next().unwrap_or(line);
            out.push(format!("L{}: {}", i + 1, head.trim()));
        }
    }
    if out.is_empty() {
        digest_fallback(content)
    } else {
        out.join("\n")
    }
}

fn digest_fallback(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();
    if n <= 6 {
        return format!("{} lines", n);
    }
    let head = lines[..3].join("\n");
    let tail = lines[n - 3..].join("\n");
    format!("{} lines\nfirst 3:\n{}\nlast 3:\n{}", n, head, tail)
}

pub fn content_hash(content: &str) -> String {
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    let bytes = h.finalize();
    hex::encode(&bytes[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_db() -> Connection {
        crux_core::db::open_in_memory().unwrap()
    }

    #[test]
    fn first_read_allowed_then_redundant() {
        let conn = fixture_db();
        let mgr = ReadCacheManager::new(&conn);

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hello.py");
        std::fs::write(&p, "import os\n\nclass A:\n    def b(self): pass\n").unwrap();

        let ev = ReadEvent {
            agent_id: "agent-1",
            session_id: "sess-1",
            project_root: dir.path(),
            file_path: &p,
            offset: 0,
            limit: 0,
        };

        match mgr.check(&ev).unwrap() {
            CacheDecision::Allow => {}
            other => panic!("expected Allow, got {:?}", other),
        }
        match mgr.check(&ev).unwrap() {
            CacheDecision::Redundant { digest, read_count } => {
                assert_eq!(read_count, 2);
                assert!(digest.contains("class A"));
            }
            other => panic!("expected Redundant, got {:?}", other),
        }
    }

    #[test]
    fn modified_file_serves_allow_again() {
        let conn = fixture_db();
        let mgr = ReadCacheManager::new(&conn);

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "v1").unwrap();

        let ev = ReadEvent {
            agent_id: "a",
            session_id: "s",
            project_root: dir.path(),
            file_path: &p,
            offset: 0,
            limit: 0,
        };
        let _ = mgr.check(&ev).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&p, "v2-new").unwrap();

        match mgr.check(&ev).unwrap() {
            CacheDecision::Allow => {}
            other => panic!("expected Allow on changed mtime, got {:?}", other),
        }
    }

    #[test]
    fn invalidate_drops_entry() {
        let conn = fixture_db();
        let mgr = ReadCacheManager::new(&conn);

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.py");
        std::fs::write(&p, "x = 1\n").unwrap();

        let ev = ReadEvent {
            agent_id: "a",
            session_id: "s",
            project_root: dir.path(),
            file_path: &p,
            offset: 0,
            limit: 0,
        };
        mgr.check(&ev).unwrap();
        assert_eq!(mgr.count().unwrap(), 1);

        mgr.invalidate("a", "s", dir.path(), &p).unwrap();
        assert_eq!(mgr.count().unwrap(), 0);
    }

    #[test]
    fn python_digest_finds_class_and_def() {
        let s = "import os\n\nclass Foo:\n    def bar(self):\n        pass\n";
        let d = digest_python(s);
        assert!(d.contains("class Foo"));
        assert!(d.contains("def bar"));
        assert!(d.contains("import os"));
    }

    #[test]
    fn fallback_for_short_file() {
        assert_eq!(digest_fallback("hi"), "1 lines");
    }
}
