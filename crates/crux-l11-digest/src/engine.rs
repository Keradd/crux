use rusqlite::{params, Connection, OptionalExtension};

use crux_core::error::{CruxError, Result};
use crux_l8_memory::{MemoryEngine, NewObservation, ObservationKind};

use crate::render;
use crate::types::{L11Config, StoredTurnEvent, TurnDigest, TurnEvent, TurnStatus};

#[derive(Debug, Clone)]
pub struct RecordOutcome {
    pub event_id: i64,
    pub auto_compacted: Option<TurnDigest>,
}

pub struct DigestEngine<'c> {
    conn: &'c Connection,
    config: L11Config,
}

impl<'c> DigestEngine<'c> {
    pub fn new(conn: &'c Connection, config: L11Config) -> Self {
        Self { conn, config }
    }

    pub fn default_with_conn(conn: &'c Connection) -> Self {
        Self {
            conn,
            config: L11Config::default(),
        }
    }

    pub fn record(&self, ev: &TurnEvent) -> Result<RecordOutcome> {
        if ev.session_id.trim().is_empty() {
            return Err(CruxError::other("turn event session_id cannot be empty"));
        }
        if ev.tool_name.trim().is_empty() {
            return Err(CruxError::other("turn event tool_name cannot be empty"));
        }
        let summary = if ev.summary.trim().is_empty() {
            default_summary(&ev.tool_name, ev.target.as_deref())
        } else {
            ev.summary.clone()
        };

        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            r#"INSERT INTO turn_events
                 (session_id, project_root, agent_id, tool_name, target,
                  status, original_tokens, compressed_tokens, summary,
                  rolled_up_into, created_at_epoch)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?)"#,
            params![
                ev.session_id,
                ev.project_root,
                ev.agent_id,
                ev.tool_name,
                ev.target,
                ev.status.as_str(),
                ev.original_tokens,
                ev.compressed_tokens,
                summary,
                now,
            ],
        )?;
        let event_id = self.conn.last_insert_rowid();

        let auto = if self.config.auto_compact_every_n > 0 {
            let pending = self.pending_count(&ev.session_id)?;
            if pending >= self.config.auto_compact_every_n as i64 {
                Some(self.compact(&ev.session_id)?)
            } else {
                None
            }
        } else {
            None
        };

        Ok(RecordOutcome {
            event_id,
            auto_compacted: auto,
        })
    }

    pub fn pending_count(&self, session_id: &str) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM turn_events WHERE session_id = ? AND rolled_up_into IS NULL",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(n)
    }

    pub fn summarize(&self, session_id: &str) -> Result<String> {
        let events =
            self.list_pending_events(session_id, self.config.render_max_events as usize)?;
        let lite: Vec<TurnEvent> = events.into_iter().map(stored_to_event).collect();
        Ok(render::render(&lite, self.config.max_summary_tokens))
    }

    pub fn latest_summary(&self, session_id: &str) -> Result<String> {
        if let Some(d) = self.latest_digest(session_id)? {
            let mut out = format!(
                "Latest digest #{} (events {}, {}–{})\n",
                d.id, d.event_count, d.ts_start_epoch, d.ts_end_epoch,
            );
            out.push_str(&d.summary);
            let pending = self.pending_count(session_id)?;
            if pending > 0 {
                out.push_str(&format!("\n\n--- {} pending event(s) ---\n", pending));
                out.push_str(&self.summarize(session_id)?);
            }
            Ok(out)
        } else {
            self.summarize(session_id)
        }
    }

    pub fn compact(&self, session_id: &str) -> Result<TurnDigest> {
        let events = self.list_pending_events(session_id, usize::MAX)?;
        let now = chrono::Utc::now().timestamp();
        let project_root = events
            .first()
            .map(|e| e.project_root.clone())
            .unwrap_or_default();
        let agent_id = events.first().and_then(|e| e.agent_id.clone());
        let ts_start = events.first().map(|e| e.created_at_epoch).unwrap_or(now);
        let ts_end = events.last().map(|e| e.created_at_epoch).unwrap_or(now);
        let total_orig: i64 = events.iter().map(|e| e.original_tokens).sum();
        let total_compressed: i64 = events.iter().map(|e| e.compressed_tokens).sum();
        let event_count = events.len() as i64;

        let lite: Vec<TurnEvent> = events.iter().cloned().map(stored_to_event).collect();
        let summary = render::render(&lite, self.config.max_summary_tokens);

        self.conn.execute(
            r#"INSERT INTO turn_digests
                 (session_id, project_root, agent_id, ts_start_epoch, ts_end_epoch,
                  event_count, original_tokens, compressed_tokens, summary,
                  observation_id, created_at_epoch)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?)"#,
            params![
                session_id,
                project_root,
                agent_id,
                ts_start,
                ts_end,
                event_count,
                total_orig,
                total_compressed,
                summary,
                now,
            ],
        )?;
        let digest_id = self.conn.last_insert_rowid();

        if !events.is_empty() {
            self.conn.execute(
                "UPDATE turn_events SET rolled_up_into = ? WHERE session_id = ? AND rolled_up_into IS NULL",
                params![digest_id, session_id],
            )?;
        }

        let mut observation_id: Option<i64> = None;
        if self.config.mirror_to_l8 && event_count > 0 && !project_root.is_empty() {
            let mem = MemoryEngine::new(self.conn)?;
            let title = format!(
                "Conversation digest #{digest_id} ({event_count} events, session {session_id})"
            );
            let id = mem.remember(NewObservation {
                project_root: project_root.clone(),
                session_id: None,
                agent_id: agent_id.clone(),
                kind: ObservationKind::Convention,
                title,
                content: summary.clone(),
                why: Some("Auto-generated by L11 conversation digest.".into()),
                how_to_apply: Some(
                    "Recall via `crux_recall` or `crux digest --session=…` to restore context."
                        .into(),
                ),
                symbol: None,
                file_path: None,
                tags: vec!["l11".into(), "digest".into()],
                importance: self.config.mirror_importance,
                private: false,
            })?;
            self.conn.execute(
                "UPDATE turn_digests SET observation_id = ? WHERE id = ?",
                params![id, digest_id],
            )?;
            observation_id = Some(id);
        }

        Ok(TurnDigest {
            id: digest_id,
            session_id: session_id.to_string(),
            project_root,
            agent_id,
            ts_start_epoch: ts_start,
            ts_end_epoch: ts_end,
            event_count,
            original_tokens: total_orig,
            compressed_tokens: total_compressed,
            summary,
            observation_id,
            created_at_epoch: now,
        })
    }

    pub fn latest_digest(&self, session_id: &str) -> Result<Option<TurnDigest>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {} FROM turn_digests WHERE session_id = ? \
                     ORDER BY created_at_epoch DESC, id DESC LIMIT 1",
                    DIGEST_COLS
                ),
                params![session_id],
                digest_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_digests(&self, project_root: &str, limit: usize) -> Result<Vec<TurnDigest>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM turn_digests WHERE project_root = ? \
             ORDER BY created_at_epoch DESC, id DESC LIMIT ?",
            DIGEST_COLS
        ))?;
        let rows = stmt
            .query_map(params![project_root, limit as i64], digest_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_pending_events(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredTurnEvent>> {
        let cap = if limit == 0 || limit == usize::MAX {
            -1
        } else {
            limit as i64
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM turn_events \
             WHERE session_id = ? AND rolled_up_into IS NULL \
             ORDER BY created_at_epoch ASC, id ASC \
             LIMIT ?",
            EVENT_COLS
        ))?;
        let rows = stmt
            .query_map(params![session_id, cap], event_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_all_events(&self, session_id: &str) -> Result<Vec<StoredTurnEvent>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM turn_events WHERE session_id = ? \
             ORDER BY created_at_epoch ASC, id ASC",
            EVENT_COLS
        ))?;
        let rows = stmt
            .query_map(params![session_id], event_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

const EVENT_COLS: &str = "id, session_id, project_root, agent_id, tool_name, target, \
    status, original_tokens, compressed_tokens, summary, rolled_up_into, created_at_epoch";

const DIGEST_COLS: &str = "id, session_id, project_root, agent_id, ts_start_epoch, ts_end_epoch, \
    event_count, original_tokens, compressed_tokens, summary, observation_id, created_at_epoch";

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredTurnEvent> {
    let status_s: String = row.get(6)?;
    let status: TurnStatus = status_s.parse().unwrap_or(TurnStatus::Ok);
    Ok(StoredTurnEvent {
        id: row.get(0)?,
        session_id: row.get(1)?,
        project_root: row.get(2)?,
        agent_id: row.get(3)?,
        tool_name: row.get(4)?,
        target: row.get(5)?,
        status,
        original_tokens: row.get(7)?,
        compressed_tokens: row.get(8)?,
        summary: row.get(9)?,
        rolled_up_into: row.get(10)?,
        created_at_epoch: row.get(11)?,
    })
}

fn digest_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnDigest> {
    Ok(TurnDigest {
        id: row.get(0)?,
        session_id: row.get(1)?,
        project_root: row.get(2)?,
        agent_id: row.get(3)?,
        ts_start_epoch: row.get(4)?,
        ts_end_epoch: row.get(5)?,
        event_count: row.get(6)?,
        original_tokens: row.get(7)?,
        compressed_tokens: row.get(8)?,
        summary: row.get(9)?,
        observation_id: row.get(10)?,
        created_at_epoch: row.get(11)?,
    })
}

fn stored_to_event(s: StoredTurnEvent) -> TurnEvent {
    TurnEvent {
        session_id: s.session_id,
        project_root: s.project_root,
        agent_id: s.agent_id,
        tool_name: s.tool_name,
        target: s.target,
        status: s.status,
        original_tokens: s.original_tokens,
        compressed_tokens: s.compressed_tokens,
        summary: s.summary,
    }
}

fn default_summary(tool_name: &str, target: Option<&str>) -> String {
    match target {
        Some(t) if !t.is_empty() => format!("{tool_name} {t}"),
        _ => tool_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        crux_core::db::open_in_memory().unwrap()
    }

    fn ev(session: &str, tool: &str, target: &str) -> TurnEvent {
        TurnEvent {
            session_id: session.into(),
            project_root: "/p".into(),
            agent_id: Some("default".into()),
            tool_name: tool.into(),
            target: Some(target.into()),
            status: TurnStatus::Ok,
            original_tokens: 100,
            compressed_tokens: 20,
            summary: format!("{tool} {target}"),
        }
    }

    #[test]
    fn record_inserts_pending_event() {
        let conn = fixture();
        let eng = DigestEngine::default_with_conn(&conn);
        let outcome = eng.record(&ev("s1", "Read", "src/a.rs")).unwrap();
        assert!(outcome.event_id > 0);
        assert!(outcome.auto_compacted.is_none());
        assert_eq!(eng.pending_count("s1").unwrap(), 1);
    }

    #[test]
    fn record_rejects_empty_session() {
        let conn = fixture();
        let eng = DigestEngine::default_with_conn(&conn);
        let bad = TurnEvent {
            session_id: "".into(),
            ..ev("s1", "Read", "x")
        };
        let err = eng.record(&bad).unwrap_err();
        assert!(err.to_string().contains("session_id"));
    }

    #[test]
    fn auto_compact_fires_on_threshold() {
        let conn = fixture();
        let cfg = L11Config {
            auto_compact_every_n: 3,
            mirror_to_l8: false,
            ..L11Config::default()
        };
        let eng = DigestEngine::new(&conn, cfg);

        for i in 0..3 {
            let outcome = eng
                .record(&ev("s1", "Read", &format!("src/a{i}.rs")))
                .unwrap();
            if i < 2 {
                assert!(outcome.auto_compacted.is_none(), "iter {i}");
            } else {
                let d = outcome
                    .auto_compacted
                    .expect("expected auto compact on iter 2");
                assert_eq!(d.event_count, 3);
                assert!(d.summary.contains("Files read"));
            }
        }
        assert_eq!(eng.pending_count("s1").unwrap(), 0);
    }

    #[test]
    fn manual_compact_marks_events_rolled_up() {
        let conn = fixture();
        let cfg = L11Config {
            auto_compact_every_n: 0,
            mirror_to_l8: false,
            ..L11Config::default()
        };
        let eng = DigestEngine::new(&conn, cfg);
        eng.record(&ev("s1", "Read", "a")).unwrap();
        eng.record(&ev("s1", "Edit", "a")).unwrap();
        let d = eng.compact("s1").unwrap();
        assert_eq!(d.event_count, 2);
        assert_eq!(eng.pending_count("s1").unwrap(), 0);
        let stored = eng.list_all_events("s1").unwrap();
        assert!(stored.iter().all(|s| s.rolled_up_into == Some(d.id)));
    }

    #[test]
    fn summarize_renders_pending() {
        let conn = fixture();
        let cfg = L11Config {
            auto_compact_every_n: 0,
            mirror_to_l8: false,
            ..L11Config::default()
        };
        let eng = DigestEngine::new(&conn, cfg);
        eng.record(&ev("s1", "Read", "src/a.rs")).unwrap();
        eng.record(&ev("s1", "Read", "src/a.rs")).unwrap();
        eng.record(&ev("s1", "Bash", "cargo test")).unwrap();
        let s = eng.summarize("s1").unwrap();
        assert!(s.contains("Files read"));
        assert!(s.contains("src/a.rs ×2"));
        assert!(s.contains("Commands"));
        assert!(s.contains("cargo ×1"));
    }

    #[test]
    fn mirror_creates_l8_observation() {
        let conn = fixture();
        let cfg = L11Config {
            auto_compact_every_n: 0,
            mirror_to_l8: true,
            mirror_importance: 6,
            ..L11Config::default()
        };
        let eng = DigestEngine::new(&conn, cfg);
        eng.record(&ev("s1", "Read", "src/a.rs")).unwrap();
        let d = eng.compact("s1").unwrap();
        assert!(d.observation_id.is_some());
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM observations WHERE id = ?",
                params![d.observation_id.unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn latest_summary_includes_pending_after_digest() {
        let conn = fixture();
        let cfg = L11Config {
            auto_compact_every_n: 0,
            mirror_to_l8: false,
            ..L11Config::default()
        };
        let eng = DigestEngine::new(&conn, cfg);
        eng.record(&ev("s1", "Read", "src/a.rs")).unwrap();
        let _d = eng.compact("s1").unwrap();
        eng.record(&ev("s1", "Edit", "src/a.rs")).unwrap();
        let s = eng.latest_summary("s1").unwrap();
        assert!(s.starts_with("Latest digest"));
        assert!(s.contains("--- 1 pending event(s) ---"));
        assert!(s.contains("Files edited"));
    }

    #[test]
    fn list_digests_orders_newest_first() {
        let conn = fixture();
        let cfg = L11Config {
            auto_compact_every_n: 0,
            mirror_to_l8: false,
            ..L11Config::default()
        };
        let eng = DigestEngine::new(&conn, cfg);
        eng.record(&ev("s1", "Read", "a")).unwrap();
        let d1 = eng.compact("s1").unwrap();
        eng.record(&ev("s1", "Edit", "a")).unwrap();
        let d2 = eng.compact("s1").unwrap();
        let list = eng.list_digests("/p", 5).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, d2.id);
        assert_eq!(list[1].id, d1.id);
    }
}
