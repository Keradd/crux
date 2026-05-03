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
use crux_l11_digest::DigestEngine;
use crux_l3_bash::FilterEngine;
use crux_l4_readcache::{CacheDecision, CheckOptions, ContextIgnore, ReadCacheManager, ReadEvent};
use crux_l5_ast::{GraphNode, GraphStore, NodeKind};
use crux_l6_search::{build_embedder, ContentType, SearchEngine, SearchOptions};
use crux_l7_sandbox::{ExecRequest, Executor, IsolationLevel, RuntimeKind};
use crux_l8_memory::{
    MemoryEngine, NewObservation, ObservationKind, RankedObservation, RecallQuery,
};

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
        "crux_digest" => digest(runtime, arguments),
        "crux_compact" => compact(runtime, arguments),
        _ => Err(format!("unknown tool: {name}")),
    };
    record_l11_event(runtime, name, arguments, &result);
    match result {
        Ok(text) => CallToolResult::text(text),
        Err(msg) => CallToolResult::error(msg),
    }
}

/// Best-effort L11 turn-event seed for every dispatched tool call. Lets
/// agents that drive CRUX through MCP only (no PreToolUse/PostToolUse
/// hooks — Cursor/Windsurf default config) still get conversation
/// digests. The digest tools themselves are excluded so calling
/// `crux_digest` doesn't pollute its own pending list.
fn record_l11_event(runtime: &Runtime, name: &str, args: &Value, result: &Result<String, String>) {
    if !runtime.config.layers.l11_digest {
        return;
    }
    if matches!(name, "crux_digest" | "crux_compact") {
        return;
    }
    use crux_l11_digest::{TurnEvent, TurnStatus};
    let project = match project_root(runtime) {
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
    let engine =
        crux_l11_digest::DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());
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
    // crux_execute carries the source under `code`; we only surface its
    // first line so multi-kilobyte snippets don't bloat the digest.
    if tool_name == "crux_execute" {
        if let Some(code) = args.get("code").and_then(|v| v.as_str()) {
            return Some(code.lines().next().unwrap_or(code).to_string());
        }
    }
    None
}

fn truncate_one_line(s: &str, n: usize) -> String {
    let line = s.lines().next().unwrap_or(s);
    if line.len() <= n {
        line.to_string()
    } else {
        format!("{}\u{2026}", &line[..n])
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
        file_paths: Vec::new(),
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

    // Resolve the read target. Three shapes accepted:
    //   1. `symbol = "<qualified_name>"`  — resolve file + line range via
    //      the L5 graph. Cheapest path for "I want one function".
    //   2. `file_path + offset + limit`   — line-range slice of a file.
    //      `offset` = 1-based line to start at (compat: 0 or 1 both mean
    //      top-of-file for ergonomics), `limit` = number of lines to
    //      return (0 = to end).
    //   3. `file_path` alone              — whole file (legacy behavior).
    //
    // When `symbol` is set, `file_path` / `offset` / `limit` are ignored.
    let (file_path, offset_lines, limit_lines, symbol_meta) =
        if let Some(qn) = args.get("symbol").and_then(|v| v.as_str()) {
            let project_s = project.display().to_string();
            let store = GraphStore::new(&runtime.conn);
            let n = store
                .get_by_qn(&project_s, qn)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("symbol '{}' not found", qn))?;
            let start = n.line_start.max(1) as u64;
            let end = n.line_end.max(start as u32) as u64;
            let limit = end.saturating_sub(start - 1);
            let abs = project.join(&n.file_path).display().to_string();
            let meta = format!(
                "{} {}\n  file: {}\n  lines: {}-{}\n\n",
                n.kind.as_str(),
                n.qualified_name,
                n.file_path,
                n.line_start,
                n.line_end,
            );
            (abs, start, limit, Some(meta))
        } else {
            let fp = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'file_path' (or 'symbol')".to_string())?
                .to_string();
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(0);
            (fp, offset, limit, None)
        };

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
    let path_buf = PathBuf::from(&file_path);
    let decision = mgr
        .check_with(
            &ReadEvent {
                agent_id,
                session_id,
                project_root: &project,
                file_path: &path_buf,
                offset: offset_lines,
                limit: limit_lines,
            },
            &opts,
        )
        .map_err(|e| e.to_string())?;

    let force_full = args
        .get("force_full")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut body = match decision {
        CacheDecision::Allow => {
            // Cache miss/fresh — read the file and slice to the requested
            // range. For full-file reads the slice is a no-op.
            let raw =
                std::fs::read_to_string(&path_buf).map_err(|e| format!("read failed: {e}"))?;

            // Outline-first auto-mode (L4+L5 fusion). Fires only when the
            // caller asked for the *whole* file (no offset/limit/symbol)
            // and didn't pass `force_full = true`. We pull the symbol
            // list from L5 and return that instead of the body — agent
            // can drill in via `crux_read --symbol=<qn>` afterwards.
            let outline_threshold = runtime.config.layer.l4.outline_above_lines;
            let want_full_read =
                offset_lines == 0 && limit_lines == 0 && symbol_meta.is_none() && !force_full;
            if want_full_read && outline_threshold > 0 {
                let line_count = raw.lines().count() as u64;
                if line_count >= outline_threshold {
                    if let Some(outline) =
                        try_render_outline(runtime, &project, &file_path, line_count)
                    {
                        outline
                    } else {
                        raw.clone()
                    }
                } else {
                    let mut out = String::new();
                    if let Some(meta) = &symbol_meta {
                        out.push_str(meta);
                    }
                    out.push_str(&slice_by_lines(
                        &raw,
                        offset_lines,
                        limit_lines,
                        symbol_meta.is_some(),
                    ));
                    out
                }
            } else {
                let mut out = String::new();
                if let Some(meta) = &symbol_meta {
                    out.push_str(meta);
                }
                out.push_str(&slice_by_lines(
                    &raw,
                    offset_lines,
                    limit_lines,
                    symbol_meta.is_some(),
                ));
                out
            }
        }
        CacheDecision::Redundant { digest, read_count } => {
            format!("[crux] file already in context (read #{read_count}). digest:\n{digest}")
        }
        CacheDecision::Delta {
            summary,
            body,
            read_count,
        } => format!(
            "[crux] file changed since read #{prev} — diff {summary}\n\n{body}",
            prev = read_count - 1,
        ),
        CacheDecision::Blocked { reason } => return Err(format!("blocked: {reason}")),
    };

    if let Some(footer) = memory_footer_for_file(runtime, &project, &file_path) {
        body.push_str(&footer);
    }
    Ok(body)
}

