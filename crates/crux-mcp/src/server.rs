//! Stdio-based MCP server.
//!
//! Reads one JSON-RPC envelope per line of stdin, dispatches it, writes
//! the response (also one line) to stdout. Nothing else gets written to
//! stdout — logs go to stderr through `tracing`.
//!
//! `serve_stdio` blocks until stdin EOF or an unrecoverable IO error.

use std::io::{BufRead, BufReader, Write};

use serde_json::{json, Value};
use tracing::{debug, warn};

use crux_core::Runtime;

use crate::dispatch;
use crate::protocol::{
    self, CallToolParams, CallToolResult, InitializeResult, ListToolsResult, Request, Response,
    RpcError, ServerCapabilities, ServerInfo, ToolsCapability,
};
use crate::tools;

pub fn serve_stdio(runtime: Runtime) -> std::io::Result<()> {
    let stdin = std::io::stdin().lock();
    let mut reader = BufReader::new(stdin);
    let mut stdout = std::io::stdout().lock();
    let mut line = String::new();

    loop {
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
            writeln!(stdout, "{s}")?;
            stdout.flush()?;
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
