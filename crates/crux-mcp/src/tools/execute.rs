use serde_json::{json, Value};

use crux_l7_sandbox::{agent_perms, ExecRequest, Executor, IsolationLevel, RuntimeKind};

use crate::dispatch::AppContext;
use crate::tools::common::project_root_path;
use crate::tools::Tool;

pub fn execute(ctx: &AppContext, args: &Value) -> Result<String, String> {
    if !ctx.config.layers.l7_sandbox {
        return Err("L7 sandbox is disabled. Set `[layers] l7_sandbox = true` \
                    in ~/.crux/config.toml (or project `.crux/config.toml`) \
                    to enable `crux_execute`. Default isolation is portable \
                    (subprocess + timeout + no network) — no system deps."
            .to_string());
    }
    let runtime_kind_s = args
        .get("runtime")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'runtime'".to_string())?;
    let runtime_kind = RuntimeKind::parse(runtime_kind_s)
        .ok_or_else(|| format!("unknown runtime '{runtime_kind_s}' (want python|bash|node)"))?;
    let code = args
        .get("code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'code'".to_string())?
        .to_string();
    if code.trim().is_empty() {
        return Err("'code' is empty".to_string());
    }
    let timeout_seconds = args
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .clamp(1, 60);
    let max_output_bytes = args
        .get("max_output_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(65_536) as usize;
    let inherit_env = args
        .get("inherit_env")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let isolation = args
        .get("isolation")
        .and_then(|v| v.as_str())
        .and_then(IsolationLevel::parse)
        .unwrap_or_default();

    let permissions = agent_perms::load_for_project(project_root_path(ctx).as_deref());
    let req = ExecRequest {
        runtime: runtime_kind,
        code,
        project_root: project_root_path(ctx),
        timeout: std::time::Duration::from_secs(timeout_seconds),
        max_output_bytes,
        env: std::collections::HashMap::new(),
        inherit_env,
        isolation,
        permissions: Some(permissions),
    };
    let exec = Executor::new();
    let res = exec.execute(&req).map_err(|e| e.to_string())?;
    let payload = json!({
        "runtime":            res.runtime.as_str(),
        "exit_code":          res.exit_code,
        "timed_out":          res.timed_out,
        "stdout":             res.stdout,
        "stderr":             res.stderr,
        "stdout_truncated":   res.stdout_truncated,
        "stderr_truncated":   res.stderr_truncated,
        "elapsed_ms":         res.elapsed_ms,
        "isolation_applied":  res.isolation_applied,
    });
    Ok(serde_json::to_string_pretty(&payload).unwrap())
}

pub struct Execute;

impl Tool for Execute {
    fn name(&self) -> &'static str {
        "crux_execute"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        execute(ctx, args)
    }
}
