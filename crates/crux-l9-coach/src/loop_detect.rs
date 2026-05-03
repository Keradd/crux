//! Loop detection — compares the most recent agent inputs against a
//! short history, flags a loop when similarity crosses the configured
//! threshold.
//!
//! We use token-level Jaccard similarity (|A ∩ B| / |A ∪ B|) instead of
//! cosine because we have no embedder yet and Jaccard survives minor
//! word reordering without false positives on common-word overlap.
//!
//! State is persisted in `loop_state` so hook calls from separate
//! processes share the same window. Each row holds at most
//! `MAX_HISTORY` entries per channel.

use std::collections::{HashSet, VecDeque};

use rusqlite::{params, Connection, OptionalExtension};

use crux_core::error::Result;

use crate::types::LoopCheckResult;

const MAX_HISTORY: usize = 5;
const DEFAULT_SIMILARITY_THRESHOLD: f64 = 0.7;
const MAX_WARNINGS_PER_SESSION: i64 = 2;

#[derive(Debug, Clone)]
pub struct LoopDetector<'c> {
    conn: &'c Connection,
    threshold: f64,
}

impl<'c> LoopDetector<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self {
            conn,
            threshold: DEFAULT_SIMILARITY_THRESHOLD,
        }
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Append a new turn and report whether we look stuck. `user_msg` or
    /// `tool_result` may be empty — the detector tracks only the ones
    /// supplied.
    pub fn check(
        &self,
        session_id: &str,
        user_msg: &str,
        tool_result: &str,
    ) -> Result<LoopCheckResult> {
        let mut state = self.load(session_id)?;

        let user_sim = push_and_score(&mut state.user_msgs, user_msg, MAX_HISTORY, self.threshold);
        let tool_sim = push_and_score(
            &mut state.tool_results,
            tool_result,
            MAX_HISTORY,
            self.threshold,
        );

        let similarity = user_sim.max(tool_sim);
        let is_loop =
            similarity >= self.threshold && state.notes_emitted < MAX_WARNINGS_PER_SESSION;

        if is_loop {
            state.notes_emitted += 1;
        }

        self.save(session_id, &state)?;

        Ok(LoopCheckResult {
            is_loop,
            similarity,
            warning: if is_loop {
                Some(format!(
                    "coach: repeated-looking {} (jaccard {:.2}). pause + reconsider.",
                    if user_sim >= tool_sim {
                        "user request"
                    } else {
                        "tool result"
                    },
                    similarity
                ))
            } else {
                None
            },
        })
    }

    pub fn reset(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM loop_state WHERE session_id = ?",
            params![session_id],
        )?;
        Ok(())
    }

    fn load(&self, session_id: &str) -> Result<LoopStateRow> {
        let row = self
            .conn
            .query_row(
                "SELECT last_user_msgs, last_tool_results, notes_emitted
                 FROM loop_state WHERE session_id = ?",
                params![session_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let (users, tools, notes) = row
            .map(|(a, b, c)| (parse_vec(&a), parse_vec(&b), c))
            .unwrap_or_default();
        Ok(LoopStateRow {
            user_msgs: users.into(),
            tool_results: tools.into(),
            notes_emitted: notes,
        })
    }

    fn save(&self, session_id: &str, state: &LoopStateRow) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let users_json =
            serde_json::to_string(&state.user_msgs.iter().collect::<Vec<_>>()).unwrap_or_default();
        let tools_json = serde_json::to_string(&state.tool_results.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        self.conn.execute(
            r#"INSERT INTO loop_state
                 (session_id, last_user_msgs, last_tool_results,
                  notes_emitted, updated_at_epoch)
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(session_id) DO UPDATE SET
                 last_user_msgs    = excluded.last_user_msgs,
                 last_tool_results = excluded.last_tool_results,
                 notes_emitted     = excluded.notes_emitted,
                 updated_at_epoch  = excluded.updated_at_epoch"#,
            params![session_id, users_json, tools_json, state.notes_emitted, now],
        )?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────
// internals
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct LoopStateRow {
    user_msgs: VecDeque<String>,
    tool_results: VecDeque<String>,
    notes_emitted: i64,
}

fn parse_vec(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Push a new turn onto `buf`, keep `buf.len() <= cap`, return the max
/// Jaccard similarity vs any previous turn. Empty pushes are ignored so
/// a call that only supplies one of user/tool doesn't clobber the other.
fn push_and_score(buf: &mut VecDeque<String>, turn: &str, cap: usize, _threshold: f64) -> f64 {
    if turn.trim().is_empty() {
        return 0.0;
    }

    let mut best = 0.0_f64;
    let new_tokens = tokenize(turn);
    for prev in buf.iter() {
        let sim = jaccard(&new_tokens, &tokenize(prev));
        if sim > best {
            best = sim;
        }
    }

    buf.push_back(turn.to_string());
    while buf.len() > cap {
        buf.pop_front();
    }
    best
}

pub(crate) fn tokenize(s: &str) -> HashSet<String> {
    s.to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty() && t.len() >= 2)
        .map(|t| t.to_string())
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        crux_core::db::open_in_memory().unwrap()
    }

    #[test]
    fn identical_repeats_flag_loop() {
        let conn = fixture();
        let det = LoopDetector::new(&conn);
        let _ = det.check("s1", "please fix the bug in auth", "").unwrap();
        let r = det.check("s1", "please fix the bug in auth", "").unwrap();
        assert!(r.is_loop, "second identical msg should trigger loop");
        assert!(r.similarity > 0.9);
    }

    #[test]
    fn distinct_messages_not_loop() {
        let conn = fixture();
        let det = LoopDetector::new(&conn);
        let _ = det.check("s1", "implement auth", "").unwrap();
        let r = det.check("s1", "refactor database schema", "").unwrap();
        assert!(!r.is_loop);
        assert!(r.similarity < 0.4);
    }

    #[test]
    fn warnings_cap_at_two() {
        let conn = fixture();
        let det = LoopDetector::new(&conn);
        for _ in 0..5 {
            det.check("s1", "repeat the same request", "").unwrap();
        }
        let r = det.check("s1", "repeat the same request", "").unwrap();
        // After 2 warnings the flag should stop firing.
        assert!(!r.is_loop);
    }

    #[test]
    fn reset_clears_state() {
        let conn = fixture();
        let det = LoopDetector::new(&conn);
        det.check("s1", "hello", "").unwrap();
        det.reset("s1").unwrap();
        let r = det.check("s1", "hello", "").unwrap();
        assert!(!r.is_loop);
    }

    #[test]
    fn jaccard_basic() {
        let a = tokenize("foo bar baz");
        let b = tokenize("foo bar qux");
        let s = jaccard(&a, &b);
        assert!((s - 2.0_f64 / 4.0_f64).abs() < 1e-9, "got {s}");
    }
}
