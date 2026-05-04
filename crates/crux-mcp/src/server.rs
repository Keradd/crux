//! Stdio-based MCP server.
//!
//! Reads one JSON-RPC envelope per line of stdin, dispatches it, writes
//! the response (also one line) to stdout. Nothing else gets written to
//! stdout — logs go to stderr through `tracing`.
//!
//! `serve_stdio` blocks until stdin EOF or an unrecoverable IO error.
//!
//! ## Hot config reload
//!
//! A long-lived `crux mcp` session spans many agent tool calls. If the
//! user edits `~/.crux/config.toml` (or the project's
//! `.crux/config.toml`) mid-session, we want the next request to see
//! the new layer flags without needing a restart. A
//! [`ConfigWatcher`](crux_core::ConfigWatcher) is built from the
//! `Runtime` at startup and consulted between requests via
//! [`reload_if_changed`] — on an mtime change the new config is parsed,
//! validated, and swapped into `runtime.config`. Malformed edits are
//! logged and ignored so a typo never takes the server down.

use std::io::{BufRead, BufReader, Write};

use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crux_core::{ConfigWatcher, Runtime};

use crate::dispatch;
use crate::protocol::{
    self, CallToolParams, CallToolResult, InitializeResult, ListToolsResult, Request, Response,
    RpcError, ServerCapabilities, ServerInfo, ToolsCapability,
};
use crate::tools;

/// Block on stdin/stdout serving MCP requests until EOF or IO error.
///
/// A [`ConfigWatcher`] built from `runtime` is consulted between every
/// request line so edits to the global or project config file are
/// picked up without restarting the server.
pub fn serve_stdio(runtime: Runtime) -> std::io::Result<()> {
    let stdin = std::io::stdin().lock();
    let reader = BufReader::new(stdin);
    let stdout = std::io::stdout().lock();
    let watcher = ConfigWatcher::from_runtime(&runtime);
    run_loop(runtime, reader, stdout, Some(&watcher))
}

/// Core request loop factored out of [`serve_stdio`] so tests can drive
/// it with arbitrary `BufRead`/`Write` pairs and simulate config edits
/// between requests. `watcher` is optional: pass `None` to disable
/// hot-reload (useful for deterministic tests).
pub(crate) fn run_loop<R: BufRead, W: Write>(
    mut runtime: Runtime,
    mut reader: R,
    mut writer: W,
    watcher: Option<&ConfigWatcher>,
) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        if let Some(w) = watcher {
            reload_if_changed(&mut runtime, w);
        }

        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            // EOF
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = handle_line(&runtime, trimmed);
        if let Some(resp) = response {
            let s = serde_json::to_string(&resp)
                .unwrap_or_else(|_| "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32603,\"message\":\"serialize\"}}".into());
            writeln!(writer, "{s}")?;
            writer.flush()?;
        }
    }
}

/// Check the watcher for mtime changes and swap `runtime.config` on
/// reload. Returns `true` if a reload happened. Parse failures inside
/// the watcher are already logged at `warn`; here we only log the
/// success case at `info` so operators can confirm edits landed.
pub(crate) fn reload_if_changed(runtime: &mut Runtime, watcher: &ConfigWatcher) -> bool {
    match watcher.tick() {
        Ok(true) => {
            runtime.config = watcher.snapshot();
            info!("crux.toml changed on disk — reloaded config");
            true
        }
        Ok(false) => false,
        Err(e) => {
            warn!(error = %e, "config watcher tick failed");
            false
        }
    }
}

fn handle_line(runtime: &Runtime, line: &str) -> Option<Response> {
    let req: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            warn!(?e, "could not parse request");
            return Some(Response::err(
                Value::Null,
                RpcError::parse_error(format!("invalid JSON: {e}")),
            ));
        }
    };

    if req.jsonrpc != "2.0" {
        return Some(Response::err(
            req.id.clone().unwrap_or(Value::Null),
            RpcError::invalid_request(format!("expected jsonrpc=2.0, got {}", req.jsonrpc)),
        ));
    }

    // Notifications (no id) get processed but no reply is sent.
    let id = req.id.clone();
    let result = dispatch_request(runtime, &req);

    match (id, result) {
        (Some(id), Ok(value)) => Some(Response::ok(id, value)),
        (Some(id), Err(err)) => Some(Response::err(id, err)),
        (None, _) => None,
    }
}

