//! `crux hook` — agent integration entry points.
//!
//! Phase 1 implements the PreToolUse path for the `Read` tool. The protocol
//! is the one Claude Code uses: a JSON event on stdin, a JSON response on
//! stdout, a non-zero exit code if the read should be blocked.
//!
//! Event shape (subset we care about):
//! ```json
//! {
//!   "tool_name": "Read",
//!   "tool_input": { "file_path": "/abs/path", "offset": 0, "limit": 0 },
//!   "session_id": "...",
//!   "agent_id": "..."
//! }
//! ```
//!
//! Response shape:
//! ```json
//! { "decision": "allow" | "block", "message": "..." }
//! ```

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

use crux_core::Runtime;
use crux_l11_digest::{DigestEngine, TurnEvent, TurnStatus};
use crux_l4_readcache::{CacheDecision, CheckOptions, ContextIgnore, ReadCacheManager, ReadEvent};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// PreToolUse handler. Reads a JSON event from stdin.
    PreTool,
    /// PostToolUse handler. Reads a JSON event from stdin.
    PostTool,
    /// Session lifecycle hooks (placeholder).
    SessionStart,
    SessionEnd,
}

pub fn run(cli: &Cli, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::PreTool => pre_tool(cli),
        Cmd::PostTool => post_tool(cli),
        Cmd::SessionStart => not_yet("session-start", "Phase 6"),
        Cmd::SessionEnd => not_yet("session-end", "Phase 6"),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Event types
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ToolEvent {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: ToolInput,
    #[serde(default)]
    tool_response: ToolResponse,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    agent_id: String,
}

#[derive(Debug, Default, Deserialize)]
struct ToolInput {
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
    /// Bash uses `command`; we extract it for L11 turn-event targets.
    #[serde(default)]
    command: Option<String>,
    /// Grep / Glob / mcp search tools use `pattern`/`query`.
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    query: Option<String>,
    /// Symbol-style targets (`crux_get_symbol_source`, `crux_find_symbol`).
    #[serde(default)]
    qualified_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ToolResponse {
    /// Claude Code sets this when a tool call fails. Present + truthy
    /// → L11 marks the event as `err`.
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
struct HookResponse<'a> {
    decision: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────
// pre-tool
// ─────────────────────────────────────────────────────────────────────────

fn pre_tool(cli: &Cli) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;

    let event = read_event_from_stdin().context("reading hook event from stdin")?;

    // Only Read is in scope for Phase 1. Pass everything else through.
    if !matches!(event.tool_name.as_str(), "Read") {
        respond(&HookResponse {
            decision: "allow",
            message: None,
        });
        return Ok(());
    }

    let Some(file_path) = event.tool_input.file_path.as_deref() else {
        respond(&HookResponse {
            decision: "allow",
            message: None,
        });
        return Ok(());
    };

    if !runtime.config.layers.l4_read_cache {
        respond(&HookResponse {
            decision: "allow",
            message: None,
        });
        return Ok(());
    }

    let mgr = ReadCacheManager::new(&runtime.conn);
    let path_buf = PathBuf::from(file_path);
    let agent_id = if event.agent_id.is_empty() {
        "default"
    } else {
        event.agent_id.as_str()
    };
    let session_id = if event.session_id.is_empty() {
        "default"
    } else {
        event.session_id.as_str()
    };

    // Compose per-call options from config + project files. The
    // contextignore engine reads `<project>/.crux/contextignore` and
    // (optionally) `$CRUX_HOME/contextignore`.
    let crux_home = crux_core::paths::crux_home().ok();
    let ci = ContextIgnore::load(&project, crux_home.as_deref());
    let opts = CheckOptions {
        contextignore: Some(ci),
        delta_max_bytes: Some(runtime.config.layer.l4.delta_max_bytes),
    };

    let decision = mgr.check_with(
        &ReadEvent {
            agent_id,
            session_id,
            project_root: &project,
            file_path: &path_buf,
            offset: event.tool_input.offset.unwrap_or(0),
            limit: event.tool_input.limit.unwrap_or(0),
        },
        &opts,
    )?;

    let mode = runtime.config.modes.l4_read_cache;
    let resp = match (decision, mode) {
        (CacheDecision::Allow, _) => HookResponse {
            decision: "allow",
            message: None,
        },
        (CacheDecision::Blocked { reason }, _) => HookResponse {
            decision: "block",
            message: Some(format!("crux: blocked — {}", reason)),
        },
        (CacheDecision::Redundant { digest, read_count }, crux_core::LayerMode::Block) => {
            HookResponse {
                decision: "block",
                message: Some(format!(
                    "crux: file already in context (read #{}). digest:\n{}",
                    read_count, digest
                )),
            }
        }
        (CacheDecision::Redundant { digest, read_count }, _) => HookResponse {
            decision: "allow",
            message: Some(format!(
                "crux: redundant read #{} — digest available: {} chars",
                read_count,
                digest.len()
            )),
        },
        // Delta in block mode: hand the diff back to the agent instead of
        // letting the full file re-enter context.
        (
            CacheDecision::Delta {
                summary,
                body,
                read_count,
            },
            crux_core::LayerMode::Block,
        ) => HookResponse {
            decision: "block",
            message: Some(format!(
                "crux: file changed since read #{} — diff {}\n\n{}",
                read_count - 1,
                summary,
                body
            )),
        },
        // Delta in warn / shadow modes: still allow the full read but
        // preview the diff so the agent knows what changed.
        (
            CacheDecision::Delta {
                summary,
                body,
                read_count,
            },
            _,
        ) => HookResponse {
            decision: "allow",
            message: Some(format!(
                "crux: changed since read #{} — diff {}\n{}",
                read_count - 1,
                summary,
                body
            )),
        },
    };

    respond(&resp);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// post-tool
// ─────────────────────────────────────────────────────────────────────────

fn post_tool(cli: &Cli) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;

    let event = read_event_from_stdin().context("reading hook event from stdin")?;

    let agent_id = if event.agent_id.is_empty() {
        "default".to_string()
    } else {
        event.agent_id.clone()
    };
    let session_id = if event.session_id.is_empty() {
        "default".to_string()
    } else {
        event.session_id.clone()
    };

    if matches!(
        event.tool_name.as_str(),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit"
    ) {
        if let Some(fp) = event.tool_input.file_path.as_deref() {
            let mgr = ReadCacheManager::new(&runtime.conn);
            let path_buf = PathBuf::from(fp);
            mgr.invalidate(&agent_id, &session_id, &project, &path_buf)?;
        }
    }

    if runtime.config.layers.l11_digest && !event.tool_name.is_empty() {
        let digest = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());
        let target = derive_target(&event);
        let status = derive_status(&event);
        let summary = build_summary(&event.tool_name, target.as_deref(), status);
        let turn = TurnEvent {
            session_id: session_id.clone(),
            project_root: project.display().to_string(),
            agent_id: Some(agent_id.clone()),
            tool_name: event.tool_name.clone(),
            target,
            status,
            original_tokens: 0,
            compressed_tokens: 0,
            summary,
        };
        // Record best-effort: a digest failure must never block the
        // post-tool hook (which is otherwise pure infrastructure).
        let _ = digest.record(&turn);
    }

    respond(&HookResponse {
        decision: "allow",
        message: None,
    });
    Ok(())
}

fn derive_target(ev: &ToolEvent) -> Option<String> {
    let i = &ev.tool_input;
    if let Some(fp) = i.file_path.as_deref() {
        return Some(fp.to_string());
    }
    if let Some(cmd) = i.command.as_deref() {
        return Some(cmd.to_string());
    }
    if let Some(q) = i.query.as_deref() {
        return Some(q.to_string());
    }
    if let Some(p) = i.pattern.as_deref() {
        return Some(p.to_string());
    }
    if let Some(qn) = i.qualified_name.as_deref() {
        return Some(qn.to_string());
    }
    if let Some(n) = i.name.as_deref() {
        return Some(n.to_string());
    }
    None
}

fn derive_status(ev: &ToolEvent) -> TurnStatus {
    if ev.tool_response.is_error.unwrap_or(false)
        || ev
            .tool_response
            .error
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    {
        TurnStatus::Err
    } else {
        TurnStatus::Ok
    }
}

fn build_summary(tool: &str, target: Option<&str>, status: TurnStatus) -> String {
    let base = match target {
        Some(t) if !t.is_empty() => {
            // Truncate noisy targets to keep the summary one-liner sane.
            let trimmed = t.lines().next().unwrap_or(t);
            if trimmed.len() > 80 {
                format!("{tool} {}…", &trimmed[..80])
            } else {
                format!("{tool} {}", trimmed)
            }
        }
        _ => tool.to_string(),
    };
    match status {
        TurnStatus::Ok => base,
        other => format!("{base} [{}]", other.as_str()),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// helpers
// ─────────────────────────────────────────────────────────────────────────

fn read_event_from_stdin() -> Result<ToolEvent> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Ok(ToolEvent {
            tool_name: String::new(),
            tool_input: ToolInput::default(),
            tool_response: ToolResponse::default(),
            session_id: String::new(),
            agent_id: String::new(),
        });
    }
    let event: ToolEvent = serde_json::from_str(&buf)
        .with_context(|| format!("hook event was not valid JSON: {}", truncate(&buf, 200)))?;
    Ok(event)
}

fn respond(resp: &HookResponse<'_>) {
    let s = serde_json::to_string(resp).unwrap_or_else(|_| "{\"decision\":\"allow\"}".into());
    println!("{}", s);
}

fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n {
        s
    } else {
        &s[..n]
    }
}

fn not_yet(name: &str, phase: &str) -> Result<()> {
    Err(anyhow::anyhow!(
        "`crux hook {}` is not implemented yet — see {} in docs/CRUX-DESIGN.md",
        name,
        phase
    ))
}