/// Hard cap on outline rows. Anything past this is collapsed into a
/// single "… and N more" hint so generated files (50k-line bundles,
/// minified vendored code) don't balloon the response.
const OUTLINE_MAX_ROWS: usize = 200;

/// L4+L5 outline-first auto-mode renderer. Returns `None` when the
/// L5 graph has no symbols indexed for `file_path`, signalling the
/// dispatcher to fall back to the full body. The returned string is a
/// compact symbol list with a sticky header pointing the agent at
/// `--symbol` / `--offset` / `--force_full` knobs for follow-up reads.
fn try_render_outline(
    runtime: &Runtime,
    project: &std::path::Path,
    file_path: &str,
    line_count: u64,
) -> Option<String> {
    let project_s = project.display().to_string();
    let store = GraphStore::new(&runtime.conn);
    let variants = file_path_variants(project, file_path);
    // Pick the file_path variant the GraphStore actually persisted under.
    // Real-world callers may pass abs while indexing wrote rel (or
    // vice-versa) — matching variant wins, others stay empty.
    let mut matched_variant: Option<&str> = None;
    let mut nodes: Vec<GraphNode> = Vec::new();
    for v in &variants {
        if let Ok(rows) = store.list_symbols_in_file(&project_s, v, OUTLINE_MAX_ROWS) {
            if !rows.is_empty() {
                nodes = rows;
                matched_variant = Some(v.as_str());
                break;
            }
        }
    }
    let matched_variant = matched_variant?;
    if nodes.is_empty() {
        return None;
    }
    let total_symbols = store
        .count_symbols_in_file(&project_s, matched_variant)
        .unwrap_or(nodes.len() as u64);
    let truncated = total_symbols as usize > nodes.len();

    let mut out = String::new();
    out.push_str(&format!(
        "[crux:l4+l5] file too large for full read ({} lines, {} symbol{}). Outline:\n",
        line_count,
        total_symbols,
        if total_symbols == 1 { "" } else { "s" },
    ));
    out.push_str("Use crux_read --symbol=<qualified_name>  or  --offset=N --limit=M  for body.\n");
    out.push_str("Override: crux_read --force_full=true\n\n");

    for n in &nodes {
        let kind = n.kind.as_str();
        let lines = if n.line_start == n.line_end {
            format!("line {}", n.line_start)
        } else {
            format!("lines {}-{}", n.line_start, n.line_end)
        };
        let sig = n
            .signature
            .as_deref()
            .map(truncate_signature)
            .unwrap_or_default();
        if sig.is_empty() {
            out.push_str(&format!(
                "  {:<9} {:<48} {}\n",
                kind, n.qualified_name, lines
            ));
        } else {
            out.push_str(&format!(
                "  {:<9} {:<48} {:<16} {}\n",
                kind, n.qualified_name, lines, sig,
            ));
        }
    }
    if truncated {
        let extra = total_symbols.saturating_sub(nodes.len() as u64);
        out.push_str(&format!(
            "  ... and {} more (use --offset/--limit to scan)\n",
            extra,
        ));
    }
    Some(out)
}

/// Trim a signature line for outline display: collapse whitespace and
/// cap at 80 chars with an ellipsis. Keeps the visible row tidy without
/// hiding the symbol's calling shape.
fn truncate_signature(sig: &str) -> String {
    let collapsed: String = sig.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= 80 {
        collapsed
    } else {
        format!("{}…", &collapsed[..79])
    }
}

