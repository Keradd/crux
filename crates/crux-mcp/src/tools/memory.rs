use std::str::FromStr;

use serde_json::Value;

use crux_l8_memory::{
    MemoryEngine, NewObservation, ObservationKind, RankedObservation, RecallQuery,
};

use crate::dispatch::AppContext;
use crate::tools::common::{file_path_variants, first_line, project_root};
use crate::tools::Tool;

pub fn remember(ctx: &AppContext, args: &Value) -> Result<String, String> {
    let project = project_root(ctx).ok_or_else(|| {
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

    let mem = MemoryEngine::new(&ctx.conn).map_err(|e| e.to_string())?;
    let id = mem.remember(obs).map_err(|e| e.to_string())?;
    Ok(format!("remembered #{id} ({})", kind_s))
}

pub fn recall(ctx: &AppContext, args: &Value) -> Result<String, String> {
    let project = project_root(ctx)
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
    let mem = MemoryEngine::new(&ctx.conn).map_err(|e| e.to_string())?;
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

pub fn memory_footer_for_file(
    ctx: &AppContext,
    project_path: &std::path::Path,
    file_path: &str,
) -> Option<String> {
    let l8 = &ctx.config.layer.l8;
    if !l8.auto_surface || l8.auto_surface_limit == 0 {
        return None;
    }
    let project = project_path.display().to_string();
    let variants = file_path_variants(project_path, file_path);
    let borrows: Vec<&str> = variants.iter().map(|s| s.as_str()).collect();
    let mem = MemoryEngine::new(&ctx.conn).ok()?;
    let hits = mem
        .recall_by_file(&project, &borrows, l8.auto_surface_limit)
        .ok()?;
    if hits.is_empty() {
        None
    } else {
        Some(format_memory_footer(&hits))
    }
}

pub fn memory_footer_for_symbol(
    ctx: &AppContext,
    project_path: &std::path::Path,
    qualified_name: &str,
    file_path: &str,
) -> Option<String> {
    let l8 = &ctx.config.layer.l8;
    if !l8.auto_surface || l8.auto_surface_limit == 0 {
        return None;
    }
    let project = project_path.display().to_string();
    let mem = MemoryEngine::new(&ctx.conn).ok()?;

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

pub fn format_memory_footer(hits: &[RankedObservation]) -> String {
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

pub struct Remember;
pub struct Recall;

impl Tool for Remember {
    fn name(&self) -> &'static str {
        "crux_remember"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        remember(ctx, args)
    }
}

impl Tool for Recall {
    fn name(&self) -> &'static str {
        "crux_recall"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        recall(ctx, args)
    }
}
