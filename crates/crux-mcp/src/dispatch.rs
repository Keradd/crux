//! Tool dispatch — translate a `tools/call` request into the underlying
//! CRUX layer API, then format the result as a [`CallToolResult`].
//!
//! Every dispatcher is sync because all current layers are sync. When we
//! add Layer 5/6 (tree-sitter + embeddings) this module will grow async
//! variants.

use std::path::PathBuf;
use std::str::FromStr;

use serde_json::{json, Value};

use crux_core::{telemetry, tokens, Runtime};
use crux_l3_bash::FilterEngine;
use crux_l4_readcache::{CacheDecision, CheckOptions, ContextIgnore, ReadCacheManager, ReadEvent};
use crux_l5_ast::{GraphNode, GraphStore, NodeKind};
use crux_l6_search::{build_embedder, ContentType, SearchEngine, SearchOptions};
use crux_l7_sandbox::{ExecRequest, Executor, IsolationLevel, RuntimeKind};
use crux_l8_memory::{MemoryEngine, NewObservation, ObservationKind, RecallQuery};

use crate::protocol::CallToolResult;

/// Apply a `tools/call` request. `arguments` is the raw JSON object
/// supplied by the agent. Errors mapped to `CallToolResult::error` so
/// the MCP client always receives a structured response (we reserve
/// JSON-RPC errors for protocol-level failures).
pub fn call(runtime: &Runtime, name: &str, arguments: &Value) -> CallToolResult {
    let result = match name {
        "crux_remember" => remember(runtime, arguments),
        "crux_recall" => recall(runtime, arguments),
        "crux_read" => read(runtime, arguments),
        "crux_bash_filter" => bash_filter(runtime, arguments),
        "crux_audit" => audit(runtime),
        "crux_find_symbol" => find_symbol(runtime, arguments),
        "crux_get_symbol_source" => get_symbol_source(runtime, arguments),
        "crux_query_graph" => query_graph(runtime, arguments),
        "crux_impact" => impact(runtime, arguments),
        "crux_search" => search(runtime, arguments),
        "crux_execute" => execute(runtime, arguments),
        _ => Err(format!("unknown tool: {name}")),
    };
    match result {
        Ok(text) => CallToolResult::text(text),
        Err(msg) => CallToolResult::error(msg),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// crux_remember
// ─────────────────────────────────────────────────────────────────────────

fn remember(runtime: &Runtime, args: &Value) -> Result<String, String> {
    let project = project_root(runtime).ok_or_else(|| {
        "no project context — run `crux init` first or set CRUX_PROJECT".to_string()
    })?;

    let kind_s = args
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'kind'".to_string())?;
    let kind = ObservationKind::from_str(kind_s)?;
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'title'".to_string())?
        .to_string();
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'content'".to_string())?
        .to_string();

    let importance = args.get("importance").and_then(|v| v.as_u64()).unwrap_or(5) as u8;
    let tags: Vec<String> = args
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let obs = NewObservation {
        project_root: project,
        session_id: None,
        agent_id: None,
        kind,
        title,
        content,
        why: args.get("why").and_then(|v| v.as_str()).map(String::from),
        how_to_apply: args
            .get("how_to_apply")
            .and_then(|v| v.as_str())
            .map(String::from),
        symbol: args
            .get("symbol")
            .and_then(|v| v.as_str())
            .map(String::from),
        file_path: args
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from),
        tags,
        importance,
        private: args
            .get("private")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };

    let mem = MemoryEngine::new(&runtime.conn).map_err(|e| e.to_string())?;
    let id = mem.remember(obs).map_err(|e| e.to_string())?;
    Ok(format!("remembered #{id} ({})", kind_s))
}

// ─────────────────────────────────────────────────────────────────────────
// crux_recall
// ─────────────────────────────────────────────────────────────────────────

fn recall(runtime: &Runtime, args: &Value) -> Result<String, String> {
    let project = project_root(runtime)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let kinds: Vec<ObservationKind> = args
        .get("kinds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .filter_map(|s| ObservationKind::from_str(s).ok())
                .collect()
        })
        .unwrap_or_default();
    let symbol = args
        .get("symbol")
        .and_then(|v| v.as_str())
        .map(String::from);
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let include_archived = args
        .get("include_archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let q = RecallQuery {
        query,
        project_root: Some(project),
        kinds,
        symbol,
        limit,
        include_archived,
    };
    let mem = MemoryEngine::new(&runtime.conn).map_err(|e| e.to_string())?;
    let results = mem.recall(&q).map_err(|e| e.to_string())?;

    if results.is_empty() {
        return Ok("(no observations found)".into());
    }
    let mut out = String::new();
    for r in &results {
        let o = &r.observation;
        out.push_str(&format!(
            "#{} [{}] importance={} score={:.2}\n  title: {}\n  content: {}\n",
            o.id,
            o.kind.as_str(),
            o.importance,
            r.score,
            o.title,
            first_line(&o.content),
        ));
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────
// crux_read
// ─────────────────────────────────────────────────────────────────────────

fn read(runtime: &Runtime, args: &Value) -> Result<String, String> {
    let project = project_root_path(runtime)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'file_path'".to_string())?;
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let crux_home = crux_core::paths::crux_home().ok();
    let ci = ContextIgnore::load(&project, crux_home.as_deref());
    let opts = CheckOptions {
        contextignore: Some(ci),
        delta_max_bytes: Some(runtime.config.layer.l4.delta_max_bytes),
    };
    let mgr = ReadCacheManager::new(&runtime.conn);
    let path_buf = PathBuf::from(file_path);
    let decision = mgr
        .check_with(
            &ReadEvent {
                agent_id,
                session_id,
                project_root: &project,
                file_path: &path_buf,
                offset: 0,
                limit: 0,
            },
            &opts,
        )
        .map_err(|e| e.to_string())?;

    match decision {
        CacheDecision::Allow => {
            // Cache miss/fresh — actually read the file and return it.
            let content =
                std::fs::read_to_string(&path_buf).map_err(|e| format!("read failed: {e}"))?;
            Ok(content)
        }
        CacheDecision::Redundant { digest, read_count } => Ok(format!(
            "[crux] file already in context (read #{read_count}). digest:\n{digest}"
        )),
        CacheDecision::Delta {
            summary,
            body,
            read_count,
        } => Ok(format!(
            "[crux] file changed since read #{prev} — diff {summary}\n\n{body}",
            prev = read_count - 1,
        )),
        CacheDecision::Blocked { reason } => Err(format!("blocked: {reason}")),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// crux_bash_filter
// ─────────────────────────────────────────────────────────────────────────

fn bash_filter(runtime: &Runtime, args: &Value) -> Result<String, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'command'".to_string())?;
    let output = args
        .get("output")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'output'".to_string())?;
    let engine = FilterEngine::builtin().map_err(|e| e.to_string())?;
    let result = engine.process(command, output);
    let project = project_root(runtime);
    let original_tokens = tokens::estimate(output) as i64;
    let compressed_tokens = tokens::estimate(&result.output.text) as i64;
    let _ = telemetry::record(
        &runtime.conn,
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

// ─────────────────────────────────────────────────────────────────────────
// crux_audit
// ─────────────────────────────────────────────────────────────────────────

fn audit(runtime: &Runtime) -> Result<String, String> {
    let project = project_root(runtime);
    let stats =
        telemetry::stats_by_layer(&runtime.conn, project.as_deref()).map_err(|e| e.to_string())?;

    let payload = json!({
        "project": project,
        "layers": {
            "l1_output": runtime.config.layers.l1_output,
            "l2_mcp_shrink": runtime.config.layers.l2_mcp_shrink,
            "l3_bash_filter": runtime.config.layers.l3_bash_filter,
            "l4_read_cache": runtime.config.layers.l4_read_cache,
            "l5_ast_graph": runtime.config.layers.l5_ast_graph,
            "l6_hybrid_search": runtime.config.layers.l6_hybrid_search,
            "l7_sandbox": runtime.config.layers.l7_sandbox,
            "l8_memory": runtime.config.layers.l8_memory,
            "l9_coach": runtime.config.layers.l9_coach,
            "l10_setup": runtime.config.layers.l10_setup,
        },
        "telemetry": stats.iter().map(|s| json!({
            "layer": s.layer,
            "events": s.events,
            "original_tokens": s.original_tokens,
            "compressed_tokens": s.compressed_tokens,
            "savings": s.savings,
        })).collect::<Vec<_>>(),
    });
    Ok(serde_json::to_string_pretty(&payload).unwrap())
}

// ─────────────────────────────────────────────────────────────────────────
// L5 — crux_find_symbol / crux_get_symbol_source / crux_query_graph / crux_impact
// ─────────────────────────────────────────────────────────────────────────

fn find_symbol(runtime: &Runtime, args: &Value) -> Result<String, String> {
    let project = project_root(runtime)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'name'".to_string())?;
    let kind: Option<NodeKind> = args
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.parse::<NodeKind>())
        .transpose()
        .map_err(|e: String| e)?;
    let exact = args.get("exact").and_then(|v| v.as_bool()).unwrap_or(false);
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(30) as usize;

    let store = GraphStore::new(&runtime.conn);
    let nodes = if exact {
        store
            .find_symbol(&project, name, kind)
            .map_err(|e| e.to_string())?
    } else {
        let mut rows = store
            .find_symbol_like(&project, name, limit)
            .map_err(|e| e.to_string())?;
        if let Some(k) = kind {
            rows.retain(|n| n.kind.as_str() == k.as_str());
        }
        rows
    };

    Ok(serde_json::to_string_pretty(&serialize_nodes(&nodes)).unwrap())
}

fn get_symbol_source(runtime: &Runtime, args: &Value) -> Result<String, String> {
    let project_path = project_root_path(runtime)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;
    let project = project_path.display().to_string();
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'qualified_name'".to_string())?;
    let include_metadata = args
        .get("include_metadata")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let store = GraphStore::new(&runtime.conn);
    let n = store
        .get_by_qn(&project, qn)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("symbol '{}' not found", qn))?;

    let abs = project_path.join(&n.file_path);
    let content =
        std::fs::read_to_string(&abs).map_err(|e| format!("read {}: {}", abs.display(), e))?;
    let lines: Vec<&str> = content.lines().collect();
    let lo = (n.line_start.saturating_sub(1)) as usize;
    let hi = (n.line_end as usize).min(lines.len());
    let mut out = String::new();
    if include_metadata {
        out.push_str(&format!(
            "{} {}\n  file: {}\n  lines: {}-{}\n",
            n.kind.as_str(),
            n.qualified_name,
            n.file_path,
            n.line_start,
            n.line_end,
        ));
        if let Some(sig) = &n.signature {
            out.push_str(&format!(
                "  signature: {}\n",
                sig.lines().next().unwrap_or("")
            ));
        }
        out.push('\n');
    }
    if lo < hi {
        for (i, line) in lines[lo..hi].iter().enumerate() {
            out.push_str(&format!("{:>5}  {}\n", lo + i + 1, line));
        }
    }
    Ok(out)
}

fn query_graph(runtime: &Runtime, args: &Value) -> Result<String, String> {
    let project = project_root(runtime)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'qualified_name'".to_string())?;
    let direction = args
        .get("direction")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'direction'".to_string())?;

    let store = GraphStore::new(&runtime.conn);
    let nodes = match direction {
        "callers" => store.callers_of(&project, qn).map_err(|e| e.to_string())?,
        "callees" => store.callees_of(&project, qn).map_err(|e| e.to_string())?,
        other => {
            return Err(format!(
                "unknown direction '{other}' (want callers|callees)"
            ))
        }
    };
    Ok(serde_json::to_string_pretty(&serialize_nodes(&nodes)).unwrap())
}

fn impact(runtime: &Runtime, args: &Value) -> Result<String, String> {
    let project = project_root(runtime)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'qualified_name'".to_string())?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as u32;
    let max = args.get("max").and_then(|v| v.as_u64()).unwrap_or(100) as u32;

    let store = GraphStore::new(&runtime.conn);
    let nodes = store
        .impact_radius(&project, qn, depth, max)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_string_pretty(&serialize_nodes(&nodes)).unwrap())
}

// ─────────────────────────────────────────────────────────────────────────
// L6 — crux_search
// ─────────────────────────────────────────────────────────────────────────

fn search(runtime: &Runtime, args: &Value) -> Result<String, String> {
    let project = project_root(runtime)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'query'".to_string())?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let kinds: Vec<ContentType> = args
        .get("kinds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .filter_map(ContentType::parse)
                .collect()
        })
        .unwrap_or_default();

    let opts = SearchOptions { limit, kinds };
    let embedder = build_embedder(&runtime.config.layer.l6).map_err(|e| e.to_string())?;
    let engine = SearchEngine::new(&runtime.conn, embedder.as_ref());
    let hits = engine
        .hybrid_search(&project, query, &opts)
        .map_err(|e| e.to_string())?;

    let payload: Value = hits
        .iter()
        .map(|h| {
            json!({
                "score": h.score,
                "snippet": h.snippet,
                "ranks": {
                    "porter":  h.bm25_porter_rank,
                    "trigram": h.bm25_trigram_rank,
                    "vector":  h.vector_rank,
                },
                "chunk": {
                    "id":            h.chunk.id,
                    "content_type":  h.chunk.content_type.as_str(),
                    "title":         h.chunk.title,
                    "file_path":     h.chunk.file_path,
                    "language":      h.chunk.language,
                    "line_start":    h.chunk.line_start,
                    "line_end":      h.chunk.line_end,
                    "tokens_est":    h.chunk.tokens_est,
                    "source_id":     h.chunk.source_id,
                }
            })
        })
        .collect::<Vec<Value>>()
        .into();
    Ok(serde_json::to_string_pretty(&payload).unwrap())
}

// ─────────────────────────────────────────────────────────────────────────
// L7 — crux_execute
// ─────────────────────────────────────────────────────────────────────────

fn execute(runtime: &Runtime, args: &Value) -> Result<String, String> {
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

    let req = ExecRequest {
        runtime: runtime_kind,
        code,
        project_root: project_root_path(runtime),
        timeout: std::time::Duration::from_secs(timeout_seconds),
        max_output_bytes,
        env: std::collections::HashMap::new(),
        inherit_env,
        isolation,
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

fn serialize_nodes(nodes: &[GraphNode]) -> Value {
    Value::Array(
        nodes
            .iter()
            .map(|n| {
                json!({
                    "kind": n.kind.as_str(),
                    "name": n.name,
                    "qualified_name": n.qualified_name,
                    "file_path": n.file_path,
                    "line_start": n.line_start,
                    "line_end": n.line_end,
                    "signature": n.signature,
                    "is_test": n.is_test,
                })
            })
            .collect(),
    )
}

// ─────────────────────────────────────────────────────────────────────────
// helpers
// ─────────────────────────────────────────────────────────────────────────

fn project_root(runtime: &Runtime) -> Option<String> {
    runtime
        .project_root
        .as_ref()
        .map(|p| p.display().to_string())
}

fn project_root_path(runtime: &Runtime) -> Option<PathBuf> {
    runtime.project_root.clone()
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crux_core::config::Config;
    use crux_l5_ast::{
        ConfidenceTier, EdgeKind, GraphStore, NodeKind, ParseResult, ParsedEdge, ParsedNode,
    };
    use std::path::PathBuf;

    fn make_runtime(project: PathBuf) -> Runtime {
        let conn = crux_core::db::open_in_memory().unwrap();
        Runtime {
            config: Config::default(),
            conn,
            project_root: Some(project),
            global_config_path: PathBuf::from("/tmp/crux-test/global.toml"),
            project_config_path: None,
        }
    }

    fn seed_graph(runtime: &Runtime, project: &str) {
        let store = GraphStore::new(&runtime.conn);
        let result = ParseResult {
            nodes: vec![
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "compute_delta".to_string(),
                    qualified_name: "demo::delta::compute_delta".to_string(),
                    line_start: 1,
                    line_end: 1,
                    parent_qn: Some("demo::delta".to_string()),
                    signature: Some("pub fn compute_delta()".to_string()),
                    is_test: false,
                },
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "caller".to_string(),
                    qualified_name: "demo::main::caller".to_string(),
                    line_start: 1,
                    line_end: 1,
                    parent_qn: Some("demo::main".to_string()),
                    signature: Some("pub fn caller()".to_string()),
                    is_test: false,
                },
            ],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                source_qn: "demo::main::caller".to_string(),
                target_qn: "compute_delta".to_string(),
                line: 1,
                confidence: 0.6,
                tier: ConfidenceTier::Inferred,
            }],
        };
        store
            .write(project, "src/lib.rs", "rust", "deadbeef", &result)
            .unwrap();
    }

    #[test]
    fn find_symbol_dispatcher_returns_matches() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        seed_graph(&runtime, &project.display().to_string());
        let out = find_symbol(&runtime, &json!({"name": "compute_delta", "exact": true})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "compute_delta");
    }

    #[test]
    fn query_graph_dispatcher_callers_resolve_via_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        seed_graph(&runtime, &project.display().to_string());
        let out = query_graph(
            &runtime,
            &json!({
                "qualified_name": "demo::delta::compute_delta",
                "direction": "callers"
            }),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["qualified_name"], "demo::main::caller");
    }

    #[test]
    fn impact_dispatcher_walks_callers() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        seed_graph(&runtime, &project.display().to_string());
        let out = impact(
            &runtime,
            &json!({
                "qualified_name": "demo::delta::compute_delta",
                "depth": 3,
                "max": 50
            }),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert!(arr.iter().any(|n| n["name"] == "caller"));
    }

    #[test]
    fn query_graph_rejects_bad_direction() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project);
        let err = query_graph(
            &runtime,
            &json!({"qualified_name": "x", "direction": "siblings"}),
        )
        .unwrap_err();
        assert!(err.contains("unknown direction"));
    }

    #[test]
    fn execute_dispatcher_runs_bash_echo() {
        if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime(dir.path().to_path_buf());
        let out = execute(
            &runtime,
            &json!({"runtime": "bash", "code": "echo from-mcp"}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["exit_code"], 0);
        assert!(v["stdout"].as_str().unwrap().contains("from-mcp"));
        assert_eq!(v["timed_out"], false);
    }

    #[test]
    fn execute_dispatcher_rejects_unknown_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime(dir.path().to_path_buf());
        let err = execute(&runtime, &json!({"runtime": "ruby", "code": "puts 1"})).unwrap_err();
        assert!(err.contains("unknown runtime"));
    }

    #[test]
    fn execute_dispatcher_rejects_empty_code() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime(dir.path().to_path_buf());
        let err = execute(&runtime, &json!({"runtime": "bash", "code": "   "})).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn execute_dispatcher_honors_hard_isolation() {
        if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime(dir.path().to_path_buf());
        let out = execute(
            &runtime,
            &json!({
                "runtime": "bash",
                "code": "echo ok",
                "isolation": "hard",
            }),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["exit_code"], 0);
        let applied: Vec<String> = v["isolation_applied"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        assert!(applied.contains(&"rlimits".to_string()));
    }

    #[test]
    fn search_dispatcher_returns_indexed_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());

        // Seed a code chunk via the L6 indexer using the same embedder
        // backend that `build_embedder` will pick from the default
        // config (provider="hash", dim=256).
        let embedder = crux_l6_search::build_embedder(&runtime.config.layer.l6).unwrap();
        let indexer = crux_l6_search::Indexer::new(&runtime.conn);
        let chunk = crux_l6_search::Chunk {
            project_root: project.display().to_string(),
            source_id: None,
            file_path: "src/lib.rs".into(),
            language: Some("rust".into()),
            content_type: crux_l6_search::ContentType::Code,
            title: Some("compute_delta".into()),
            content: "compute delta over old and new strings".into(),
            line_start: 1,
            line_end: 5,
        };
        indexer.index_chunks(&[chunk], embedder.as_ref()).unwrap();

        let out = search(&runtime, &json!({"query": "compute delta", "limit": 5})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert!(!arr.is_empty(), "expected at least one hit");
        assert_eq!(arr[0]["chunk"]["title"], "compute_delta");
    }

    #[test]
    fn get_symbol_source_reads_actual_file() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("src/lib.rs"),
            "pub fn compute_delta() -> i32 { 7 }\npub fn caller() -> i32 { compute_delta() }\n",
        )
        .unwrap();
        let runtime = make_runtime(project.clone());
        seed_graph(&runtime, &project.display().to_string());
        let out = get_symbol_source(
            &runtime,
            &json!({"qualified_name": "demo::delta::compute_delta", "include_metadata": true}),
        )
        .unwrap();
        assert!(out.contains("pub fn compute_delta"));
        assert!(out.contains("file: src/lib.rs"));
    }
}
