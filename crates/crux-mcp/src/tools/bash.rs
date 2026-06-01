use serde_json::Value;

use crux_core::{telemetry, tokens};
use crux_l3_bash::FilterEngine;

use crate::dispatch::AppContext;
use crate::tools::common::{first_word, project_root};
use crate::tools::Tool;

const MAX_BASH_INPUT: usize = 1_048_576;

pub fn bash_filter(ctx: &AppContext, args: &Value) -> Result<String, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'command'".to_string())?;
    let output = args
        .get("output")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'output'".to_string())?;
    if command.len() > MAX_BASH_INPUT || output.len() > MAX_BASH_INPUT {
        return Err(format!("input exceeds max size ({} bytes)", MAX_BASH_INPUT));
    }
    let engine = FilterEngine::builtin().map_err(|e| e.to_string())?;
    let result = engine.process(command, output);
    let project = project_root(ctx);
    let original_tokens = tokens::estimate(output) as i64;
    let compressed_tokens = tokens::estimate(&result.output.text) as i64;
    let _ = telemetry::record(
        &ctx.conn,
        &telemetry::Event {
            project_root: project.as_deref(),
            layer: "l3",
            feature: &result
                .filter_name
                .as_ref()
                .map(|n| format!("bash:{}", n))
                .unwrap_or_else(|| "bash:passthrough".to_string()),
            agent_id: None,
            session_id: None,
            command_pattern: Some(first_word(command)),
            original_tokens,
            compressed_tokens,
            exec_time_ms: None,
            quality_preserved: true,
            detail: Some(match result.output.kind {
                crux_l3_bash::OutputKind::Matched(_) => "matched",
                crux_l3_bash::OutputKind::OnEmpty => "on_empty",
                crux_l3_bash::OutputKind::Filtered => "filtered",
                crux_l3_bash::OutputKind::Passthrough => "passthrough",
            }),
        },
    );
    Ok(result.output.text)
}

pub struct BashFilter;

impl Tool for BashFilter {
    fn name(&self) -> &'static str {
        "crux_bash_filter"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        bash_filter(ctx, args)
    }
}
