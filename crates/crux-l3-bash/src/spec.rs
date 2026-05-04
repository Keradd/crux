use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FilterFile {
    #[serde(default)]
    pub filters: std::collections::BTreeMap<String, FilterSpec>,
    #[serde(default)]
    pub tests: Vec<FilterTest>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FilterSpec {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub match_command: String,

    #[serde(default)]
    pub strip_ansi: bool,

    #[serde(default)]
    pub replace: Vec<ReplaceRule>,

    #[serde(default)]
    pub match_output: Vec<MatchRule>,

    #[serde(default)]
    pub strip_lines_matching: Vec<String>,

    #[serde(default)]
    pub truncate_lines_at: Option<usize>,

    #[serde(default)]
    pub head_lines: Option<usize>,
    #[serde(default)]
    pub tail_lines: Option<usize>,

    #[serde(default)]
    pub max_lines: Option<usize>,

    #[serde(default)]
    pub on_empty: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReplaceRule {
    pub pattern: String,
    pub replacement: String,
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
