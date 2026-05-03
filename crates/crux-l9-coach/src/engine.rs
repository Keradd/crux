//! `CoachEngine` — compute a CRUX-wide health score from the live
//! configuration + telemetry + memory state. Persists snapshots to
//! `quality_scores` so later runs can show deltas.
//!
//! The scoring matrix is adapted from alex/token-optimizer's Coach mode
//! (see `docs/CRUX-DESIGN.md` §7.9). Thresholds are expressed as percent
//! of the dominant context window so the score is model-agnostic.

use std::path::Path;

use rusqlite::{params, Connection};

use crux_core::{config::Config, error::Result, paths, telemetry};

use crate::drift::DriftTracker;
use crate::types::{score_to_grade, CoachData, Pattern, Severity, Snapshot};

/// Assumed context window for scoring when we don't have model-specific
/// telemetry yet. 200k matches Sonnet 3.5 / current Claude Code default.
const DEFAULT_CONTEXT_WINDOW: u32 = 200_000;
/// CLAUDE.md budget thresholds (as fraction of context window).
const CLAUDE_MD_LEAN_PCT: f64 = 2.0;
const CLAUDE_MD_FAT_PCT: f64 = 3.0;

pub struct CoachEngine<'c> {
    conn: &'c Connection,
    config: &'c Config,
    project_root: Option<&'c Path>,
}

impl<'c> CoachEngine<'c> {
    pub fn new(conn: &'c Connection, config: &'c Config, project_root: Option<&'c Path>) -> Self {
        Self {
            conn,
            config,
            project_root,
        }
    }

