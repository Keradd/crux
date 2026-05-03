//! `MemoryEngine` — orchestrates remember/recall/decay over the SQLite
//! tables created by migration 003.

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crux_core::error::{CruxError, Result};

use crate::decay::{decayed_score, DecayTable};
use crate::types::{
    DecayStats, NewObservation, Observation, ObservationKind, RankedObservation, RecallQuery,
};

pub struct MemoryEngine<'c> {
    conn: &'c Connection,
    decay: DecayTable,
}

impl<'c> MemoryEngine<'c> {
    pub fn new(conn: &'c Connection) -> Result<Self> {
        let decay = DecayTable::load(conn)?;
        Ok(Self { conn, decay })
    }

    // ─────────────────────────────────────────────────────────────────
    // remember
    // ─────────────────────────────────────────────────────────────────

    /// Persist a new observation. Idempotent: same content_hash + project
    /// → existing row id, and we bump the access count instead.
    pub fn remember(&self, obs: NewObservation) -> Result<i64> {
        if obs.title.trim().is_empty() {
            return Err(CruxError::other("observation title cannot be empty"));
        }
        if obs.content.trim().is_empty() {
            return Err(CruxError::other("observation content cannot be empty"));
        }
        if obs.importance < 1 || obs.importance > 10 {
            return Err(CruxError::other("observation importance must be 1..=10"));
        }

        let hash = content_hash(&obs.kind, &obs.title, &obs.content);
        // Dedup
        if let Some(id) = self.find_by_hash(&obs.project_root, &hash)? {
            self.bump_access(id, /*boost=*/ true)?;
            return Ok(id);
        }

        let now = chrono::Utc::now().timestamp();
        let tags_json = serde_json::to_string(&obs.tags)?;
        self.conn.execute(
            r#"INSERT INTO observations
               (session_id, project_root, agent_id, kind, title, content, why,
                how_to_apply, symbol, file_path, tags, importance,
                relevance_score, access_count, content_hash,
                archived, private, last_accessed_epoch,
                created_at_epoch, updated_at_epoch)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1.0, 0, ?, 0, ?, ?, ?, ?)"#,
            params![
                obs.session_id,
                obs.project_root,
                obs.agent_id,
                obs.kind.as_str(),
                obs.title,
                obs.content,
                obs.why,
                obs.how_to_apply,
                obs.symbol,
                obs.file_path,
                tags_json,
                obs.importance,
                hash,
                if obs.private { 1 } else { 0 },
                now,
                now,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn archive(&self, id: i64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE observations SET archived = 1, updated_at_epoch = ? WHERE id = ?",
            params![chrono::Utc::now().timestamp(), id],
        )?;
        Ok(n > 0)
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM observations WHERE id = ?", params![id])?;
        Ok(n > 0)
    }

    // ─────────────────────────────────────────────────────────────────
    // recall
    // ─────────────────────────────────────────────────────────────────

    /// Decay-aware recall. Empty query returns the most-recent / highest
    /// importance items. Non-empty query runs FTS5 against title + content
    /// + why + how_to_apply + tags, then re-ranks by:
    ///
    ///   final = importance * decayed_relevance + 0.1 * fts_bonus
    ///
    /// where `fts_bonus` is `-bm25(...)` clamped to `[0, 1]`. Higher = better.
    pub fn recall(&self, q: &RecallQuery) -> Result<Vec<RankedObservation>> {
        let now = chrono::Utc::now().timestamp();
        let mut where_clauses: Vec<String> = vec!["o.archived = 0".into()];
        let mut params_vec: Vec<rusqlite::types::Value> = Vec::new();

        if let Some(pr) = &q.project_root {
            where_clauses.push("o.project_root = ?".into());
            params_vec.push(pr.clone().into());
        }
        if let Some(sym) = &q.symbol {
            where_clauses.push("o.symbol = ?".into());
            params_vec.push(sym.clone().into());
        }
        if !q.kinds.is_empty() {
            let placeholders: Vec<&str> = q.kinds.iter().map(|_| "?").collect();
            where_clauses.push(format!("o.kind IN ({})", placeholders.join(",")));
            for k in &q.kinds {
                params_vec.push(k.as_str().to_string().into());
            }
        }
        if !q.file_paths.is_empty() {
            let placeholders: Vec<&str> = q.file_paths.iter().map(|_| "?").collect();
            where_clauses.push(format!("o.file_path IN ({})", placeholders.join(",")));
            for p in &q.file_paths {
                params_vec.push(p.clone().into());
            }
        }
        if q.include_archived {
            // Replace the first clause we added.
            where_clauses[0] = "1=1".into();
        }

        let where_sql = where_clauses.join(" AND ");
        let limit = q.limit.max(1);

        let rows: Vec<(Observation, f64)> = if q.query.trim().is_empty() {
            self.fetch_no_query(&where_sql, &params_vec, limit)?
        } else {
            self.fetch_fts(&q.query, &where_sql, &params_vec, limit)?
        };

        let mut ranked: Vec<RankedObservation> = rows
            .into_iter()
            .map(|(obs, fts_score)| {
                let p = self.decay.params(obs.kind);
                let last = obs.last_accessed_epoch.unwrap_or(obs.created_at_epoch);
                let decayed = decayed_score(p, obs.relevance_score, last, now);
                let importance_weight = (obs.importance as f64) / 5.0;
                let score = importance_weight * decayed + 0.1 * fts_score;
                RankedObservation {
                    observation: obs,
                    rank: 0,
                    score,
                    fts_score,
                }
            })
            .collect();

        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (i, r) in ranked.iter_mut().enumerate() {
            r.rank = i;
        }

        // Bump access on the items we surface so decay rewards repeat use.
        let now_epoch = chrono::Utc::now().timestamp();
        for r in &ranked {
            self.touch(r.observation.id, now_epoch)?;
        }

        Ok(ranked)
    }

    pub fn get(&self, id: i64) -> Result<Option<Observation>> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {} FROM observations WHERE id = ?", obs_columns()),
                params![id],
                obs_from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list(&self, project_root: &str, limit: usize) -> Result<Vec<Observation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM observations o
             WHERE archived = 0 AND project_root = ?
             ORDER BY importance DESC, relevance_score DESC, created_at_epoch DESC
             LIMIT ?",
            obs_columns()
        ))?;
        let rows = stmt
            .query_map(params![project_root, limit as i64], obs_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ─────────────────────────────────────────────────────────────────
    // passive auto-surface helpers
    //
    // These exist for the MCP `crux_read` / `crux_get_symbol_source`
    // dispatchers: every time the agent reads a file or fetches symbol
    // source, CRUX injects a short footer listing past observations
    // attached to that file / symbol. Same decay-aware ranking as
    // `recall`, but without an FTS query.
    // ─────────────────────────────────────────────────────────────────

    /// Return top-N active observations whose `file_path` matches one of
    /// `path_variants`. Pass both absolute and project-relative forms so
    /// we catch observations stored in either shape. Empty query, decay
    /// re-ranked. Touches (bumps access) the surfaced rows.
    pub fn recall_by_file(
        &self,
        project_root: &str,
        path_variants: &[&str],
        limit: usize,
    ) -> Result<Vec<RankedObservation>> {
        if path_variants.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let q = RecallQuery {
            query: String::new(),
            project_root: Some(project_root.to_string()),
            file_paths: path_variants.iter().map(|s| s.to_string()).collect(),
            limit,
            ..Default::default()
        };
        self.recall(&q)
    }

    /// Return top-N active observations whose `symbol` column exactly
    /// matches `qualified_name`. Empty query, decay re-ranked.
    pub fn recall_by_symbol(
        &self,
        project_root: &str,
        qualified_name: &str,
        limit: usize,
    ) -> Result<Vec<RankedObservation>> {
        if qualified_name.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let q = RecallQuery {
            query: String::new(),
            project_root: Some(project_root.to_string()),
            symbol: Some(qualified_name.to_string()),
            limit,
            ..Default::default()
        };
        self.recall(&q)
    }

    /// Run periodic decay maintenance: drop relevance below floor for each
    /// observation, archive ones whose decayed score has hit the floor.
    pub fn decay_pass(&self, now_epoch: i64) -> Result<DecayStats> {
        let mut stats = DecayStats::default();
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, relevance_score, last_accessed_epoch, created_at_epoch, archived
             FROM observations",
        )?;
        let rows: Vec<(i64, String, f64, Option<i64>, i64, i64)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        for (id, kind_s, rel, last_epoch, created, archived) in rows {
            stats.scanned += 1;
            if archived != 0 {
                continue;
            }
            let kind = match kind_s.parse::<ObservationKind>() {
                Ok(k) => k,
                Err(_) => continue,
            };
            let p = self.decay.params(kind);
            if p.decay_rate >= 1.0 {
                continue;
            }
            let last = last_epoch.unwrap_or(created);
            let new_score = decayed_score(p, rel, last, now_epoch);
            if (new_score - rel).abs() > 1e-6 {
                self.conn.execute(
                    "UPDATE observations SET relevance_score = ?, updated_at_epoch = ? WHERE id = ?",
                    params![new_score, now_epoch, id],
                )?;
                stats.updated += 1;
            }
            if (new_score - p.min_score).abs() < 1e-6 && p.min_score < 0.2 {
                // Hit the floor at a "low importance" floor — archive.
                self.conn.execute(
                    "UPDATE observations SET archived = 1, updated_at_epoch = ? WHERE id = ?",
                    params![now_epoch, id],
                )?;
                stats.archived += 1;
            }
        }
        Ok(stats)
    }

    // ─────────────────────────────────────────────────────────────────
    // internal
    // ─────────────────────────────────────────────────────────────────

    fn find_by_hash(&self, project: &str, hash: &str) -> Result<Option<i64>> {
        let id = self
            .conn
            .query_row(
                "SELECT id FROM observations WHERE project_root = ? AND content_hash = ? LIMIT 1",
                params![project, hash],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        Ok(id)
    }

    fn bump_access(&self, id: i64, boost: bool) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        if boost {
            // Determine the kind so we know how much to boost.
            let kind_s: String = self.conn.query_row(
                "SELECT kind FROM observations WHERE id = ?",
                params![id],
                |r| r.get(0),
            )?;
            let kind: ObservationKind = kind_s.parse().map_err(CruxError::other)?;
            let bump = self.decay.params(kind).boost_on_access;
            self.conn.execute(
                "UPDATE observations
                 SET access_count = access_count + 1,
                     relevance_score = MIN(1.0, relevance_score + ?),
                     last_accessed_epoch = ?,
                     updated_at_epoch = ?
                 WHERE id = ?",
                params![bump, now, now, id],
            )?;
        } else {
            self.touch(id, now)?;
        }
        Ok(())
    }

    fn touch(&self, id: i64, now_epoch: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE observations
             SET access_count = access_count + 1,
                 last_accessed_epoch = ?,
                 updated_at_epoch = ?
             WHERE id = ?",
            params![now_epoch, now_epoch, id],
        )?;
        Ok(())
    }

    fn fetch_no_query(
        &self,
        where_sql: &str,
        params_vec: &[rusqlite::types::Value],
        limit: usize,
    ) -> Result<Vec<(Observation, f64)>> {
        let sql = format!(
            "SELECT {} FROM observations o
             WHERE {}
             ORDER BY importance DESC, relevance_score DESC, created_at_epoch DESC
             LIMIT ?",
            obs_columns(),
            where_sql,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut p: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|v| v as &dyn rusqlite::ToSql)
            .collect();
        let lim = limit as i64;
        p.push(&lim);
        let rows = stmt
            .query_map(rusqlite::params_from_iter(p.iter()), obs_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().map(|o| (o, 0.0)).collect())
    }

    fn fetch_fts(
        &self,
        query: &str,
        where_sql: &str,
        params_vec: &[rusqlite::types::Value],
        limit: usize,
    ) -> Result<Vec<(Observation, f64)>> {
        // FTS5's `MATCH` is strict about syntax; quote the user query to
        // avoid surfacing parse errors on punctuation.
        let safe = sanitize_fts_query(query);
        let sql = format!(
            "SELECT {cols}, bm25(observations_fts) AS bm
             FROM observations_fts
             JOIN observations o ON o.id = observations_fts.rowid
             WHERE observations_fts MATCH ? AND {where_sql}
             ORDER BY bm
             LIMIT ?",
            cols = obs_columns(),
            where_sql = where_sql,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut p: Vec<&dyn rusqlite::ToSql> = vec![&safe as &dyn rusqlite::ToSql];
        for v in params_vec {
            p.push(v as &dyn rusqlite::ToSql);
        }
        let lim = limit as i64;
        p.push(&lim);

        let rows = stmt
            .query_map(rusqlite::params_from_iter(p.iter()), |row| {
                let obs = obs_from_row(row)?;
                let bm: f64 = row.get(OBS_COLUMN_COUNT)?;
                // BM25 is negative-better in SQLite FTS5; map to a 0..1
                // bonus by clamping.
                let bonus = (-bm).clamp(0.0, 5.0) / 5.0;
                Ok((obs, bonus))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Row mapping helpers
// ─────────────────────────────────────────────────────────────────────────

const OBS_COLUMN_COUNT: usize = 19;

fn obs_columns() -> &'static str {
    // Order MUST match `obs_from_row` indices.
    "o.id, o.project_root, o.session_id, o.agent_id, o.kind, o.title, o.content, \
     o.why, o.how_to_apply, o.symbol, o.file_path, o.tags, o.importance, \
     o.relevance_score, o.access_count, o.content_hash, o.archived, \
     o.private, o.last_accessed_epoch, o.created_at_epoch, o.updated_at_epoch"
}

fn obs_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Observation> {
    let kind_s: String = row.get(4)?;
    let kind = kind_s.parse().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    let tags_json: Option<String> = row.get(11)?;
    let tags: Vec<String> = tags_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    Ok(Observation {
        id: row.get(0)?,
        project_root: row.get(1)?,
        session_id: row.get(2)?,
        agent_id: row.get(3)?,
        kind,
        title: row.get(5)?,
        content: row.get(6)?,
        why: row.get(7)?,
        how_to_apply: row.get(8)?,
        symbol: row.get(9)?,
        file_path: row.get(10)?,
        tags,
        importance: row.get::<_, i64>(12)? as u8,
        relevance_score: row.get(13)?,
        access_count: row.get(14)?,
        content_hash: row.get(15)?,
        archived: row.get::<_, i64>(16)? != 0,
        private: row.get::<_, i64>(17)? != 0,
        last_accessed_epoch: row.get(18)?,
        created_at_epoch: row.get(19)?,
        updated_at_epoch: row.get(20)?,
    })
}

fn content_hash(kind: &ObservationKind, title: &str, content: &str) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_str().as_bytes());
    h.update(b"\x00");
    h.update(title.as_bytes());
    h.update(b"\x00");
    h.update(content.as_bytes());
    let bytes = h.finalize();
    hex::encode(&bytes[..16])
}

/// Wrap each space-separated word in quotes so FTS5 treats them as
/// phrase tokens. Drops anything that would make MATCH unhappy.
fn sanitize_fts_query(q: &str) -> String {
    q.split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
        .map(|w| {
            let cleaned: String = w
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if cleaned.is_empty() {
                String::new()
            } else {
                format!("\"{}\"", cleaned)
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with(conn: &Connection) -> MemoryEngine<'_> {
        MemoryEngine::new(conn).unwrap()
    }

    fn fixture() -> Connection {
        crux_core::db::open_in_memory().unwrap()
    }

    #[test]
    fn remember_returns_id() {
        let conn = fixture();
        let mem = engine_with(&conn);
        let id = mem
            .remember(NewObservation::minimal(
                "/p",
                ObservationKind::Decision,
                "Use Vue 3 Composition",
                "Composition API gives us better reuse than Options API.",
            ))
            .unwrap();
        assert!(id > 0);
    }

    #[test]
    fn remember_dedupes_same_content() {
        let conn = fixture();
        let mem = engine_with(&conn);
        let mk = || {
            NewObservation::minimal(
                "/p",
                ObservationKind::Convention,
                "snake_case",
                "Use snake_case for module names.",
            )
        };
        let a = mem.remember(mk()).unwrap();
        let b = mem.remember(mk()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn recall_finds_by_keyword() {
        let conn = fixture();
        let mem = engine_with(&conn);
        mem.remember(NewObservation::minimal(
            "/p",
            ObservationKind::Reference,
            "Vue 3 docs",
            "https://vuejs.org/guide/introduction.html",
        ))
        .unwrap();
        mem.remember(NewObservation::minimal(
            "/p",
            ObservationKind::Decision,
            "Use Pinia",
            "We use Pinia, not Vuex.",
        ))
        .unwrap();

        let q = RecallQuery {
            query: "Pinia".into(),
            project_root: Some("/p".into()),
            limit: 5,
            ..Default::default()
        };
        let results = mem.recall(&q).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].observation.title.contains("Pinia"));
    }

    #[test]
    fn list_orders_by_importance() {
        let conn = fixture();
        let mem = engine_with(&conn);
        let mut hi =
            NewObservation::minimal("/p", ObservationKind::Guardrail, "always_x", "always do x");
        hi.importance = 9;
        let mut lo =
            NewObservation::minimal("/p", ObservationKind::Reference, "see_y", "see https://y");
        lo.importance = 2;
        mem.remember(lo).unwrap();
        mem.remember(hi).unwrap();
        let items = mem.list("/p", 10).unwrap();
        assert_eq!(items[0].title, "always_x");
    }

    #[test]
    fn recall_empty_query_returns_recent() {
        let conn = fixture();
        let mem = engine_with(&conn);
        mem.remember(NewObservation::minimal(
            "/p",
            ObservationKind::Project,
            "react",
            "use react",
        ))
        .unwrap();
        let q = RecallQuery {
            query: "".into(),
            project_root: Some("/p".into()),
            limit: 5,
            ..Default::default()
        };
        let r = mem.recall(&q).unwrap();
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn archive_then_recall_skips() {
        let conn = fixture();
        let mem = engine_with(&conn);
        let id = mem
            .remember(NewObservation::minimal(
                "/p",
                ObservationKind::Project,
                "old",
                "old fact",
            ))
            .unwrap();
        mem.archive(id).unwrap();
        let q = RecallQuery {
            query: "old".into(),
            project_root: Some("/p".into()),
            limit: 5,
            ..Default::default()
        };
        let r = mem.recall(&q).unwrap();
        assert!(r.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────
    // passive auto-surface: recall_by_file / recall_by_symbol
    // ─────────────────────────────────────────────────────────────────

    fn obs_with_file(
        kind: ObservationKind,
        title: &str,
        content: &str,
        file_path: &str,
    ) -> NewObservation {
        let mut o = NewObservation::minimal("/p", kind, title, content);
        o.file_path = Some(file_path.into());
        o
    }

    fn obs_with_symbol(
        kind: ObservationKind,
        title: &str,
        content: &str,
        symbol: &str,
    ) -> NewObservation {
        let mut o = NewObservation::minimal("/p", kind, title, content);
        o.symbol = Some(symbol.into());
        o
    }

    #[test]
    fn recall_by_file_matches_exact_path() {
        let conn = fixture();
        let mem = engine_with(&conn);
        mem.remember(obs_with_file(
            ObservationKind::Decision,
            "zstd level",
            "zstd=3 balances speed/ratio",
            "src/cache.rs",
        ))
        .unwrap();
        mem.remember(obs_with_file(
            ObservationKind::Convention,
            "other file obs",
            "unrelated",
            "src/other.rs",
        ))
        .unwrap();

        let hits = mem.recall_by_file("/p", &["src/cache.rs"], 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation.title, "zstd level");
    }

    #[test]
    fn recall_by_file_matches_any_of_variants() {
        let conn = fixture();
        let mem = engine_with(&conn);
        mem.remember(obs_with_file(
            ObservationKind::ErrorPattern,
            "stored with absolute path",
            "detail",
            "/home/x/proj/src/lib.rs",
        ))
        .unwrap();
        mem.remember(obs_with_file(
            ObservationKind::Decision,
            "stored with relative path",
            "detail",
            "src/lib.rs",
        ))
        .unwrap();

        // Caller passes both variants; both obs should surface.
        let hits = mem
            .recall_by_file("/p", &["/home/x/proj/src/lib.rs", "src/lib.rs"], 10)
            .unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn recall_by_file_respects_limit_and_importance() {
        let conn = fixture();
        let mem = engine_with(&conn);
        let mut hi = obs_with_file(
            ObservationKind::Guardrail,
            "hi-importance",
            "never compress this file",
            "src/auth.rs",
        );
        hi.importance = 9;
        let mut lo = obs_with_file(
            ObservationKind::Reference,
            "lo-importance",
            "see docs",
            "src/auth.rs",
        );
        lo.importance = 2;
        mem.remember(lo).unwrap();
        mem.remember(hi).unwrap();

        let hits = mem.recall_by_file("/p", &["src/auth.rs"], 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation.title, "hi-importance");
    }

    #[test]
    fn recall_by_file_empty_inputs_return_empty() {
        let conn = fixture();
        let mem = engine_with(&conn);
        assert!(mem.recall_by_file("/p", &[], 3).unwrap().is_empty());
        assert!(mem
            .recall_by_file("/p", &["src/x.rs"], 0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn recall_by_file_skips_archived() {
        let conn = fixture();
        let mem = engine_with(&conn);
        let id = mem
            .remember(obs_with_file(
                ObservationKind::Project,
                "outdated",
                "stale note",
                "src/old.rs",
            ))
            .unwrap();
        mem.archive(id).unwrap();
        assert!(mem
            .recall_by_file("/p", &["src/old.rs"], 3)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn recall_by_file_scopes_to_project() {
        let conn = fixture();
        let mem = engine_with(&conn);
        // Project A
        mem.remember(obs_with_file(
            ObservationKind::Decision,
            "proj-a obs",
            "a",
            "src/x.rs",
        ))
        .unwrap();
        // Project B — same file path, different project_root
        let mut b = obs_with_file(ObservationKind::Decision, "proj-b obs", "b", "src/x.rs");
        b.project_root = "/q".into();
        mem.remember(b).unwrap();

        let hits = mem.recall_by_file("/p", &["src/x.rs"], 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation.title, "proj-a obs");
    }

    #[test]
    fn recall_by_symbol_matches_qualified_name() {
        let conn = fixture();
        let mem = engine_with(&conn);
        mem.remember(obs_with_symbol(
            ObservationKind::Decision,
            "chose rayon",
            "parallel iter for perf",
            "demo::worker::run",
        ))
        .unwrap();
        mem.remember(obs_with_symbol(
            ObservationKind::Convention,
            "other symbol",
            "unrelated",
            "demo::other::fn",
        ))
        .unwrap();

        let hits = mem.recall_by_symbol("/p", "demo::worker::run", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation.title, "chose rayon");
    }

    #[test]
    fn recall_by_symbol_empty_inputs_return_empty() {
        let conn = fixture();
        let mem = engine_with(&conn);
        assert!(mem.recall_by_symbol("/p", "", 3).unwrap().is_empty());
        assert!(mem.recall_by_symbol("/p", "x::y", 0).unwrap().is_empty());
    }
}
