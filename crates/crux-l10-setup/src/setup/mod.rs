pub mod agents;
pub mod json_merge;
pub mod skill;
pub mod yaml_merge;

use std::collections::BTreeMap;
use std::path::PathBuf;

use crux_core::error::{CruxError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    Windsurf,
    Cline,
    Zed,
    OpenClaw,
    Hermes,
}

impl AgentKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "claude-code" | "claude_code" | "claudecode" | "claude" => Self::ClaudeCode,
            "claude-desktop" | "claude_desktop" | "claudedesktop" | "desktop" => {
                Self::ClaudeDesktop
            }
            "cursor" => Self::Cursor,
            "windsurf" | "cascade" | "codeium" => Self::Windsurf,
            "cline" => Self::Cline,
            "zed" => Self::Zed,
            "openclaw" | "open-claw" | "open_claw" => Self::OpenClaw,
            "hermes" | "hermes-agent" | "hermes_agent" | "nous" => Self::Hermes,
            _ => return None,
        })
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::ClaudeDesktop => "claude-desktop",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::Cline => "cline",
            Self::Zed => "zed",
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf (Cascade)",
            Self::Cline => "Cline (VS Code)",
            Self::Zed => "Zed",
            Self::OpenClaw => "OpenClaw",
            Self::Hermes => "Hermes Agent",
        }
    }

    pub fn supports_hooks(self) -> bool {
        matches!(self, Self::ClaudeCode)
    }

    pub fn supports_slash_command(self) -> bool {
        matches!(self, Self::ClaudeCode)
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::ClaudeCode,
            Self::ClaudeDesktop,
            Self::Cursor,
            Self::Windsurf,
            Self::Cline,
            Self::Zed,
            Self::OpenClaw,
            Self::Hermes,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
    Auto,
}

#[derive(Debug, Clone)]
pub struct IntegrateOptions {
    pub agent: AgentKind,
    pub scope: Scope,
    pub project_root: PathBuf,
    pub crux_path: String,
    pub env: BTreeMap<String, String>,
    pub install_hooks: bool,
    pub install_skill: bool,
    pub install_hygiene_hook: bool,
    pub remove_hygiene_hook: bool,
    pub dry_run: bool,
    pub force: bool,
}

impl IntegrateOptions {
    pub fn new(agent: AgentKind, project_root: PathBuf) -> Self {
        Self {
            agent,
            scope: Scope::Auto,
            project_root,
            crux_path: default_crux_path(),
            env: BTreeMap::new(),
            install_hooks: agent.supports_hooks(),
            install_skill: agent.supports_slash_command(),
            install_hygiene_hook: false,
            remove_hygiene_hook: false,
            dry_run: false,
            force: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    Created(PathBuf),
    Updated(PathBuf),
    Skipped { path: PathBuf, reason: &'static str },
    Note(String),
}

#[derive(Debug, Clone)]
pub struct IntegrateReport {
    pub agent: &'static str,
    pub actions: Vec<Action>,
}

impl IntegrateReport {
    pub fn new(agent: AgentKind) -> Self {
        Self {
            agent: agent.label(),
            actions: Vec::new(),
        }
    }

    pub fn changed(&self) -> bool {
        self.actions
            .iter()
            .any(|a| !matches!(a, Action::Skipped { .. } | Action::Note(_)))
    }
}

pub fn auto_detect() -> Vec<AgentKind> {
    AgentKind::all()
        .iter()
        .copied()
        .filter(|k| agents::is_installed(*k))
        .collect()
}

pub fn integrate(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    agents::integrate(opts)
}

pub fn default_crux_path() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(s) = exe.to_str() {
            return s.to_string();
        }
    }
    "crux".to_string()
}

pub fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| CruxError::other("could not resolve $HOME"))
}
