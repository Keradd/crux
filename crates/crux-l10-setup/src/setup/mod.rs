//! `crux setup <agent>` — register CRUX as an MCP server (and hooks
//! where supported) inside third-party AI tools.
//!
//! Supported agents:
//!
//! - **Claude Code** — MCP entry + PreToolUse/PostToolUse hooks +
//!   slash command (`/crux`).
//! - **Claude Desktop** — MCP entry.
//! - **Cursor** — MCP entry.
//! - **Windsurf (Cascade)** — MCP entry.
//! - **Cline** (VS Code extension) — MCP entry in the VS Code
//!   `globalStorage` settings file.
//! - **Zed** — `context_servers` entry (Zed's MCP equivalent).
//!
//! Every operation is idempotent: existing entries are detected and
//! left alone unless they differ. JSON files are read with `serde_json`
//! (plain JSON only — JSONC comments must be stripped).

pub mod agents;
pub mod json_merge;
pub mod skill;
pub mod yaml_merge;

use std::collections::BTreeMap;
use std::path::PathBuf;

use crux_core::error::{CruxError, Result};

/// Which agent to integrate with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    Windsurf,
    Cline,
    Zed,
    /// OpenClaw Gateway (docs.openclaw.ai). MCP client registry lives
    /// at `~/.openclaw/openclaw.json` (or `$OPENCLAW_CONFIG_PATH`)
    /// under the `mcp.servers.<name>` key path. JSON5 comments are
    /// not supported by CRUX's merge — clients with comments will
    /// need to strip them before re-registering.
    OpenClaw,
    /// Hermes Agent (NousResearch). MCP servers are registered in
    /// `~/.hermes/config.yaml` under the top-level `mcp_servers`
    /// mapping. CRUX writes the file as plain YAML; existing keys
    /// are preserved.
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

/// Where to write the integration. `Auto` picks `Global` for agents
/// whose canonical location is the user's home dir (almost all of
/// them), and `Project` only for the small number of agents that
/// strictly require per-project config.
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
    /// Command to invoke inside the registered MCP entry. Default:
    /// the absolute path of the running `crux` binary, falling back
    /// to the bare name `crux`.
    pub crux_path: String,
    /// Environment variables to record in the agent's MCP entry. Some
    /// agents (Windsurf in particular) launch their MCP children from
    /// `$HOME` rather than the project root, so passing
    /// `CRUX_PROJECT=/path/to/repo` here lets CRUX's L5 / L6 tools
    /// target the right project. Use a `BTreeMap` so writes have a
    /// stable order across invocations (idempotency).
    pub env: BTreeMap<String, String>,
    /// Claude Code only: also write PreToolUse / PostToolUse hooks.
    pub install_hooks: bool,
    /// Claude Code only: also write the `/crux` slash-command file.
    pub install_skill: bool,
    /// If true, do not write to disk; only return the planned actions.
    pub dry_run: bool,
    /// Overwrite the slash-command skill file if it already exists.
    /// (MCP / hook entries are merged idempotently regardless.)
    pub force: bool,
}

impl IntegrateOptions {
    /// Sensible defaults for a typical "I just want it to work" run.
    pub fn new(agent: AgentKind, project_root: PathBuf) -> Self {
        Self {
            agent,
            scope: Scope::Auto,
            project_root,
            crux_path: default_crux_path(),
            env: BTreeMap::new(),
            install_hooks: agent.supports_hooks(),
            install_skill: agent.supports_slash_command(),
            dry_run: false,
            force: false,
        }
    }
}

/// Per-action record so the CLI / JSON renderer can describe what
/// `crux setup` did or would do.
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

/// Detect every supported agent that looks installed on this machine.
/// "Installed" is heuristic: presence of the agent's known config
/// directory or executable.
pub fn auto_detect() -> Vec<AgentKind> {
    AgentKind::all()
        .iter()
        .copied()
        .filter(|k| agents::is_installed(*k))
        .collect()
}

/// Run the integration for a single agent.
pub fn integrate(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    agents::integrate(opts)
}

/// Best-effort default for the `crux` binary path: absolute path of
/// the currently-running executable, falling back to the bare name
/// `"crux"` (which assumes `crux` is on `$PATH` of the spawned MCP
/// child process — typically the case after `cargo install` or a
/// release-binary install into `/usr/local/bin`).
pub fn default_crux_path() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(s) = exe.to_str() {
            return s.to_string();
        }
    }
    "crux".to_string()
}

/// Resolve `~`/`$HOME`. Returns an error if neither is available.
pub fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| CruxError::other("could not resolve $HOME"))
}