fn dispatch_request(runtime: &Runtime, req: &Request) -> Result<Value, RpcError> {
    debug!(method = %req.method, "dispatch");
    match req.method.as_str() {
        "initialize" => Ok(serde_json::to_value(InitializeResult {
            protocol_version: protocol::PROTOCOL_VERSION,
            capabilities: ServerCapabilities {
                tools: ToolsCapability {
                    list_changed: false,
                },
            },
            server_info: ServerInfo {
                name: protocol::SERVER_NAME,
                version: protocol::SERVER_VERSION,
            },
            instructions: Some(
                "CRUX exposes token-optimization tools: crux_remember, crux_recall, \
                 crux_read, crux_bash_filter, crux_audit. Prefer these over raw Read/Bash \
                 to keep context lean.",
            ),
        })
        .map_err(|e| RpcError::internal(e.to_string()))?),
        // `notifications/initialized` is a one-way notification per the MCP
        // spec. We accept it and reply with `null`; real notifications get
        // dropped earlier (no id).
        "initialized" | "notifications/initialized" => Ok(Value::Null),
        "tools/list" => Ok(serde_json::to_value(ListToolsResult {
            tools: tools::all_tools(),
        })
        .map_err(|e| RpcError::internal(e.to_string()))?),
        "tools/call" => {
            let params: CallToolParams = serde_json::from_value(req.params.clone())
                .map_err(|e| RpcError::invalid_params(format!("invalid tools/call params: {e}")))?;
            let result: CallToolResult = dispatch::call(runtime, &params.name, &params.arguments);
            Ok(serde_json::to_value(result).map_err(|e| RpcError::internal(e.to_string()))?)
        }
        // Required no-op responses to keep clients happy.
        "ping" => Ok(json!({})),
        other => Err(RpcError::method_not_found(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;

    /// `$CRUX_HOME` is a process-global env var, but tests across
    /// multiple files can be run in parallel by cargo. Serialize every
    /// watcher-touching test in this module through a single mutex so
    /// we don't race on env mutations.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_crux_home<R>(f: impl FnOnce(&Path) -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var("CRUX_HOME").ok();
        std::env::set_var("CRUX_HOME", dir.path());
        let r = f(dir.path());
        match prev {
            Some(v) => std::env::set_var("CRUX_HOME", v),
            None => std::env::remove_var("CRUX_HOME"),
        }
        r
    }

    fn write_project_config(project: &Path, body: &str) {
        let path = project.join(".crux").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    #[test]
    fn reload_if_changed_returns_false_when_nothing_moved() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            let mut runtime = Runtime::open(Some(dir.path().to_path_buf())).unwrap();
            let watcher = ConfigWatcher::from_runtime(&runtime);
            assert!(!reload_if_changed(&mut runtime, &watcher));
            assert!(runtime.config.layers.l7_sandbox);
        });
    }

    #[test]
    fn reload_if_changed_swaps_runtime_config_on_project_edit() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            let mut runtime = Runtime::open(Some(dir.path().to_path_buf())).unwrap();
            let watcher = ConfigWatcher::from_runtime(&runtime);
            assert!(runtime.config.layers.l7_sandbox);

            // Edit the project config file; next reload must flip it.
            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            assert!(reload_if_changed(&mut runtime, &watcher));
            assert!(!runtime.config.layers.l7_sandbox);

            // A second call without further edits must no-op.
            assert!(!reload_if_changed(&mut runtime, &watcher));
        });
    }

    #[test]
    fn reload_if_changed_preserves_config_on_malformed_toml() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "[layers]\nl7_sandbox = true\n");
            let mut runtime = Runtime::open(Some(dir.path().to_path_buf())).unwrap();
            let watcher = ConfigWatcher::from_runtime(&runtime);

            // Write garbage. The watcher must log+swallow the error so
            // the runtime never sees a half-parsed config.
            std::thread::sleep(Duration::from_millis(1100));
            let path = dir.path().join(".crux").join("config.toml");
            fs::write(&path, "not = = valid toml").unwrap();
            assert!(!reload_if_changed(&mut runtime, &watcher));
            assert!(
                runtime.config.layers.l7_sandbox,
                "broken edit must not wipe live config"
            );

            // Once the user fixes the file, a subsequent call recovers.
            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            assert!(reload_if_changed(&mut runtime, &watcher));
            assert!(!runtime.config.layers.l7_sandbox);
        });
    }

    #[test]
    fn run_loop_handles_ping_and_reaches_eof() {
        // Smoke test that the extracted run_loop drives requests end
        // to end — it must reply to `ping`, terminate on stdin EOF,
        // and leave the provided writer with exactly one response
        // line.
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "");
            let runtime = Runtime::open(Some(dir.path().to_path_buf())).unwrap();
            let input = Cursor::new(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec(),
            );
            let mut output: Vec<u8> = Vec::new();
            run_loop(runtime, input, &mut output, None).unwrap();
            let s = String::from_utf8(output).unwrap();
            let lines: Vec<&str> = s.trim_end().split('\n').collect();
            assert_eq!(lines.len(), 1, "expected exactly one response line");
            let resp: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
            assert_eq!(resp["id"], 1);
            assert!(resp["result"].is_object(), "ping must return a result");
        });
    }
}
