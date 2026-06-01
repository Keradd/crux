use serde_json::{json, Value};

use crux_l6_search::{build_embedder, ContentType, SearchConfig, SearchEngine, SearchOptions};

use crate::dispatch::AppContext;
use crate::tools::common::{project_root, round4};
use crate::tools::Tool;

const SEARCH_DEFAULT_VIEW_LINES: u64 = 3;
const SEARCH_MAX_VIEW_LINES: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchView {
    Compact,
    Default,
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

pub fn search(ctx: &AppContext, args: &Value) -> Result<String, String> {
    let project = project_root(ctx)
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
    let embedder = build_embedder(&ctx.config.layer.l6).map_err(|e| e.to_string())?;
    let search_cfg = SearchConfig::from(&ctx.config.layer.l6);
    let engine = SearchEngine::with_config(&ctx.conn, embedder.as_ref(), search_cfg);
    let hits = engine
        .hybrid_search(&project, query, &opts)
        .map_err(|e| e.to_string())?;

    let payload: Value = hits
        .iter()
        .map(|h| render_hit(ctx, h, query, view, view_lines, debug))
        .collect::<Vec<Value>>()
        .into();
    Ok(serde_json::to_string_pretty(&payload).unwrap())
}

fn render_hit(
    ctx: &AppContext,
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
        .and_then(|sid| symbol_qn_for_source_id(ctx, sid));

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

fn symbol_qn_for_source_id(ctx: &AppContext, source_id: i64) -> Option<String> {
    ctx.conn
        .query_row(
            "SELECT qualified_name FROM ast_nodes WHERE id = ?",
            [source_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
}

pub struct Search;

impl Tool for Search {
    fn name(&self) -> &'static str {
        "crux_search"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        search(ctx, args)
    }
}
