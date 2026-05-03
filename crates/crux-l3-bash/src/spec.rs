//! TOML filter specification (deserialized form).
//!
//! Each filter file (built-in or user-supplied) is a single TOML document
//! describing 1+ filters keyed by name. Example:
//!
//! ```toml
//! [filters.git-status]
//! match_command = "^git\\s+status\\b"
//! strip_ansi    = true
//! truncate_lines_at = 200
//! head_lines    = 30
//! max_lines     = 60
//! on_empty      = "git status: clean"
//!
//! [[filters.git-status.replace]]
//! pattern     = "(?m)^On branch (\\S+).*$"
//! replacement = "branch: $1"
//!
//! [[filters.git-status.match_output]]
//! pattern = "nothing to commit, working tree clean"
//! message = "git status: clean"
//!
//! [[tests]]
//! name     = "clean"
//! filter   = "git-status"
//! input    = "On branch main\nnothing to commit, working tree clean\n"
//! expected = "git status: clean"
//! ```
//!
//! The `[[tests]]` array is optional and only consumed by the test harness;
//! the runtime ignores it.

use serde::{Deserialize, Serialize};

/// Top-level structure of a `<name>.toml` filter file.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FilterFile {
    #[serde(default)]
    pub filters: std::collections::BTreeMap<String, FilterSpec>,
    /// Optional inline tests for this file. Validated by `cargo test`.
    #[serde(default)]
    pub tests: Vec<FilterTest>,
}

/// One filter — the 8-stage pipeline plus a command matcher.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FilterSpec {
    /// Human description (free text).
    #[serde(default)]
    pub description: String,
    /// Regex applied to the *full command line*. The first filter whose
    /// `match_command` matches wins. Required for all real filters.
    #[serde(default)]
    pub match_command: String,

    // ── Stage 1: ANSI stripping ──
    #[serde(default)]
    pub strip_ansi: bool,

    // ── Stage 2: line-by-line regex substitutions ──
    #[serde(default)]
    pub replace: Vec<ReplaceRule>,

    // ── Stage 3: short-circuit on output match ──
    #[serde(default)]
    pub match_output: Vec<MatchRule>,

    // ── Stage 4: drop lines matching any of these regexes ──
    #[serde(default)]
    pub strip_lines_matching: Vec<String>,

    // ── Stage 5: per-line truncation ──
    #[serde(default)]
    pub truncate_lines_at: Option<usize>,

    // ── Stage 6: head/tail keep ──
    #[serde(default)]
    pub head_lines: Option<usize>,
    #[serde(default)]
    pub tail_lines: Option<usize>,

    // ── Stage 7: absolute line cap ──
    #[serde(default)]
    pub max_lines: Option<usize>,

    // ── Stage 8: empty-output fallback ──
    #[serde(default)]
    pub on_empty: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReplaceRule {
    pub pattern: String,
    pub replacement: String,
    /// Apply across the whole text instead of per line. Default: false.
    #[serde(default)]
    pub multiline: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MatchRule {
    pub pattern: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FilterTest {
    pub name: String,
    pub filter: String,
    pub input: String,
    pub expected: String,
}
