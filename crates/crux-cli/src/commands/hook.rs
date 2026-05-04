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
    PreTool,
    PostTool,
    SessionStart,
    SessionEnd,
    OpenclawCompact,
}

pub fn run(cli: &Cli, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::PreTool => pre_tool(cli),
        Cmd::PostTool => post_tool(cli),
        Cmd::SessionStart => not_yet("session-start", "Phase 6"),
        Cmd::SessionEnd => not_yet("session-end", "Phase 6"),
        Cmd::OpenclawCompact => openclaw_compact(cli),
    }
}

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
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    qualified_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ToolResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    is_error: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct CompactEvent {
    #[serde(default, alias = "sessionId")]
    session_id: String,
    #[serde(default, alias = "projectDir", alias = "project_dir")]
    cwd: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
}

#[derive(Debug, Serialize)]
struct HookResponse<'a> {
    decision: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

fn pre_tool(cli: &Cli) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;

    let event = read_event_from_stdin().context("reading hook event from stdin")?;

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

#[derive(Debug, Clone)]
struct CompactOutcome {
    session_id: String,
    digest_id: i64,
    event_count: i64,
    pending_before: i64,
    trigger: Option<String>,
    skipped: Option<&'static str>,
}

impl CompactOutcome {
    fn skipped(session_id: String, event: &CompactEvent, reason: &'static str) -> Self {
        Self {
            session_id,
            digest_id: 0,
            event_count: 0,
            pending_before: 0,
            trigger: event.trigger.clone(),
            skipped: Some(reason),
        }
    }

    fn message(&self) -> String {
        if let Some(reason) = self.skipped {
            return format!("crux: openclaw-compact skipped — {reason}");
        }
        let trig = self.trigger.as_deref().unwrap_or("openclaw");
        format!(
            "crux: compacted session={} via {} → digest #{} ({} event(s), {} pending before)",
            self.session_id, trig, self.digest_id, self.event_count, self.pending_before,
        )
    }
}

fn openclaw_compact(cli: &Cli) -> Result<()> {
    let event = read_compact_event_from_stdin().context("reading compact hook event from stdin")?;

    let project = match event.cwd.as_deref().filter(|s| !s.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => resolve_project_root(cli.project.as_deref()),
    };
    let runtime = Runtime::open(Some(project.clone())).context("opening CRUX runtime")?;

    let outcome = perform_compact(&runtime, &event);
    respond(&HookResponse {
        decision: "allow",
        message: Some(outcome.message()),
    });
    Ok(())
}

fn perform_compact(runtime: &Runtime, event: &CompactEvent) -> CompactOutcome {
    let session_id = normalize_session_id(&event.session_id);
    if !runtime.config.layers.l11_digest {
        return CompactOutcome::skipped(session_id, event, "l11_digest layer disabled");
    }
    let engine = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());
    compact_session(&engine, event)
}

fn normalize_session_id(raw: &str) -> String {
    if raw.trim().is_empty() {
        "default".to_string()
    } else {
        raw.trim().to_string()
    }
}

fn compact_session(engine: &DigestEngine<'_>, event: &CompactEvent) -> CompactOutcome {
    let session_id = normalize_session_id(&event.session_id);
    let pending_before = engine.pending_count(&session_id).unwrap_or(0);
    match engine.compact(&session_id) {
        Ok(digest) => CompactOutcome {
            session_id,
            digest_id: digest.id,
            event_count: digest.event_count,
            pending_before,
            trigger: event.trigger.clone(),
            skipped: None,
        },
        Err(_) => CompactOutcome {
            session_id,
            digest_id: 0,
            event_count: 0,
            pending_before,
            trigger: event.trigger.clone(),
            skipped: Some("compact failed"),
        },
    }
}

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

fn read_compact_event_from_stdin() -> Result<CompactEvent> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    parse_compact_event(&buf)
}

