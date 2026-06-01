use serde_json::Value;

use crux_l5_ast::{GraphStore, NodeKind};

use crate::dispatch::AppContext;
use crate::tools::common::{project_root, project_root_path, serialize_nodes};
use crate::tools::memory;
use crate::tools::Tool;

pub fn find_symbol(ctx: &AppContext, args: &Value) -> Result<String, String> {
    let project = project_root(ctx)
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

    let store = GraphStore::new(&ctx.conn);
    let nodes = if exact {
        store
            .find_symbol(&project, name, kind)
            .map_err(|e| e.to_string())?
    } else {
        store
            .find_symbol_like(&project, name, kind, limit)
            .map_err(|e| e.to_string())?
    };

    Ok(serde_json::to_string_pretty(&serialize_nodes(&nodes)).unwrap())
}

pub fn get_symbol_source(ctx: &AppContext, args: &Value) -> Result<String, String> {
    let project_path = project_root_path(ctx)
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

    let store = GraphStore::new(&ctx.conn);
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

    if let Some(footer) = memory::memory_footer_for_symbol(ctx, &project_path, qn, &n.file_path) {
        out.push_str(&footer);
    }
    Ok(out)
}

pub struct FindSymbol;
pub struct GetSymbolSource;

impl Tool for FindSymbol {
    fn name(&self) -> &'static str {
        "crux_find_symbol"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        find_symbol(ctx, args)
    }
}

impl Tool for GetSymbolSource {
    fn name(&self) -> &'static str {
        "crux_get_symbol_source"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        get_symbol_source(ctx, args)
    }
}
