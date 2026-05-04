//! Public types shared across the memory engine.

use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Eight observation kinds, copied from token-savior. The decay table in
/// `decay_config` is keyed on these strings; renaming a variant here
/// requires a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    /// User preference / persistent personal note.
    User,
    /// Direct correction the user gave us — high signal.
    Feedback,
    /// Project-level decision (e.g., "we use Vue 3 Composition").
    Project,
    /// Reference: pointer to docs, code, external link.
    Reference,
    /// Hard rule that must never decay (security, safety).
    Guardrail,
    /// Known failure mode + how to avoid.
    ErrorPattern,
    /// Architectural choice with rationale.
    Decision,
    /// Naming / style convention.
    Convention,
}

impl ObservationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ObservationKind::User => "user",
            ObservationKind::Feedback => "feedback",
            ObservationKind::Project => "project",
            ObservationKind::Reference => "reference",
            ObservationKind::Guardrail => "guardrail",
            ObservationKind::ErrorPattern => "error_pattern",
            ObservationKind::Decision => "decision",
            ObservationKind::Convention => "convention",
        }
    }

    pub const ALL: &'static [ObservationKind] = &[
        ObservationKind::User,
        ObservationKind::Feedback,
        ObservationKind::Project,
        ObservationKind::Reference,
        ObservationKind::Guardrail,
        ObservationKind::ErrorPattern,
        ObservationKind::Decision,
        ObservationKind::Convention,
    ];
}

impl FromStr for ObservationKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "feedback" => Ok(Self::Feedback),
            "project" => Ok(Self::Project),
            "reference" => Ok(Self::Reference),
            "guardrail" => Ok(Self::Guardrail),
            "error_pattern" => Ok(Self::ErrorPattern),
            "decision" => Ok(Self::Decision),
            "convention" => Ok(Self::Convention),
            other => Err(format!(
                "unknown observation kind '{other}' (expected one of: user, feedback, project, reference, guardrail, error_pattern, decision, convention)"
            )),
        }
    }
}

/// Inputs required to create a new observation. `id`, scoring, hashes,
/// and timestamps are derived inside the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewObservation {
    pub project_root: String,
    pub session_id: Option<i64>,
    pub agent_id: Option<String>,
    pub kind: ObservationKind,
    pub title: String,
    pub content: String,
    pub why: Option<String>,
    pub how_to_apply: Option<String>,
    pub symbol: Option<String>,
    pub file_path: Option<String>,
    pub tags: Vec<String>,
    /// 1..=10. Higher persists longer through decay.
    pub importance: u8,
    pub private: bool,
}

impl NewObservation {
    pub fn minimal(
        project_root: impl Into<String>,
        kind: ObservationKind,
        title: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            session_id: None,
            agent_id: None,
            kind,
            title: title.into(),
            content: content.into(),
            why: None,
            how_to_apply: None,
            symbol: None,
            file_path: None,
            tags: vec![],
            importance: 5,
            private: false,
        }
    }
}

/// Stored observation as returned from the database. `relevance_score` is
/// the live decayed score, not the raw stored value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub id: i64,
    pub project_root: String,
    pub session_id: Option<i64>,
    pub agent_id: Option<String>,
    pub kind: ObservationKind,
    pub title: String,
    pub content: String,
    pub why: Option<String>,
    pub how_to_apply: Option<String>,
    pub symbol: Option<String>,
    pub file_path: Option<String>,
    pub tags: Vec<String>,
    pub importance: u8,
    pub relevance_score: f64,
    pub access_count: i64,
    pub content_hash: String,
    pub archived: bool,
    pub private: bool,
    pub last_accessed_epoch: Option<i64>,
    pub created_at_epoch: i64,
    pub updated_at_epoch: i64,
}

/// Decay-aware ranked result. `score` is post-decay relevance; `rank`
/// is 0-based position in the result list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedObservation {
    pub observation: Observation,
    pub rank: usize,
    pub score: f64,
    pub fts_score: f64,
}

/// Filter for `recall`.
#[derive(Debug, Clone, Default)]
pub struct RecallQuery {
    pub query: String,
    pub project_root: Option<String>,
    pub kinds: Vec<ObservationKind>,
    pub symbol: Option<String>,
    /// When non-empty, restrict to observations whose `file_path` column
    /// exactly matches one of the supplied values. Callers that want to
    /// match both absolute and project-relative forms should pass both.
    pub file_paths: Vec<String>,
    pub limit: usize,
    pub include_archived: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DecayStats {
    pub scanned: usize,
    pub updated: usize,
    pub archived: usize,
}
