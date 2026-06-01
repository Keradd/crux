use serde_json::Value;

use crux_l5_ast::GraphStore;

use crate::dispatch::AppContext;
use crate::tools::common::{project_root, serialize_nodes};
use crate::tools::Tool;

pub fn query_graph(ctx: &AppContext, args: &Value) -> Result<String, String> {
    let project = project_root(ctx)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'qualified_name'".to_string())?;
    let direction = args
        .get("direction")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'direction'".to_string())?;

    let store = GraphStore::new(&ctx.conn);
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

pub fn impact(ctx: &AppContext, args: &Value) -> Result<String, String> {
    let project = project_root(ctx)
        .ok_or_else(|| "no project context — run `crux init` first".to_string())?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'qualified_name'".to_string())?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as u32;
    let max = args.get("max").and_then(|v| v.as_u64()).unwrap_or(100) as u32;

    let store = GraphStore::new(&ctx.conn);
    let nodes = store
        .impact_radius(&project, qn, depth, max)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_string_pretty(&serialize_nodes(&nodes)).unwrap())
}

pub struct QueryGraph;
pub struct Impact;

impl Tool for QueryGraph {
    fn name(&self) -> &'static str {
        "crux_query_graph"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        query_graph(ctx, args)
    }
}

impl Tool for Impact {
    fn name(&self) -> &'static str {
        "crux_impact"
    }
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String> {
        impact(ctx, args)
    }
}
