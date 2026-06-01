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

pub fn serve_stdio(runtime: Runtime) -> std::io::Result<()> {
    let stdin = std::io::stdin().lock();
    let reader = BufReader::new(stdin);
    let stdout = std::io::stdout().lock();
    let watcher = ConfigWatcher::from_runtime(&runtime);
    run_loop(runtime, reader, stdout, Some(&watcher))
}

pub fn serve_tcp(runtime: Runtime, addr: std::net::SocketAddr) -> std::io::Result<()> {
    let listener = std::net::TcpListener::bind(addr)?;
    info!("MCP server listening on {addr}");
    serve_tcp_on_listener(runtime, listener)
}

pub fn serve_tcp_on_listener(
    runtime: Runtime,
    listener: std::net::TcpListener,
) -> std::io::Result<()> {
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();
    if ctrlc::set_handler(move || {
        info!("shutdown signal received, stopping accept loop");
        r.store(false, std::sync::atomic::Ordering::SeqCst);
    })
    .is_err()
    {
        warn!("could not set Ctrl+C handler (non-interactive mode?)");
    }
    listener.set_nonblocking(true)?;

    while running.load(std::sync::atomic::Ordering::SeqCst) {
        let stream = match listener.accept() {
            Ok((s, _)) => Some(s),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(200));
                None
            }
            Err(e) => {
                warn!("accept error: {e}");
                std::thread::sleep(std::time::Duration::from_millis(1000));
                None
            }
        };
        let Some(stream) = stream else {
            continue;
        };
        let config = runtime.config.clone();
        let db_path = runtime.db_path.clone();
        let project_root = runtime.project_root.clone();
        let global_config_path = runtime.global_config_path.clone();
        let project_config_path = runtime.project_config_path.clone();

        std::thread::spawn(move || {
            let conn = match crux_core::db::open(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("failed to open db for connection: {e}");
                    return;
                }
            };
            let rt = Runtime {
                config,
                conn,
                db_path,
                project_root,
                global_config_path,
                project_config_path,
            };
            let stream_clone = match stream.try_clone() {
                Ok(s) => s,
                Err(e) => {
                    warn!("failed to clone stream: {e}");
                    return;
                }
            };
            let reader = BufReader::new(stream_clone);
            let writer = stream;
            let watcher = ConfigWatcher::from_runtime(&rt);
            let _ = run_loop(rt, reader, writer, Some(&watcher));
        });
    }
    Ok(())
}

pub(crate) fn run_loop<R: BufRead, W: Write>(
    mut runtime: Runtime,
    mut reader: R,
    mut writer: W,
    watcher: Option<&ConfigWatcher>,
) -> std::io::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<Response>();
    let mut line = String::new();
    loop {
        if let Some(w) = watcher {
            reload_if_changed(&mut runtime, w);
        }

        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        if line.len() > 1_048_576 {
            warn!("request line too large ({} bytes), dropping", line.len());
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match parse_request(trimmed) {
            Ok(ParsedRequest { id, method, params }) if method == "tools/call" => {
                let tx = tx.clone();
                let db_path = runtime.db_path.clone();
                let config = runtime.config.clone();
                let project_root = runtime.project_root.clone();
                let global_config_path = runtime.global_config_path.clone();
                let project_config_path = runtime.project_config_path.clone();
                let id2 = id.clone();

                std::thread::spawn(move || {
                    let conn = match crux_core::db::open(&db_path) {
                        Ok(c) => c,
                        Err(e) => {
                            let resp = Response::err(
                                id2.unwrap_or(Value::Null),
                                RpcError::internal(e.to_string()),
                            );
                            let _ = tx.send(resp);
                            return;
                        }
                    };
                    let app_ctx = dispatch::AppContext {
                        conn,
                        config,
                        project_root,
                        global_config_path,
                        project_config_path,
                    };
                    let params: CallToolParams = match serde_json::from_value(params) {
                        Ok(p) => p,
                        Err(e) => {
                            let resp = Response::err(
                                id2.unwrap_or(Value::Null),
                                RpcError::invalid_params(e.to_string()),
                            );
                            let _ = tx.send(resp);
                            return;
                        }
                    };
                    let result = dispatch::call(&app_ctx, &params.name, &params.arguments);
                    let resp = match id {
                        Some(id) => {
                            Response::ok(id, serde_json::to_value(result).unwrap_or_default())
                        }
                        None => return,
                    };
                    let _ = tx.send(resp);
                });
            }
            Ok(ParsedRequest { id, method, .. }) if method == "tools/list" => {
                if let Some(id) = id {
                    let resp = Response::ok(
                        id,
                        serde_json::to_value(ListToolsResult {
                            tools: tools::all_tools(),
                        })
                        .unwrap(),
                    );
                    write_response(&mut writer, &resp)?;
                }
            }
            Ok(ParsedRequest { id, .. }) if id.is_some() => {
                let response = handle_line(&runtime, trimmed);
                if let Some(resp) = response {
                    write_response(&mut writer, &resp)?;
                }
            }
            Ok(_) => {} // notification, no response
            Err(resp) => {
                write_response(&mut writer, &resp)?;
            }
        }

        while let Ok(resp) = rx.try_recv() {
            write_response(&mut writer, &resp)?;
        }
    }
}

