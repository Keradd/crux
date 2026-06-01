use serde_json::{json, Value};

use crux_core::tokens;
use crux_l11_digest::DigestEngine;

use crate::dispatch::AppContext;
use crate::tools::common::{project_root, truncate_one_line};
use crate::tools::Tool;

pub fn record_l11_event(
    ctx: &AppContext,
    name: &str,
    args: &Value,
    result: &Result<String, String>,
) {
    if !ctx.config.layers.l11_digest {
        return;
    }
    if matches!(name, "crux_digest" | "crux_compact") {
        return;
    }
    use crux_l11_digest::{TurnEvent, TurnStatus};
    let project = match project_root(ctx) {
        Some(p) => p,
        None => return,
    };
    let session = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string();
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string();
    let target = derive_l11_target(name, args);
    let status = if result.is_ok() {
        TurnStatus::Ok
    } else {
        TurnStatus::Err
    };
    let display_name = format!("mcp__crux__{name}");
    let summary = match (target.as_deref(), status) {
        (Some(t), TurnStatus::Ok) => format!("{display_name} {}", truncate_one_line(t, 80)),
        (Some(t), s) => format!(
            "{display_name} {} [{}]",
            truncate_one_line(t, 80),
            s.as_str()
        ),
        (None, TurnStatus::Ok) => display_name.clone(),
        (None, s) => format!("{display_name} [{}]", s.as_str()),
    };
    let turn = TurnEvent {
        session_id: session,
        project_root: project,
        agent_id: Some(agent_id),
        tool_name: display_name,
        target,
        status,
        original_tokens: 0,
        compressed_tokens: result
            .as_ref()
            .map(|s| tokens::estimate(s) as i64)
            .unwrap_or(0),
        summary,
    };
    let engine = crux_l11_digest::DigestEngine::new(&ctx.conn, ctx.config.layer.l11.clone());
    let _ = engine.record(&turn);
}

fn derive_l11_target(tool_name: &str, args: &Value) -> Option<String> {
    let candidates = [
        "file_path",
        "qualified_name",
        "symbol",
        "command",
        "query",
        "name",
        "pattern",
        "title",
    ];
    for key in candidates {
        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    if tool_name == "crux_execute" {
        if let Some(code) = args.get("code").and_then(|v| v.as_str()) {
            return Some(code.lines().next().unwrap_or(code).to_string());
        }
    }
    None
}

pub fn digest(ctx: &AppContext, args: &Value) -> Result<String, String> {
    if !ctx.config.layers.l11_digest {
        return Err("L11 digest is disabled. Set `[layers] l11_digest = true` \
                    in your .crux/config.toml to enable conversation digests."
            .to_string());
    }
    let session = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string();
    let pending_only = args
        .get("pending_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let engine = DigestEngine::new(&ctx.conn, ctx.config.layer.l11.clone());
    let summary = if pending_only {
        engine.summarize(&session).map_err(|e| e.to_string())?
    } else {
        engine.latest_summary(&session).map_err(|e| e.to_string())?
    };
    Ok(summary)
}

pub fn compact(ctx: &AppContext, args: &Value) -> Result<String, String> {
    if !ctx.config.layers.l11_digest {
        return Err("L11 digest is disabled. Set `[layers] l11_digest = true` \
                    in your .crux/config.toml to enable conversation digests."
            .to_string());
    }
    let session = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string();
    let engine = DigestEngine::new(&ctx.conn, ctx.config.layer.l11.clone());
    let pending = engine.pending_count(&session).map_err(|e| e.to_string())?;
    let d = engine.compact(&session).map_err(|e| e.to_string())?;
    let payload = json!({
        "id": d.id,
        "session_id": d.session_id,
        "event_count": d.event_count,
        "pending_before": pending,
        "ts_start_epoch": d.ts_start_epoch,
        "ts_end_epoch": d.ts_end_epoch,
        "observation_id": d.observation_id,
        "summary": d.summary,
    });
    Ok(serde_json::to_string_pretty(&payload).unwrap())
}

pub struct Digest;
pub struct Compact;

impl Tool for Digest {
    fn name(&self) -> &'static str {
        "crux_digest"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        digest(ctx, args)
    }
}

impl Tool for Compact {
    fn name(&self) -> &'static str {
        "crux_compact"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        compact(ctx, args)
    }
}
