//! Built-in Layer 1 profiles (output-compression rules).
//!
//! These are baked into the binary so `crux init --profile coding` works
//! offline. Future versions will support overlay profiles in
//! `~/.config/crux/profiles/<name>.toml`.
//!
//! Sources for the rule wording: drona23/claude-token-efficient and
//! juliusbrussee/caveman, with adjustments for CRUX terminology.

#[derive(Debug, Clone, Copy)]
pub struct Profile {
    pub name: &'static str,
    pub description: &'static str,
    pub claude_md: &'static str,
}

pub const CODING: Profile = Profile {
    name: "coding",
    description: "Dev projects, code review, debugging, refactoring.",
    claude_md: include_str!("../profiles/coding.md"),
};

pub const ANALYSIS: Profile = Profile {
    name: "analysis",
    description: "Research, exploration, summarization tasks.",
    claude_md: include_str!("../profiles/analysis.md"),
};

pub const AGENTS: Profile = Profile {
    name: "agents",
    description: "Autonomous multi-step agent workflows.",
    claude_md: include_str!("../profiles/agents.md"),
};

pub const ALL: &[Profile] = &[CODING, ANALYSIS, AGENTS];

pub fn get(name: &str) -> Option<Profile> {
    ALL.iter().find(|p| p.name == name).copied()
}