    /// Compute the current health snapshot. Does NOT persist — call
    /// [`Self::persist`] separately if you want history.
    pub fn snapshot(&self) -> Result<CoachData> {
        let mut score: i32 = 75;
        let mut patterns_good: Vec<Pattern> = Vec::new();
        let mut patterns_bad: Vec<Pattern> = Vec::new();

        let ctx_window = DEFAULT_CONTEXT_WINDOW;
        let (claude_md_tokens, claude_md_pct) = self.claude_md_metrics(ctx_window);

        // ── CLAUDE.md size
        if claude_md_tokens > 0 && claude_md_pct < CLAUDE_MD_LEAN_PCT {
            score += 5;
            patterns_good.push(Pattern::good(
                "Lean CLAUDE.md",
                format!(
                    "{} tokens ({:.1}% of {}k ctx), under the {:.0}% threshold.",
                    claude_md_tokens,
                    claude_md_pct,
                    ctx_window / 1000,
                    CLAUDE_MD_LEAN_PCT
                ),
            ));
        } else if claude_md_pct > CLAUDE_MD_FAT_PCT {
            score -= 5;
            patterns_bad.push(
                Pattern::bad(
                    "Oversized CLAUDE.md",
                    format!(
                        "{} tokens ({:.1}% of ctx) — exceeds the {:.0}% ceiling.",
                        claude_md_tokens, claude_md_pct, CLAUDE_MD_FAT_PCT
                    ),
                    Severity::Medium,
                )
                .with_fix("Move verbose rules into `.crux/` docs + .claudeignore them.")
                .with_savings(format!(
                    "~{} tokens/message if trimmed to 2%",
                    ((claude_md_tokens as f64) * 0.4) as i64
                )),
            );
        } else if claude_md_tokens == 0 {
            score -= 3;
            patterns_bad.push(
                Pattern::bad(
                    "Missing CLAUDE.md",
                    "no CLAUDE.md found — agent has no persistent guidance.",
                    Severity::Low,
                )
                .with_fix("Run `crux init` to scaffold a baseline profile."),
            );
        }

        // ── telemetry signals
        let project_pr = self.project_root.map(|p| p.display().to_string());
        let stats = telemetry::stats_by_layer(self.conn, project_pr.as_deref())?;
        let total_events: i64 = stats.iter().map(|s| s.events).sum();
        let total_savings: i64 = stats.iter().map(|s| s.savings).sum();
        let total_original: i64 = stats.iter().map(|s| s.original_tokens).sum();
        let savings_pct = if total_original > 0 {
            (total_savings as f64 / total_original as f64) * 100.0
        } else {
            0.0
        };

        if total_events == 0 {
            score -= 2;
            patterns_bad.push(
                Pattern::bad(
                    "No telemetry recorded yet",
                    "run some `crux bash` / `crux hook` calls so the coach has data.",
                    Severity::Low,
                )
                .with_fix("Wire `crux hook pre-tool` into your agent's PreToolUse settings."),
            );
        } else {
            score += 5;
            patterns_good.push(Pattern::good(
                "Usage tracking active",
                format!(
                    "{} events recorded, {:.1}% aggregate savings.",
                    total_events, savings_pct
                ),
            ));
        }

        // ── L4 cache hit telemetry detail
        let l4_hits = stats
            .iter()
            .find(|s| s.layer == "l4")
            .map(|s| s.events)
            .unwrap_or(0);
        if l4_hits >= 5 && savings_pct < 5.0 {
            score -= 3;
            patterns_bad.push(
                Pattern::bad(
                    "Cache saving little",
                    format!(
                        "L4 has {} events but only {:.1}% tokens saved.",
                        l4_hits, savings_pct
                    ),
                    Severity::Low,
                )
                .with_fix("Check .contextignore and enable `block` mode in `.crux/config.toml`."),
            );
        }

        // ── layer coverage — count what's ON
        let active = active_layer_count(&self.config.layers);
        let unused = 11 - active;
        if active <= 3 {
            score -= 5;
            patterns_bad.push(
                Pattern::bad(
                    "Few layers active",
                    format!("only {active}/11 layers enabled."),
                    Severity::Medium,
                )
                .with_fix("Enable at minimum L3/L4/L8 — see `.crux/config.toml`."),
            );
        }

        // ── L7 sandbox note (default-on since 2026-05-03)
        if !self.config.layers.l7_sandbox {
            patterns_good.push(Pattern::good(
                "Sandbox disabled",
                "L7 sandbox is explicitly disabled. Re-enable with `[layers] l7_sandbox = true` if you want `crux_execute` to replace multi-file read patterns (95-98% token reduction on that workflow per CRUX-DESIGN §13.1).",
            ));
        }

        // ── memory telemetry detail
        let mem_count = self.observation_count()?;

        // clamp
        score = score.clamp(0, 100);
        let grade = score_to_grade(score);

        Ok(CoachData {
            health_score: score,
            grade,
            patterns_good,
            patterns_bad,
            snapshot: Snapshot {
                context_window: ctx_window,
                claude_md_tokens,
                claude_md_pct,
                total_savings_tokens: total_savings,
                total_original_tokens: total_original,
                savings_pct,
                telemetry_events: total_events,
                l4_cache_hits: l4_hits,
                memory_observations: mem_count,
                active_layers: active,
                unused_layers: unused,
            },
        })
    }

    /// Run the scoring pass and write a row to `quality_scores`. Also
    /// refreshes `claude_md_history`. Returns the snapshot so callers
    /// don't need to call `snapshot()` twice.
    pub fn persist(&self, session_id: Option<&str>) -> Result<CoachData> {
        let data = self.snapshot()?;
        let now = chrono::Utc::now().timestamp();
        let project = self
            .project_root
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        let good = serde_json::to_string(&data.patterns_good).unwrap_or_default();
        let bad = serde_json::to_string(&data.patterns_bad).unwrap_or_default();
        let snap = serde_json::to_string(&data.snapshot).unwrap_or_default();

        self.conn.execute(
            r#"INSERT INTO quality_scores
                 (project_root, session_id, score, grade, patterns_good,
                  patterns_bad, snapshot, created_at_epoch)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
            params![
                project,
                session_id,
                data.health_score,
                data.grade.to_string(),
                good,
                bad,
                snap,
                now,
            ],
        )?;

        if let Some(root) = self.project_root {
            let tracker = DriftTracker::new(self.conn);
            let _ = tracker.check(root)?;
        }

        Ok(data)
    }

