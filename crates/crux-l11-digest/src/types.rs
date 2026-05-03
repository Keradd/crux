//! Public types for the L11 conversation digest engine.
//!
//! These map 1:1 to the columns in migration 010 (`turn_events` /
//! `turn_digests`). `serde` is enabled so the CLI/MCP can ship them as
//! JSON without adapter types.

use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Outcome of a single tool call. Mirrors the CHECK constraint on
/// `turn_events.status` in migration 010.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TurnStatus {
    #[default]
    Ok,
    Err,
    Timeout,
    Blocked,
    Skipped,
}

impl TurnStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TurnStatus::Ok => "ok",
            TurnStatus::Err => "err",
            TurnStatus::Timeout => "timeout",
            TurnStatus::Blocked => "blocked",
            TurnStatus::Skipped => "skipped",
        }
    }
}

impl FromStr for TurnStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, <Self as FromStr>::Err> {
        match s {
            "ok" => Ok(TurnStatus::Ok),
            "err" => Ok(TurnStatus::Err),
            "timeout" => Ok(TurnStatus::Timeout),
            "blocked" => Ok(TurnStatus::Blocked),
            "skipped" => Ok(TurnStatus::Skipped),
            other => Err(format!(
                "unknown turn status '{other}' (expected ok/err/timeout/blocked/skipped)"
            )),
        }
    }
}

/// Per-tool-call event. Created by the agent integration layer (the
/// `crux hook post-tool` handler in particular) and persisted via
/// [`crate::DigestEngine::record`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnEvent {
    pub session_id: String,
    pub project_root: String,
    pub agent_id: Option<String>,
    /// Free-form tool name. Convention: native tools use bare names
    /// (`Read`, `Edit`, `Bash`, …) while MCP tools use the
    /// `mcp__<server>__<tool>` form Claude Code already emits.
    pub tool_name: String,
    /// Subject of the call: file path, command, query string, etc.
    pub target: Option<String>,
    pub status: TurnStatus,
    /// Token estimate for the original tool result before any
    /// CRUX-side compression. `0` if unknown.
    pub original_tokens: i64,
    /// Tokens that actually entered the agent's context. `0` if
    /// unknown / equal to `original_tokens`.
    pub compressed_tokens: i64,
    /// One-line human-readable description ("read login.rs (47 LOC)").
    pub summary: String,
}

impl TurnEvent {
    /// Convenience constructor for the common "tool with target" shape.
    pub fn new(
        session_id: impl Into<String>,
        project_root: impl Into<String>,
        tool_name: impl Into<String>,
        target: Option<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            project_root: project_root.into(),
            agent_id: None,
            tool_name: tool_name.into(),
            target,
            status: TurnStatus::Ok,
            original_tokens: 0,
            compressed_tokens: 0,
            summary: summary.into(),
        }
    }
}

/// Persisted turn-event row (post-`record`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTurnEvent {
    pub id: i64,
    pub session_id: String,
    pub project_root: String,
    pub agent_id: Option<String>,
    pub tool_name: String,
    pub target: Option<String>,
    pub status: TurnStatus,
    pub original_tokens: i64,
    pub compressed_tokens: i64,
    pub summary: String,
    pub rolled_up_into: Option<i64>,
    pub created_at_epoch: i64,
}

/// Compact rollup of N consecutive `turn_events` for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnDigest {
    pub id: i64,
    pub session_id: String,
    pub project_root: String,
    pub agent_id: Option<String>,
    pub ts_start_epoch: i64,
    pub ts_end_epoch: i64,
    pub event_count: i64,
    pub original_tokens: i64,
    pub compressed_tokens: i64,
    pub summary: String,
    pub observation_id: Option<i64>,
    pub created_at_epoch: i64,
}

/// Re-export the canonical `[layer.l11]` config from `crux-core` so
/// callers don't have to reach into both crates. The struct lives in
/// core because the global TOML loader needs to deserialize it.
pub use crux_core::config::L11Config;