/// Slice `raw` to a 1-based line range. `offset == 0` means top-of-file
/// (1 is treated identically so 0/1 ergonomics both work). `limit == 0`
/// means to end-of-file. When `annotate_lines` is true the slice is
/// prefixed with 1-based gutter line numbers — matching
/// `get_symbol_source`'s style so agents can jump back to the source.
fn slice_by_lines(raw: &str, offset: u64, limit: u64, annotate_lines: bool) -> String {
    if offset == 0 && limit == 0 {
        return raw.to_string();
    }
    let lines: Vec<&str> = raw.lines().collect();
    let first_1based = offset.max(1) as usize;
    let lo = first_1based.saturating_sub(1).min(lines.len());
    let hi = if limit == 0 {
        lines.len()
    } else {
        (lo + limit as usize).min(lines.len())
    };
    let mut out = String::new();
    for (i, line) in lines[lo..hi].iter().enumerate() {
        if annotate_lines {
            out.push_str(&format!("{:>5}  {}\n", lo + i + 1, line));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
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
            "l11_digest": runtime.config.layers.l11_digest,
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

    // Auto-surface: observations about this symbol OR its file.
    if let Some(footer) = memory_footer_for_symbol(runtime, &project_path, qn, &n.file_path) {
        out.push_str(&footer);
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

/// Trim factor for line-aware snippets. `view_lines = 3` means "matched
/// line plus three lines on either side" — six lines of code context
/// fits on a typical IDE screen and stays well under the previous
/// 80-char text snippet's "fits in one line of model output" budget.
const SEARCH_DEFAULT_VIEW_LINES: u64 = 3;
const SEARCH_MAX_VIEW_LINES: u64 = 20;

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

    let view = args
        .get("view")
        .and_then(|v| v.as_str())
        .and_then(SearchView::parse)
        .unwrap_or(SearchView::Default);
    let view_lines = args
        .get("view_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(SEARCH_DEFAULT_VIEW_LINES)
        .clamp(0, SEARCH_MAX_VIEW_LINES) as usize;
    let debug = args.get("debug").and_then(|v| v.as_bool()).unwrap_or(false);

    let opts = SearchOptions { limit, kinds };
    let embedder = build_embedder(&runtime.config.layer.l6).map_err(|e| e.to_string())?;
    let engine = SearchEngine::new(&runtime.conn, embedder.as_ref());
    let hits = engine
        .hybrid_search(&project, query, &opts)
        .map_err(|e| e.to_string())?;

    let payload: Value = hits
        .iter()
        .map(|h| render_hit(runtime, h, query, view, view_lines, debug))
        .collect::<Vec<Value>>()
        .into();
    Ok(serde_json::to_string_pretty(&payload).unwrap())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchView {
    /// Char-based 80-char window around the best match (legacy shape,
    /// smallest payload).
    Compact,
    /// Line-aware multi-line snippet for code, char-window for prose
    /// (default — usually saves the agent's follow-up read).
    Default,
    /// Full chunk content. Skip follow-up reads at the cost of a fatter
    /// search response — useful when an agent wants every match in
    /// context.
    Full,
}

impl SearchView {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "compact" => Self::Compact,
            "default" | "" => Self::Default,
            "full" => Self::Full,
            _ => return None,
        })
    }
}

fn render_hit(
    runtime: &Runtime,
    h: &crux_l6_search::HybridResult,
    query: &str,
    view: SearchView,
    view_lines: usize,
    debug: bool,
) -> Value {
    let chunk = &h.chunk;
    let snippet = match view {
        SearchView::Compact => h.snippet.clone(),
        SearchView::Default => match chunk.content_type {
            ContentType::Code | ContentType::Symbol => {
                line_aware_snippet(&chunk.content, query, view_lines)
            }
            _ => h.snippet.clone(),
        },
        SearchView::Full => chunk.content.clone(),
    };

    let symbol_qn = chunk
        .source_id
        .and_then(|sid| symbol_qn_for_source_id(runtime, sid));

    let score = round4(h.score);
    let mut out = json!({
        "id": chunk.id,
        "kind": chunk.content_type.as_str(),
        "file": chunk.file_path,
        "lines": format!("{}-{}", chunk.line_start, chunk.line_end),
        "title": chunk.title,
        "snippet": snippet,
        "score": score,
    });
    if let Some(qn) = symbol_qn {
        out["symbol"] = Value::String(qn);
    }
    if debug {
        out["debug"] = json!({
            "tokens_est": chunk.tokens_est,
            "language": chunk.language,
            "source_id": chunk.source_id,
            "ranks": {
                "porter":  h.bm25_porter_rank,
                "trigram": h.bm25_trigram_rank,
                "vector":  h.vector_rank,
            },
            "score_full": h.score,
        });
    }
    out
}

/// Return the best-matching multi-line slice of `content`, padded with
/// `ctx` lines on either side. Line-aware so code excerpts stay
/// syntactically meaningful.
fn line_aware_snippet(content: &str, query: &str, ctx: usize) -> String {
    let qtokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let best = if qtokens.is_empty() {
        0
    } else {
        let mut best_idx = 0usize;
        let mut best_hits = 0usize;
        for (i, l) in lines.iter().enumerate() {
            let lower = l.to_ascii_lowercase();
            let hits = qtokens
                .iter()
                .filter(|t| lower.contains(t.as_str()))
                .count();
            if hits > best_hits {
                best_hits = hits;
                best_idx = i;
            }
        }
        best_idx
    };
    let lo = best.saturating_sub(ctx);
    let hi = (best + ctx + 1).min(lines.len());
    let mut out = String::new();
    if lo > 0 {
        out.push_str("…\n");
    }
    for (i, l) in lines[lo..hi].iter().enumerate() {
        let abs = lo + i;
        if abs == best {
            out.push_str("> ");
        } else {
            out.push_str("  ");
        }
        out.push_str(l);
        out.push('\n');
    }
    if hi < lines.len() {
        out.push_str("…\n");
    }
    out
}

fn symbol_qn_for_source_id(runtime: &Runtime, source_id: i64) -> Option<String> {
    runtime
        .conn
        .query_row(
            "SELECT qualified_name FROM ast_nodes WHERE id = ?",
            [source_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

// ─────────────────────────────────────────────────────────────────────────
// L7 — crux_execute
// ─────────────────────────────────────────────────────────────────────────

fn execute(runtime: &Runtime, args: &Value) -> Result<String, String> {
    if !runtime.config.layers.l7_sandbox {
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

// ─────────────────────────────────────────────────────────────────────────
// L11 — crux_digest / crux_compact
// ─────────────────────────────────────────────────────────────────────────

fn digest(runtime: &Runtime, args: &Value) -> Result<String, String> {
    if !runtime.config.layers.l11_digest {
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

    let engine = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());
    let summary = if pending_only {
        engine.summarize(&session).map_err(|e| e.to_string())?
    } else {
        engine.latest_summary(&session).map_err(|e| e.to_string())?
    };
    Ok(summary)
}

fn compact(runtime: &Runtime, args: &Value) -> Result<String, String> {
    if !runtime.config.layers.l11_digest {
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
    let engine = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());
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

// ─────────────────────────────────────────────────────────────────────────
// L8 auto-surface helpers
//
// `crux_read` and `crux_get_symbol_source` call these to append a short
// footer listing past observations attached to the file / symbol they
// return. Zero new tool calls, zero new MCP surface — pure context
// injection, gated by `[layer.l8] auto_surface = true`.
// ─────────────────────────────────────────────────────────────────────────

fn memory_footer_for_file(
    runtime: &Runtime,
    project_path: &std::path::Path,
    file_path: &str,
) -> Option<String> {
    let l8 = &runtime.config.layer.l8;
    if !l8.auto_surface || l8.auto_surface_limit == 0 {
        return None;
    }
    let project = project_path.display().to_string();
    let variants = file_path_variants(project_path, file_path);
    let borrows: Vec<&str> = variants.iter().map(|s| s.as_str()).collect();
    let mem = MemoryEngine::new(&runtime.conn).ok()?;
    let hits = mem
        .recall_by_file(&project, &borrows, l8.auto_surface_limit)
        .ok()?;
    if hits.is_empty() {
        None
    } else {
        Some(format_memory_footer(&hits))
    }
}

fn memory_footer_for_symbol(
    runtime: &Runtime,
    project_path: &std::path::Path,
    qualified_name: &str,
    file_path: &str,
) -> Option<String> {
    let l8 = &runtime.config.layer.l8;
    if !l8.auto_surface || l8.auto_surface_limit == 0 {
        return None;
    }
    let project = project_path.display().to_string();
    let mem = MemoryEngine::new(&runtime.conn).ok()?;

    // Merge symbol + file hits and dedupe by observation id, keeping the
    // highest score. This covers both "a note about this symbol" and
    // "a note about the file the symbol lives in".
    let mut sym_hits = mem
        .recall_by_symbol(&project, qualified_name, l8.auto_surface_limit)
        .ok()
        .unwrap_or_default();
    let variants = file_path_variants(project_path, file_path);
    let borrows: Vec<&str> = variants.iter().map(|s| s.as_str()).collect();
    let file_hits = mem
        .recall_by_file(&project, &borrows, l8.auto_surface_limit)
        .ok()
        .unwrap_or_default();

    for h in file_hits {
        if !sym_hits
            .iter()
            .any(|e| e.observation.id == h.observation.id)
        {
            sym_hits.push(h);
        }
    }
    if sym_hits.is_empty() {
        return None;
    }
    sym_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sym_hits.truncate(l8.auto_surface_limit);
    Some(format_memory_footer(&sym_hits))
}

/// Build the set of file_path strings an observation might have been
/// stored under: the caller-supplied form plus, when distinct, its
/// complementary form. If the caller gave an absolute path we also
/// return the project-relative form (and vice versa) so observations
/// match regardless of which shape was persisted.
fn file_path_variants(project_path: &std::path::Path, file_path: &str) -> Vec<String> {
    let mut out = vec![file_path.to_string()];
    let as_path = std::path::Path::new(file_path);
    if as_path.is_absolute() {
        if let Ok(rel) = as_path.strip_prefix(project_path) {
            let rel_s = rel.display().to_string();
            if rel_s != file_path {
                out.push(rel_s);
            }
        }
    } else {
        let abs = project_path.join(file_path).display().to_string();
        if abs != file_path {
            out.push(abs);
        }
    }
    out
}

fn format_memory_footer(hits: &[RankedObservation]) -> String {
    let mut s = format!(
        "\n\n[crux:l8] {} past observation(s) in scope:\n",
        hits.len()
    );
    for h in hits {
        let o = &h.observation;
        s.push_str(&format!(
            "  #{} [{}] imp={} {}\n",
            o.id,
            o.kind.as_str(),
            o.importance,
            first_line(&o.title),
        ));
    }
    s
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
    fn dispatch_call_records_l11_event_for_normal_tools() {
        use crux_l11_digest::DigestEngine;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        seed_graph(&runtime, &project.display().to_string());

        // A non-digest tool should record an event.
        let _ = call(
            &runtime,
            "crux_find_symbol",
            &json!({"name": "compute_delta", "exact": true, "session_id": "mcps"}),
        );
        let engine = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());
        let pending = engine.list_pending_events("mcps", 50).unwrap();
        assert_eq!(pending.len(), 1, "expected 1 recorded event");
        assert_eq!(pending[0].tool_name, "mcp__crux__crux_find_symbol");
        assert_eq!(pending[0].target.as_deref(), Some("compute_delta"));
    }

    #[test]
    fn dispatch_call_skips_l11_event_for_digest_tools() {
        use crux_l11_digest::DigestEngine;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project);
        // Force a digest call (no pending events; will return empty).
        let _ = call(&runtime, "crux_digest", &json!({"session_id": "mcps"}));
        let engine = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());
        let pending = engine.list_pending_events("mcps", 50).unwrap();
        assert_eq!(pending.len(), 0, "digest tool must not self-record");
    }

    #[test]
    fn digest_dispatcher_renders_summary() {
        use crux_l11_digest::{DigestEngine, TurnEvent};
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        let engine = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());
        let project_s = project.display().to_string();
        engine
            .record(&TurnEvent::new(
                "s1",
                project_s.clone(),
                "Read",
                Some("src/login.rs".into()),
                "Read src/login.rs",
            ))
            .unwrap();
        engine
            .record(&TurnEvent::new(
                "s1",
                project_s.clone(),
                "Read",
                Some("src/login.rs".into()),
                "Read src/login.rs",
            ))
            .unwrap();
        engine
            .record(&TurnEvent::new(
                "s1",
                project_s,
                "Bash",
                Some("cargo test".into()),
                "Bash cargo test",
            ))
            .unwrap();

        let out = digest(&runtime, &json!({"session_id": "s1"})).unwrap();
        assert!(out.contains("Files read"), "missing reads bucket: {out}");
        assert!(out.contains("src/login.rs ×2"));
        assert!(out.contains("Commands"));
        assert!(out.contains("cargo ×1"));
    }

    #[test]
    fn digest_dispatcher_disabled_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let mut runtime = make_runtime(project);
        runtime.config.layers.l11_digest = false;
        let err = digest(&runtime, &json!({"session_id": "s1"})).unwrap_err();
        assert!(err.contains("L11"));
    }

    #[test]
    fn compact_dispatcher_returns_digest_payload() {
        use crux_l11_digest::{DigestEngine, TurnEvent};
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        let engine = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());
        let project_s = project.display().to_string();
        engine
            .record(&TurnEvent::new(
                "s1",
                project_s,
                "Edit",
                Some("src/login.rs".into()),
                "Edit src/login.rs",
            ))
            .unwrap();

        let out = compact(&runtime, &json!({"session_id": "s1"})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["session_id"], "s1");
        assert_eq!(v["event_count"], 1);
        assert!(v["summary"].as_str().unwrap().contains("Files edited"));
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
    fn execute_dispatcher_rejects_when_l7_disabled() {
        // When the user explicitly turns L7 off in config, crux_execute must
        // fail fast with a message that tells them exactly how to re-enable —
        // silent pass-through would violate the opt-out semantics.
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = make_runtime(dir.path().to_path_buf());
        runtime.config.layers.l7_sandbox = false;
        let err = execute(&runtime, &json!({"runtime": "bash", "code": "echo x"})).unwrap_err();
        assert!(
            err.contains("L7 sandbox is disabled") && err.contains("l7_sandbox = true"),
            "expected helpful re-enable hint, got: {err}"
        );
    }

    #[test]
    fn execute_dispatcher_runs_by_default_without_explicit_config() {
        // Regression: the default Config must have l7_sandbox on, so an
        // agent can call `crux_execute` immediately after `crux init`
        // without editing config.toml.
        if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime(dir.path().to_path_buf());
        assert!(runtime.config.layers.l7_sandbox, "default must be on");
        let out = execute(
            &runtime,
            &json!({"runtime": "bash", "code": "echo default-on-works"}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["exit_code"], 0);
        assert!(v["stdout"].as_str().unwrap().contains("default-on-works"));
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
        seed_indexed_code_chunk(&runtime, &project);

        let out = search(&runtime, &json!({"query": "compute delta", "limit": 5})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert!(!arr.is_empty(), "expected at least one hit");
        // New flat shape: title + file + lines + snippet at top level.
        assert_eq!(arr[0]["title"], "compute_delta");
        assert_eq!(arr[0]["file"], "src/lib.rs");
        assert!(arr[0].get("lines").is_some());
        assert!(arr[0].get("snippet").is_some());
        // Verbose metadata stays hidden by default.
        assert!(
            arr[0].get("debug").is_none(),
            "debug must be off-by-default"
        );
    }

    #[test]
    fn search_default_view_uses_line_aware_snippet_for_code() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        // Multi-line content so the line-aware path has something to slice.
        let body =
            "fn untouched_a() {}\nfn compute_delta(a: i32) -> i32 { a + 1 }\nfn untouched_b() {}\n";
        seed_indexed_code_chunk_with_body(&runtime, &project, body);

        let out = search(&runtime, &json!({"query": "compute_delta", "limit": 5})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        let snip = arr[0]["snippet"].as_str().unwrap();
        // Matched line is marked with `> ` and surrounding context lines
        // get a leading `  ` prefix.
        assert!(
            snip.lines()
                .any(|l| l.starts_with("> ") && l.contains("compute_delta")),
            "snippet should mark the matched line: {snip}"
        );
    }

    #[test]
    fn search_compact_view_keeps_legacy_char_window() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        let body = "fn compute_delta(a: i32, b: i32) -> i32 { a - b }\n";
        seed_indexed_code_chunk_with_body(&runtime, &project, body);

        let out = search(
            &runtime,
            &json!({"query": "compute_delta", "limit": 5, "view": "compact"}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        let snip = arr[0]["snippet"].as_str().unwrap();
        // Compact never uses the `> `/`  ` line-prefix shape.
        assert!(
            !snip.contains("\n> "),
            "compact view should not be line-aware: {snip}"
        );
    }

    #[test]
    fn search_full_view_returns_entire_chunk_content() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        let body = "line one\nline two with compute_delta\nline three\nline four\n";
        seed_indexed_code_chunk_with_body(&runtime, &project, body);

        let out = search(
            &runtime,
            &json!({"query": "compute_delta", "limit": 5, "view": "full"}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        let snip = arr[0]["snippet"].as_str().unwrap();
        assert!(snip.contains("line one"));
        assert!(snip.contains("line four"));
    }

    #[test]
    fn search_debug_flag_attaches_ranks() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        seed_indexed_code_chunk(&runtime, &project);

        let out = search(
            &runtime,
            &json!({"query": "compute delta", "limit": 5, "debug": true}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        let dbg = arr[0]
            .get("debug")
            .expect("debug block present when requested");
        assert!(dbg.get("ranks").is_some());
        assert!(dbg.get("score_full").is_some());
    }

    #[test]
    fn search_enriches_with_symbol_qn_when_source_id_links_ast_node() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let runtime = make_runtime(project.clone());
        seed_graph(&runtime, &project.display().to_string());

        // Pick the just-seeded ast_node id for compute_delta and tie a
        // chunk to it. The dispatcher's source_id JOIN should then
        // surface the qualified_name on the hit.
        let source_id: i64 = runtime
            .conn
            .query_row(
                "SELECT id FROM ast_nodes WHERE qualified_name = ?",
                ["demo::delta::compute_delta"],
                |r| r.get(0),
            )
            .unwrap();

        let embedder = crux_l6_search::build_embedder(&runtime.config.layer.l6).unwrap();
        let indexer = crux_l6_search::Indexer::new(&runtime.conn);
        let chunk = crux_l6_search::Chunk {
            project_root: project.display().to_string(),
            source_id: Some(source_id),
            file_path: "src/lib.rs".into(),
            language: Some("rust".into()),
            content_type: crux_l6_search::ContentType::Code,
            title: Some("compute_delta".into()),
            content: "fn compute_delta() -> i32 { 0 }\n".into(),
            line_start: 1,
            line_end: 1,
        };
        indexer.index_chunks(&[chunk], embedder.as_ref()).unwrap();

        let out = search(&runtime, &json!({"query": "compute_delta", "limit": 5})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0]["symbol"], "demo::delta::compute_delta");
    }

    fn seed_indexed_code_chunk(runtime: &Runtime, project: &std::path::Path) {
        seed_indexed_code_chunk_with_body(
            runtime,
            project,
            "compute delta over old and new strings",
        );
    }

    fn seed_indexed_code_chunk_with_body(runtime: &Runtime, project: &std::path::Path, body: &str) {
        let embedder = crux_l6_search::build_embedder(&runtime.config.layer.l6).unwrap();
        let indexer = crux_l6_search::Indexer::new(&runtime.conn);
        let chunk = crux_l6_search::Chunk {
            project_root: project.display().to_string(),
            source_id: None,
            file_path: "src/lib.rs".into(),
            language: Some("rust".into()),
            content_type: crux_l6_search::ContentType::Code,
            title: Some("compute_delta".into()),
            content: body.into(),
            line_start: 1,
            line_end: body.lines().count().max(1) as u32,
        };
        indexer.index_chunks(&[chunk], embedder.as_ref()).unwrap();
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

    // ─────────────────────────────────────────────────────────────────
    // L8 auto-surface integration tests
    // ─────────────────────────────────────────────────────────────────

    fn seed_file_obs(
        runtime: &Runtime,
        project: &std::path::Path,
        file_path: &str,
        kind: ObservationKind,
        title: &str,
        importance: u8,
    ) -> i64 {
        let mem = MemoryEngine::new(&runtime.conn).unwrap();
        let mut o = NewObservation::minimal(
            project.display().to_string(),
            kind,
            title,
            "body irrelevant for these tests",
        );
        o.file_path = Some(file_path.into());
        o.importance = importance;
        mem.remember(o).unwrap()
    }

    fn seed_symbol_obs(
        runtime: &Runtime,
        project: &std::path::Path,
        symbol: &str,
        kind: ObservationKind,
        title: &str,
        importance: u8,
    ) -> i64 {
        let mem = MemoryEngine::new(&runtime.conn).unwrap();
        let mut o = NewObservation::minimal(
            project.display().to_string(),
            kind,
            title,
            "body irrelevant",
        );
        o.symbol = Some(symbol.into());
        o.importance = importance;
        mem.remember(o).unwrap()
    }

    #[test]
    fn read_appends_footer_when_file_has_observations() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        let file_rel = "src/cache.rs";
        let file_abs = project.join(file_rel).display().to_string();
        std::fs::write(&file_abs, "fn zstd() {}\n").unwrap();
        let runtime = make_runtime(project.clone());

        // Store observation against the relative path; read with the
        // absolute path — the variant logic must still match.
        seed_file_obs(
            &runtime,
            &project,
            file_rel,
            ObservationKind::Decision,
            "zstd=3 chosen",
            8,
        );

        let out = read(&runtime, &json!({"file_path": file_abs})).unwrap();
        assert!(out.contains("fn zstd()"), "actual file body preserved");
        assert!(
            out.contains("[crux:l8]"),
            "expected auto-surface footer, got: {out}"
        );
        assert!(out.contains("zstd=3 chosen"));
    }

    #[test]
    fn read_no_footer_when_no_observations() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        let abs = project.join("src/empty.rs").display().to_string();
        std::fs::write(&abs, "// nothing\n").unwrap();
        let runtime = make_runtime(project.clone());

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(!out.contains("[crux:l8]"));
    }

    #[test]
    fn read_footer_disabled_via_config() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        let abs = project.join("src/off.rs").display().to_string();
        std::fs::write(&abs, "// x\n").unwrap();
        let mut runtime = make_runtime(project.clone());
        runtime.config.layer.l8.auto_surface = false;

        seed_file_obs(
            &runtime,
            &project,
            "src/off.rs",
            ObservationKind::Guardrail,
            "never surface me",
            9,
        );

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(!out.contains("[crux:l8]"));
        assert!(!out.contains("never surface me"));
    }

    #[test]
    fn read_footer_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        let abs = project.join("src/many.rs").display().to_string();
        std::fs::write(&abs, "// y\n").unwrap();
        let mut runtime = make_runtime(project.clone());
        runtime.config.layer.l8.auto_surface_limit = 2;

        for i in 0..5 {
            seed_file_obs(
                &runtime,
                &project,
                "src/many.rs",
                ObservationKind::Convention,
                &format!("note-{i}"),
                5,
            );
        }

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(out.contains("[crux:l8] 2 past observation(s)"));
        // Exactly two "#<id>" footer lines.
        let footer_lines = out
            .lines()
            .filter(|l| l.trim_start().starts_with('#'))
            .count();
        assert_eq!(footer_lines, 2);
    }

    #[test]
    fn read_footer_matches_relative_path_variant() {
        // Read is called with an absolute path (the real-world shape);
        // observation was stored with the project-relative path. The
        // variant builder must surface both forms.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        let rel = "src/a.rs";
        let abs = project.join(rel).display().to_string();
        std::fs::write(&abs, "// abs variant\n").unwrap();
        let runtime = make_runtime(project.clone());

        seed_file_obs(
            &runtime,
            &project,
            rel,
            ObservationKind::ErrorPattern,
            "stored-relative",
            7,
        );

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(
            out.contains("stored-relative"),
            "relative-path obs should match absolute read"
        );
    }

    #[test]
    fn get_symbol_source_appends_footer_for_symbol_match() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("src/lib.rs"),
            "pub fn compute_delta() -> i32 { 7 }\n",
        )
        .unwrap();
        let runtime = make_runtime(project.clone());
        seed_graph(&runtime, &project.display().to_string());

        seed_symbol_obs(
            &runtime,
            &project,
            "demo::delta::compute_delta",
            ObservationKind::Decision,
            "rayon chosen for perf",
            8,
        );

        let out = get_symbol_source(
            &runtime,
            &json!({
                "qualified_name": "demo::delta::compute_delta",
                "include_metadata": true,
            }),
        )
        .unwrap();
        assert!(out.contains("[crux:l8]"));
        assert!(out.contains("rayon chosen"));
    }

    #[test]
    fn get_symbol_source_appends_footer_for_file_match() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("src/lib.rs"),
            "pub fn compute_delta() -> i32 { 7 }\n",
        )
        .unwrap();
        let runtime = make_runtime(project.clone());
        seed_graph(&runtime, &project.display().to_string());

        // Attach the obs to the FILE (not the symbol).
        seed_file_obs(
            &runtime,
            &project,
            "src/lib.rs",
            ObservationKind::Convention,
            "lib.rs is the crate entrypoint",
            6,
        );

        let out = get_symbol_source(
            &runtime,
            &json!({"qualified_name": "demo::delta::compute_delta"}),
        )
        .unwrap();
        assert!(out.contains("[crux:l8]"));
        assert!(out.contains("crate entrypoint"));
    }

    // ─────────────────────────────────────────────────────────────────
    // crux_read range + symbol slicing
    // ─────────────────────────────────────────────────────────────────

    fn make_multi_line_project(name: &str, body: &str) -> (tempfile::TempDir, PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        let abs = project.join(format!("src/{name}")).display().to_string();
        std::fs::write(&abs, body).unwrap();
        (dir, project, abs)
    }

    #[test]
    fn read_slice_returns_only_requested_lines() {
        let body = (1..=10)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_dir, project, abs) = make_multi_line_project("many.txt", &body);
        let runtime = make_runtime(project);

        let out = read(
            &runtime,
            &json!({"file_path": abs, "offset": 3, "limit": 4}),
        )
        .unwrap();
        // Lines 3..=6 only. Lines 1, 2, 7, 8 must be absent.
        assert!(out.contains("line-3"));
        assert!(out.contains("line-6"));
        assert!(!out.contains("line-1"));
        assert!(!out.contains("line-2"));
        assert!(!out.contains("line-7"));
        assert!(!out.contains("line-8"));
    }

    #[test]
    fn read_slice_limit_zero_goes_to_end_of_file() {
        let body = (1..=5)
            .map(|i| format!("L{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_dir, project, abs) = make_multi_line_project("tail.txt", &body);
        let runtime = make_runtime(project);

        let out = read(
            &runtime,
            &json!({"file_path": abs, "offset": 3, "limit": 0}),
        )
        .unwrap();
        for n in 3..=5 {
            assert!(out.contains(&format!("L{n}")));
        }
        assert!(!out.contains("L1"));
        assert!(!out.contains("L2"));
    }

    #[test]
    fn read_slice_offset_zero_or_one_both_mean_top() {
        let body = "A\nB\nC\n";
        let (_dir, project, abs) = make_multi_line_project("top.txt", body);
        let runtime = make_runtime(project);

        let a = read(
            &runtime,
            &json!({"file_path": abs.clone(), "offset": 0, "limit": 2}),
        )
        .unwrap();
        let b = read(
            &runtime,
            &json!({"file_path": abs, "offset": 1, "limit": 2}),
        )
        .unwrap();
        // Both must contain the top two lines and exclude C.
        for out in [&a, &b] {
            assert!(out.contains('A'));
            assert!(out.contains('B'));
            assert!(!out.contains('C'));
        }
    }

    #[test]
    fn read_slice_clamps_past_end_of_file() {
        let body = "only one line\n";
        let (_dir, project, abs) = make_multi_line_project("tiny.txt", body);
        let runtime = make_runtime(project);

        // offset well past EOF → empty result, not an error.
        let out = read(
            &runtime,
            &json!({"file_path": abs, "offset": 100, "limit": 5}),
        )
        .unwrap();
        assert!(!out.contains("only one line"));
    }

    #[test]
    fn read_full_file_legacy_behavior_unchanged() {
        // No offset/limit/symbol → whole file, no annotation gutter.
        let body = "hello\nworld\n";
        let (_dir, project, abs) = make_multi_line_project("full.txt", body);
        let runtime = make_runtime(project);

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
        // No gutter → no "    1  hello" shape.
        assert!(!out.contains("    1  hello"));
    }

    #[test]
    fn read_symbol_resolves_file_and_range() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("src/lib.rs"),
            "fn before() {}\npub fn compute_delta() -> i32 { 7 }\nfn after() {}\n",
        )
        .unwrap();
        let runtime = make_runtime(project.clone());

        // Seed the symbol at line 2 so `symbol =` resolves to line 2 only.
        let store = GraphStore::new(&runtime.conn);
        let result = crux_l5_ast::ParseResult {
            nodes: vec![crux_l5_ast::ParsedNode {
                kind: NodeKind::Function,
                name: "compute_delta".to_string(),
                qualified_name: "demo::delta::compute_delta".to_string(),
                line_start: 2,
                line_end: 2,
                parent_qn: Some("demo::delta".to_string()),
                signature: Some("pub fn compute_delta()".to_string()),
                is_test: false,
            }],
            edges: vec![],
        };
        store
            .write(
                &project.display().to_string(),
                "src/lib.rs",
                "rust",
                "deadbeef",
                &result,
            )
            .unwrap();

        let out = read(&runtime, &json!({"symbol": "demo::delta::compute_delta"})).unwrap();
        assert!(
            out.contains("compute_delta"),
            "symbol body should be present"
        );
        assert!(
            !out.contains("fn before()"),
            "line 1 must be excluded: {out}"
        );
        assert!(
            !out.contains("fn after()"),
            "line 3 must be excluded: {out}"
        );
        // Metadata prefix present by default.
        assert!(out.contains("file: src/lib.rs"));
        assert!(out.contains("lines: 2-2"));
    }

    #[test]
    fn read_symbol_not_found_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime(dir.path().to_path_buf());
        let err = read(&runtime, &json!({"symbol": "does::not::exist"})).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn read_missing_both_file_path_and_symbol_errors() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime(dir.path().to_path_buf());
        let err = read(&runtime, &json!({})).unwrap_err();
        assert!(err.contains("file_path") || err.contains("symbol"));
    }

    #[test]
    fn read_range_still_appends_memory_footer() {
        // Range reads must not bypass L8 auto-surface.
        let body = (1..=20)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_dir, project, abs) = make_multi_line_project("ranged.rs", &body);
        let runtime = make_runtime(project.clone());
        seed_file_obs(
            &runtime,
            &project,
            "src/ranged.rs",
            ObservationKind::Decision,
            "ranged-note",
            7,
        );

        let out = read(
            &runtime,
            &json!({"file_path": abs, "offset": 5, "limit": 3}),
        )
        .unwrap();
        assert!(out.contains("[crux:l8]"));
        assert!(out.contains("ranged-note"));
    }

    // ─────────────────────────────────────────────────────────────────
    // crux_read outline-first auto-mode (L4+L5 fusion)
    // ─────────────────────────────────────────────────────────────────

    /// Seed a parsed file with `n_symbols` function rows spaced
    /// evenly through `total_lines`. Mirrors what L5 indexer would
    /// emit for a real file. `rel_path` is the project-relative path
    /// the GraphStore key needs.
    fn seed_outline_graph(
        runtime: &Runtime,
        project: &str,
        rel_path: &str,
        n_symbols: usize,
        total_lines: u32,
    ) {
        let store = GraphStore::new(&runtime.conn);
        let span = (total_lines / n_symbols.max(1) as u32).max(1);
        let nodes = (0..n_symbols)
            .map(|i| {
                let line_start = (i as u32) * span + 1;
                let line_end = line_start + span - 1;
                ParsedNode {
                    kind: NodeKind::Function,
                    name: format!("fn_{i}"),
                    qualified_name: format!("demo::big::fn_{i}"),
                    line_start,
                    line_end,
                    parent_qn: Some("demo::big".into()),
                    signature: Some(format!("pub fn fn_{i}() -> i32")),
                    is_test: false,
                }
            })
            .collect();
        let result = ParseResult {
            nodes,
            edges: vec![],
        };
        store
            .write(project, rel_path, "rust", "deadbeef", &result)
            .unwrap();
    }

    fn make_big_file_project(name: &str, line_count: u32) -> (tempfile::TempDir, PathBuf, String) {
        let body = (1..=line_count)
            .map(|i| format!("// line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        let abs = project.join(format!("src/{name}")).display().to_string();
        std::fs::write(&abs, body).unwrap();
        (dir, project, abs)
    }

    #[test]
    fn read_outline_when_file_above_threshold_and_l5_indexed() {
        let (_dir, project, abs) = make_big_file_project("big.rs", 1500);
        let mut runtime = make_runtime(project.clone());
        runtime.config.layer.l4.outline_above_lines = 1000;
        let project_s = project.display().to_string();
        seed_outline_graph(&runtime, &project_s, "src/big.rs", 5, 1500);

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(
            out.contains("[crux:l4+l5]"),
            "outline header missing: {out}"
        );
        assert!(out.contains("1500 lines"));
        assert!(out.contains("5 symbols"));
        assert!(out.contains("demo::big::fn_0"));
        assert!(out.contains("demo::big::fn_4"));
        // Body content (the `// line N` comments) must NOT be present.
        assert!(
            !out.contains("// line 100"),
            "outline must not include body: {out}"
        );
    }

    #[test]
    fn read_outline_skipped_when_below_threshold() {
        let (_dir, project, abs) = make_big_file_project("small.rs", 50);
        let mut runtime = make_runtime(project.clone());
        runtime.config.layer.l4.outline_above_lines = 1000;
        let project_s = project.display().to_string();
        seed_outline_graph(&runtime, &project_s, "src/small.rs", 3, 50);

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(
            !out.contains("[crux:l4+l5]"),
            "small file must not trigger outline: {out}"
        );
        assert!(out.contains("// line 1"));
    }

    #[test]
    fn read_outline_falls_back_when_l5_empty() {
        // File above threshold but L5 has no symbols indexed → must
        // gracefully return full body, not error.
        let (_dir, project, abs) = make_big_file_project("unindexed.rs", 1500);
        let mut runtime = make_runtime(project);
        runtime.config.layer.l4.outline_above_lines = 1000;

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(!out.contains("[crux:l4+l5]"), "no L5 = no outline: {out}");
        assert!(out.contains("// line 1"));
        assert!(out.contains("// line 1500"));
    }

    #[test]
    fn read_outline_force_full_bypass() {
        let (_dir, project, abs) = make_big_file_project("force.rs", 1500);
        let mut runtime = make_runtime(project.clone());
        runtime.config.layer.l4.outline_above_lines = 1000;
        let project_s = project.display().to_string();
        seed_outline_graph(&runtime, &project_s, "src/force.rs", 5, 1500);

        let out = read(&runtime, &json!({"file_path": abs, "force_full": true})).unwrap();
        assert!(
            !out.contains("[crux:l4+l5]"),
            "force_full must skip outline: {out}"
        );
        assert!(out.contains("// line 100"));
    }

    #[test]
    fn read_outline_skipped_with_offset_limit() {
        // Range reads are already lean — outline must not fire.
        let (_dir, project, abs) = make_big_file_project("ranged.rs", 1500);
        let mut runtime = make_runtime(project.clone());
        runtime.config.layer.l4.outline_above_lines = 1000;
        let project_s = project.display().to_string();
        seed_outline_graph(&runtime, &project_s, "src/ranged.rs", 5, 1500);

        let out = read(
            &runtime,
            &json!({"file_path": abs, "offset": 100, "limit": 5}),
        )
        .unwrap();
        assert!(
            !out.contains("[crux:l4+l5]"),
            "range read must skip outline: {out}"
        );
        assert!(out.contains("// line 100"));
        assert!(out.contains("// line 104"));
    }

    #[test]
    fn read_outline_disabled_when_threshold_zero() {
        let (_dir, project, abs) = make_big_file_project("disabled.rs", 5000);
        let mut runtime = make_runtime(project.clone());
        runtime.config.layer.l4.outline_above_lines = 0; // explicitly disable
        let project_s = project.display().to_string();
        seed_outline_graph(&runtime, &project_s, "src/disabled.rs", 50, 5000);

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(
            !out.contains("[crux:l4+l5]"),
            "threshold=0 must disable outline: {out}"
        );
        assert!(out.contains("// line 4999"));
    }

    #[test]
    fn read_outline_emits_signature_and_lines() {
        let (_dir, project, abs) = make_big_file_project("sig.rs", 1500);
        let mut runtime = make_runtime(project.clone());
        runtime.config.layer.l4.outline_above_lines = 1000;
        let project_s = project.display().to_string();
        seed_outline_graph(&runtime, &project_s, "src/sig.rs", 4, 1500);

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(out.contains("pub fn fn_0()"), "signature missing: {out}");
        assert!(out.contains("Function"), "kind missing: {out}");
        assert!(
            out.contains("lines 1-") || out.contains("line 1"),
            "line range missing: {out}"
        );
        // Drill-in hint must be present so agents know the next step.
        assert!(out.contains("crux_read --symbol="));
        assert!(out.contains("--force_full=true"));
    }

    #[test]
    fn read_outline_truncates_above_max_rows() {
        // Synthetic monster with 250 symbols — must collapse to
        // OUTLINE_MAX_ROWS (200) plus a "... and N more" hint.
        let (_dir, project, abs) = make_big_file_project("monster.rs", 5000);
        let mut runtime = make_runtime(project.clone());
        runtime.config.layer.l4.outline_above_lines = 1000;
        let project_s = project.display().to_string();
        seed_outline_graph(&runtime, &project_s, "src/monster.rs", 250, 5000);

        let out = read(&runtime, &json!({"file_path": abs})).unwrap();
        assert!(out.contains("[crux:l4+l5]"));
        assert!(out.contains("250 symbols"));
        assert!(
            out.contains("and 50 more"),
            "truncation hint missing: {out}"
        );
        // First 200 rows must be present, row 200+ must not.
        assert!(out.contains("demo::big::fn_0"));
        assert!(out.contains("demo::big::fn_199"));
        assert!(
            !out.contains("demo::big::fn_249"),
            "row 250 should be hidden behind truncation: {out}"
        );
    }

    #[test]
    fn get_symbol_source_dedupes_when_obs_matches_both_symbol_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("src/lib.rs"),
            "pub fn compute_delta() -> i32 { 7 }\n",
        )
        .unwrap();
        let runtime = make_runtime(project.clone());
        seed_graph(&runtime, &project.display().to_string());

        // Single obs carrying BOTH symbol and file_path that match.
        let mem = MemoryEngine::new(&runtime.conn).unwrap();
        let mut o = NewObservation::minimal(
            project.display().to_string(),
            ObservationKind::Decision,
            "joint obs",
            "body",
        );
        o.symbol = Some("demo::delta::compute_delta".into());
        o.file_path = Some("src/lib.rs".into());
        mem.remember(o).unwrap();

        let out = get_symbol_source(
            &runtime,
            &json!({"qualified_name": "demo::delta::compute_delta"}),
        )
        .unwrap();
        // Exactly one footer entry despite matching both filters.
        let count = out.matches("joint obs").count();
        assert_eq!(count, 1, "dedup failed: footer = {out}");
    }
}