    fn claude_md_metrics(&self, ctx_window: u32) -> (u32, f64) {
        let Some(root) = self.project_root else {
            return (0, 0.0);
        };
        let path = root.join("CLAUDE.md");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return (0, 0.0);
        };
        let tokens = crux_core::tokens::estimate(&content) as u32;
        let pct = if ctx_window > 0 {
            (tokens as f64 / ctx_window as f64) * 100.0
        } else {
            0.0
        };
        (tokens, pct)
    }

    fn observation_count(&self) -> Result<i64> {
        // observations table comes from migration 003; fall back to 0 if
        // somehow missing so coach still renders on older DBs.
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM observations WHERE archived = 0",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(n)
    }
}

fn active_layer_count(t: &crux_core::config::LayerToggles) -> u32 {
    [
        t.l1_output,
        t.l2_mcp_shrink,
        t.l3_bash_filter,
        t.l4_read_cache,
        t.l5_ast_graph,
        t.l6_hybrid_search,
        t.l7_sandbox,
        t.l8_memory,
        t.l9_coach,
        t.l10_setup,
        t.l11_digest,
    ]
    .into_iter()
    .filter(|b| *b)
    .count() as u32
}

// Expose the crux_home helper so CLI doesn't need a direct dep.
pub fn crux_home_opt() -> Option<std::path::PathBuf> {
    paths::crux_home().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crux_core::config::Config;

    fn fixture_runtime(project: &Path) -> (Connection, Config) {
        let conn = crux_core::db::open_in_memory().unwrap();
        let cfg = Config::default();
        // Make sure the dir looks like a CRUX project for the tests.
        std::fs::create_dir_all(project.join(".crux")).unwrap();
        (conn, cfg)
    }

    #[test]
    fn snapshot_penalizes_missing_claude_md() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, cfg) = fixture_runtime(dir.path());
        let coach = CoachEngine::new(&conn, &cfg, Some(dir.path()));
        let data = coach.snapshot().unwrap();
        assert!(
            data.patterns_bad
                .iter()
                .any(|p| p.name == "Missing CLAUDE.md"),
            "expected missing-claude-md penalty"
        );
    }

    #[test]
    fn snapshot_rewards_lean_claude_md() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, cfg) = fixture_runtime(dir.path());
        std::fs::write(dir.path().join("CLAUDE.md"), "short rules").unwrap();
        let coach = CoachEngine::new(&conn, &cfg, Some(dir.path()));
        let data = coach.snapshot().unwrap();
        assert!(
            data.patterns_good
                .iter()
                .any(|p| p.name == "Lean CLAUDE.md"),
            "expected lean-claude-md bonus"
        );
    }

    #[test]
    fn score_is_grade_c_or_better_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, cfg) = fixture_runtime(dir.path());
        std::fs::write(dir.path().join("CLAUDE.md"), "short rules").unwrap();
        let coach = CoachEngine::new(&conn, &cfg, Some(dir.path()));
        let data = coach.snapshot().unwrap();
        assert!(data.health_score >= 70, "got {}", data.health_score);
    }

    #[test]
    fn persist_writes_history_row() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, cfg) = fixture_runtime(dir.path());
        std::fs::write(dir.path().join("CLAUDE.md"), "rules").unwrap();
        let coach = CoachEngine::new(&conn, &cfg, Some(dir.path()));
        coach.persist(Some("s1")).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM quality_scores", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let h: i64 = conn
            .query_row("SELECT COUNT(*) FROM claude_md_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(h, 1);
    }
}
