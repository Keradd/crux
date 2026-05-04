use serde::{Deserialize, Serialize};
use std::str::FromStr;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnEvent {
    pub session_id: String,
    pub project_root: String,
    pub agent_id: Option<String>,
    pub tool_name: String,
    pub target: Option<String>,
    pub status: TurnStatus,
    pub original_tokens: i64,
    pub compressed_tokens: i64,
    pub summary: String,
}

impl TurnEvent {
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

pub use crux_core::config::L11Config;
