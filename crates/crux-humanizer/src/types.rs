use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Concise,
    Casual,
    Professional,
    Developer,
    Social,
    GithubReadme,
}

impl Mode {
    pub const ALL: &'static [Mode] = &[
        Mode::Concise,
        Mode::Casual,
        Mode::Professional,
        Mode::Developer,
        Mode::Social,
        Mode::GithubReadme,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Concise => "concise",
            Mode::Casual => "casual",
            Mode::Professional => "professional",
            Mode::Developer => "developer",
            Mode::Social => "social",
            Mode::GithubReadme => "github-readme",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "concise" => Ok(Mode::Concise),
            "casual" => Ok(Mode::Casual),
            "professional" | "pro" => Ok(Mode::Professional),
            "developer" | "dev" => Ok(Mode::Developer),
            "social" => Ok(Mode::Social),
            "github-readme" | "github" | "readme" | "github_readme" => Ok(Mode::GithubReadme),
            other => Err(format!(
                "unknown humanize mode '{other}' \
                 (want concise|casual|professional|developer|social|github-readme)"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanizeOptions {
    pub collapse_blanks: bool,

    pub strip_pleasantries: bool,

    pub contract: bool,

    pub dedupe_repeats: bool,
}

impl HumanizeOptions {
    pub fn for_mode(mode: Mode) -> Self {
        match mode {
            Mode::Concise => Self {
                collapse_blanks: true,
                strip_pleasantries: true,
                contract: false,
                dedupe_repeats: true,
            },
            Mode::Casual => Self {
                collapse_blanks: true,
                strip_pleasantries: true,
                contract: true,
                dedupe_repeats: true,
            },
            Mode::Professional => Self {
                collapse_blanks: true,
                strip_pleasantries: true,
                contract: false,
                dedupe_repeats: true,
            },
            Mode::Developer => Self {
                collapse_blanks: true,
                strip_pleasantries: true,
                contract: false,
                dedupe_repeats: true,
            },
            Mode::Social => Self {
                collapse_blanks: true,
                strip_pleasantries: true,
                contract: true,
                dedupe_repeats: true,
            },
            Mode::GithubReadme => Self {
                collapse_blanks: false,
                strip_pleasantries: true,
                contract: false,
                dedupe_repeats: true,
            },
        }
    }
}

impl Default for HumanizeOptions {
    fn default() -> Self {
        Self::for_mode(Mode::Concise)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Stats {
    pub original_chars: usize,
    pub rewritten_chars: usize,
    pub original_words: usize,
    pub rewritten_words: usize,
    pub edits_applied: usize,
}

impl Stats {
    pub fn chars_saved(&self) -> usize {
        self.original_chars.saturating_sub(self.rewritten_chars)
    }

    pub fn words_saved(&self) -> usize {
        self.original_words.saturating_sub(self.rewritten_words)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanizeResult {
    pub mode: Mode,
    pub text: String,
    pub stats: Stats,
}
