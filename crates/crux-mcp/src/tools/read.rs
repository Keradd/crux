use std::path::{Path, PathBuf};

use serde_json::Value;

use crux_l4_readcache::{CacheDecision, CheckOptions, ContextIgnore, ReadCacheManager, ReadEvent};
use crux_l5_ast::{GraphNode, GraphStore};

use crate::dispatch::AppContext;
use crate::tools::common::{file_path_variants, project_root_path, resolve_in_project};
use crate::tools::memory;
use crate::tools::Tool;

pub fn read(ctx: &AppContext, args: &Value) -> Result<String, String> {
    let project = project_root_path(ctx)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;

    let (file_path, offset_lines, limit_lines, symbol_meta) =
        if let Some(qn) = args.get("symbol").and_then(|v| v.as_str()) {
            let project_s = project.display().to_string();
            let store = GraphStore::new(&ctx.conn);
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
            let fp_raw = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'file_path' (or 'symbol')".to_string())?;
            let resolved = resolve_in_project(&project, fp_raw)?;
            let fp = resolved.display().to_string();
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
        delta_max_bytes: Some(ctx.config.layer.l4.delta_max_bytes),
    };
    let mgr = ReadCacheManager::new(&ctx.conn);
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
            let raw =
                std::fs::read_to_string(&path_buf).map_err(|e| format!("read failed: {e}"))?;

            let outline_threshold = ctx.config.layer.l4.outline_above_lines;
            let want_full_read =
                offset_lines == 0 && limit_lines == 0 && symbol_meta.is_none() && !force_full;
            if want_full_read && outline_threshold > 0 {
                let line_count = raw.lines().count() as u64;
                if line_count >= outline_threshold {
                    if let Some(outline) = try_render_outline(ctx, &project, &file_path, line_count)
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

    if let Some(footer) = memory::memory_footer_for_file(ctx, &project, &file_path) {
        body.push_str(&footer);
    }
    Ok(body)
}

const OUTLINE_MAX_ROWS: usize = 200;

fn try_render_outline(
    ctx: &AppContext,
    project: &Path,
    file_path: &str,
    line_count: u64,
) -> Option<String> {
    let project_s = project.display().to_string();
    let store = GraphStore::new(&ctx.conn);
    let variants = file_path_variants(project, file_path);
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

fn truncate_signature(sig: &str) -> String {
    let collapsed: String = sig.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= 80 {
        collapsed
    } else {
        format!("{}…", &collapsed[..79])
    }
}

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

pub struct Read;

impl Tool for Read {
    fn name(&self) -> &'static str {
        "crux_read"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        read(ctx, args)
    }
}