fn parse_compact_event(raw: &str) -> Result<CompactEvent> {
    if raw.trim().is_empty() {
        return Ok(CompactEvent::default());
    }
    serde_json::from_str(raw)
        .with_context(|| format!("compact event was not valid JSON: {}", truncate(raw, 200)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crux_core::config::L11Config;
    use crux_l11_digest::{DigestEngine, TurnEvent, TurnStatus};

    fn ev(session: &str, tool: &str) -> TurnEvent {
        TurnEvent {
            session_id: session.into(),
            project_root: "/p".into(),
            agent_id: Some("default".into()),
            tool_name: tool.into(),
            target: Some("x".into()),
            status: TurnStatus::Ok,
            original_tokens: 0,
            compressed_tokens: 0,
            summary: format!("{tool} x"),
        }
    }

    fn cfg() -> L11Config {
        L11Config {
            auto_compact_every_n: 0,
            mirror_to_l8: false,
            ..L11Config::default()
        }
    }

    #[test]
    fn parse_empty_returns_default_event() {
        let ev = parse_compact_event("").unwrap();
        assert!(ev.session_id.is_empty());
        assert!(ev.cwd.is_none());
        assert!(ev.trigger.is_none());

        let ev_ws = parse_compact_event("   \n\t  ").unwrap();
        assert!(ev_ws.session_id.is_empty());
    }

    #[test]
    fn parse_snake_case_fields() {
        let raw = r#"{"session_id":"sess-1","cwd":"/proj","trigger":"manual"}"#;
        let ev = parse_compact_event(raw).unwrap();
        assert_eq!(ev.session_id, "sess-1");
        assert_eq!(ev.cwd.as_deref(), Some("/proj"));
        assert_eq!(ev.trigger.as_deref(), Some("manual"));
    }

    #[test]
    fn parse_camel_case_aliases() {
        let raw = r#"{"sessionId":"sess-2","projectDir":"/proj","trigger":"auto"}"#;
        let ev = parse_compact_event(raw).unwrap();
        assert_eq!(ev.session_id, "sess-2");
        assert_eq!(ev.cwd.as_deref(), Some("/proj"));
        assert_eq!(ev.trigger.as_deref(), Some("auto"));
    }

    #[test]
    fn parse_project_dir_snake_alias() {
        let raw = r#"{"session_id":"s","project_dir":"/proj"}"#;
        let ev = parse_compact_event(raw).unwrap();
        assert_eq!(ev.cwd.as_deref(), Some("/proj"));
    }

    #[test]
    fn parse_invalid_json_errors() {
        let err = parse_compact_event("not json").unwrap_err();
        assert!(err.to_string().contains("compact event was not valid JSON"));
    }

    #[test]
    fn normalize_session_defaults_when_empty() {
        assert_eq!(normalize_session_id(""), "default");
        assert_eq!(normalize_session_id("   \n\t"), "default");
    }

    #[test]
    fn normalize_session_trims_whitespace() {
        assert_eq!(normalize_session_id("  spaced  "), "spaced");
    }

    #[test]
    fn outcome_skipped_message_format() {
        let event = CompactEvent {
            session_id: "s".into(),
            ..CompactEvent::default()
        };
        let out = CompactOutcome::skipped("s".into(), &event, "l11_digest layer disabled");
        assert_eq!(out.skipped, Some("l11_digest layer disabled"));
        assert_eq!(out.session_id, "s");
        assert!(out.message().contains("l11_digest layer disabled"));
        assert!(out.message().starts_with("crux: openclaw-compact skipped"));
    }

    #[test]
    fn compact_defaults_session_when_empty() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let engine = DigestEngine::new(&conn, cfg());
        let event = CompactEvent::default();
        let out = compact_session(&engine, &event);
        assert_eq!(out.session_id, "default");
        assert!(out.skipped.is_none());
        assert_eq!(out.event_count, 0);
    }

    #[test]
    fn compact_rolls_up_pending_events() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let engine = DigestEngine::new(&conn, cfg());
        engine.record(&ev("sess-1", "Read")).unwrap();
        engine.record(&ev("sess-1", "Edit")).unwrap();

        let event = CompactEvent {
            session_id: "sess-1".into(),
            trigger: Some("auto".into()),
            ..CompactEvent::default()
        };
        let out = compact_session(&engine, &event);

        assert!(out.skipped.is_none());
        assert_eq!(out.session_id, "sess-1");
        assert_eq!(out.event_count, 2);
        assert_eq!(out.pending_before, 2);
        assert!(out.digest_id > 0);
        let msg = out.message();
        assert!(msg.contains("session=sess-1"));
        assert!(msg.contains("via auto"));
        assert!(msg.contains(&format!("digest #{}", out.digest_id)));
    }

    #[test]
    fn compact_trims_session_whitespace() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let engine = DigestEngine::new(&conn, cfg());
        let event = CompactEvent {
            session_id: "  spaced  ".into(),
            ..CompactEvent::default()
        };
        let out = compact_session(&engine, &event);
        assert_eq!(out.session_id, "spaced");
    }

    #[test]
    fn compact_message_default_trigger_label() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let engine = DigestEngine::new(&conn, cfg());
        engine.record(&ev("s2", "Bash")).unwrap();
        let event = CompactEvent {
            session_id: "s2".into(),
            ..CompactEvent::default()
        };
        let out = compact_session(&engine, &event);
        assert!(out.message().contains("via openclaw"));
    }

    #[test]
    fn compact_idempotent_when_no_new_events() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let engine = DigestEngine::new(&conn, cfg());
        engine.record(&ev("s3", "Read")).unwrap();
        let event = CompactEvent {
            session_id: "s3".into(),
            ..CompactEvent::default()
        };
        let first = compact_session(&engine, &event);
        assert_eq!(first.event_count, 1);
        let second = compact_session(&engine, &event);
        assert!(second.skipped.is_none());
        assert_eq!(second.event_count, 0);
        assert_eq!(second.pending_before, 0);
        assert_ne!(first.digest_id, second.digest_id);
    }
}
