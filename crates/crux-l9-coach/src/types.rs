//! Public types for the coach engine.
//!
//! Mirrors the schema from alex/token-optimizer so porting telemetry
//! dashboards later is a rename, not a rewrite.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub name: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub savings: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub earned: bool,
}

impl Pattern {
    pub fn good(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
            severity: None,
            fix: None,
            savings: None,
            earned: true,
        }
    }
    pub fn bad(name: &str, detail: impl Into<String>, severity: Severity) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
            severity: Some(severity),
            fix: None,
            savings: None,
            earned: false,
        }
    }
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
    pub fn with_savings(mut self, savings: impl Into<String>) -> Self {
        self.savings = Some(savings.into());
        self
    }
}

/// Derived health snapshot — produced by `CoachEngine::snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoachData {
    pub health_score: i32,
    pub grade: char,
    pub patterns_good: Vec<Pattern>,
    pub patterns_bad: Vec<Pattern>,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub context_window: u32,
    pub claude_md_tokens: u32,
    pub claude_md_pct: f64,
    pub total_savings_tokens: i64,
    pub total_original_tokens: i64,
    pub savings_pct: f64,
    pub telemetry_events: i64,
    pub l4_cache_hits: i64,
    pub memory_observations: i64,
    pub active_layers: u32,
    pub unused_layers: u32,
}

/// Outcome from `check_loop`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopCheckResult {
    pub is_loop: bool,
    pub similarity: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Outcome from `check_drift`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftCheckResult {
    pub changed: bool,
    pub previous_hash: Option<String>,
    pub current_hash: String,
    pub tokens_est: u32,
    pub byte_size: u64,
    pub history_depth: u32,
}

/// Score → letter grade. Thresholds match alex/token-optimizer so
/// existing dashboards don't need relabeling.
pub fn score_to_grade(score: i32) -> char {
    match score {
        90..=i32::MAX => 'A',
        80..=89 => 'B',
        70..=79 => 'C',
        60..=69 => 'D',
        _ => 'F',
    }
}