fn write_response<W: Write>(writer: &mut W, resp: &Response) -> std::io::Result<()> {
    let s = serde_json::to_string(resp)
        .unwrap_or_else(|_| "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32603,\"message\":\"serialize\"}}".into());
    writeln!(writer, "{s}")?;
    writer.flush()
}

struct ParsedRequest {
    id: Option<Value>,
    method: String,
    params: Value,
}

#[allow(clippy::result_large_err)]
fn parse_request(line: &str) -> Result<ParsedRequest, Response> {
    let req: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            warn!(?e, "could not parse request");
            return Err(Response::err(
                Value::Null,
                RpcError::parse_error(format!("invalid JSON: {e}")),
            ));
        }
    };

    if req.jsonrpc != "2.0" {
        return Err(Response::err(
            req.id.clone().unwrap_or(Value::Null),
            RpcError::invalid_request(format!("expected jsonrpc=2.0, got {}", req.jsonrpc)),
        ));
    }

    Ok(ParsedRequest {
        id: req.id,
        method: req.method,
        params: req.params,
    })
}

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
        "initialized" | "notifications/initialized" => Ok(Value::Null),
        "tools/list" => Ok(serde_json::to_value(ListToolsResult {
            tools: tools::all_tools(),
        })
        .map_err(|e| RpcError::internal(e.to_string()))?),
        "tools/call" => {
            let params: CallToolParams = serde_json::from_value(req.params.clone())
                .map_err(|e| RpcError::invalid_params(format!("invalid tools/call params: {e}")))?;
            let conn = crux_core::db::open(&runtime.db_path)
                .map_err(|e| RpcError::internal(format!("open db: {e}")))?;
            let app_ctx = dispatch::AppContext {
                conn,
                config: runtime.config.clone(),
                project_root: runtime.project_root.clone(),
                global_config_path: runtime.global_config_path.clone(),
                project_config_path: runtime.project_config_path.clone(),
            };
            let result: CallToolResult = dispatch::call(&app_ctx, &params.name, &params.arguments);
            Ok(serde_json::to_value(result).map_err(|e| RpcError::internal(e.to_string()))?)
        }
        "ping" => Ok(json!({})),
        other => Err(RpcError::method_not_found(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{BufReader, Cursor, Write};
    use std::net::TcpStream;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;

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

            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            assert!(reload_if_changed(&mut runtime, &watcher));
            assert!(!runtime.config.layers.l7_sandbox);

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

            std::thread::sleep(Duration::from_millis(1100));
            let path = dir.path().join(".crux").join("config.toml");
            fs::write(&path, "not = = valid toml").unwrap();
            assert!(!reload_if_changed(&mut runtime, &watcher));
            assert!(
                runtime.config.layers.l7_sandbox,
                "broken edit must not wipe live config"
            );

            std::thread::sleep(Duration::from_millis(1100));
            write_project_config(dir.path(), "[layers]\nl7_sandbox = false\n");
            assert!(reload_if_changed(&mut runtime, &watcher));
            assert!(!runtime.config.layers.l7_sandbox);
        });
    }

    #[test]
    fn run_loop_handles_ping_and_reaches_eof() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "");
            let runtime = Runtime::open(Some(dir.path().to_path_buf())).unwrap();
            let input =
                Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec());
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

    #[test]
    fn serve_tcp_handles_ping() {
        with_crux_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            write_project_config(dir.path(), "");
            let runtime = Runtime::open(Some(dir.path().to_path_buf())).unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();

            std::thread::spawn(move || {
                let _ = serve_tcp_on_listener(runtime, listener);
            });

            let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
            stream
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
                .unwrap();
            stream.flush().unwrap();

            let mut buf = String::new();
            let mut reader = BufReader::new(&mut stream);
            reader.read_line(&mut buf).unwrap();
            let resp: serde_json::Value = serde_json::from_str(buf.trim()).unwrap();
            assert_eq!(resp["id"], 1);
            assert!(resp["result"].is_object(), "tcp ping must return a result");
        });
    }
}
