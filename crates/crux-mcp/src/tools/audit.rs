use serde_json::{json, Value};

use crux_core::{config::LayerToggles, telemetry};

use crate::dispatch::AppContext;
use crate::tools::common::project_root;
use crate::tools::Tool;

pub fn audit(ctx: &AppContext) -> Result<String, String> {
    let project = project_root(ctx);
    let stats =
        telemetry::stats_by_layer(&ctx.conn, project.as_deref()).map_err(|e| e.to_string())?;

    let layers = &ctx.config.layers;
    let payload = json!({
        "project": project,
        "layers": {
            "l1_output": layers.l1_output,
            "l2_mcp_shrink": layers.l2_mcp_shrink,
            "l3_bash_filter": layers.l3_bash_filter,
            "l4_read_cache": layers.l4_read_cache,
            "l5_ast_graph": layers.l5_ast_graph,
            "l6_hybrid_search": layers.l6_hybrid_search,
            "l7_sandbox": layers.l7_sandbox,
            "l8_memory": layers.l8_memory,
            "l9_coach": layers.l9_coach,
            "l10_setup": layers.l10_setup,
            "l11_digest": layers.l11_digest,
            "l12_hygiene": layers.l12_hygiene,
        },
        "layers_info": layers_info(layers),
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

fn layers_info(t: &LayerToggles) -> Value {
    json!({
        "l1_output":        layer_info_entry(t.l1_output, None),
        "l2_mcp_shrink":    layer_info_entry(t.l2_mcp_shrink, None),
        "l3_bash_filter":   layer_info_entry(t.l3_bash_filter, None),
        "l4_read_cache":    layer_info_entry(t.l4_read_cache, None),
        "l5_ast_graph":     layer_info_entry(t.l5_ast_graph, None),
        "l6_hybrid_search": layer_info_entry(t.l6_hybrid_search, None),
        "l7_sandbox":       layer_info_entry(t.l7_sandbox, None),
        "l8_memory":        layer_info_entry(t.l8_memory, None),
        "l9_coach":         layer_info_entry(t.l9_coach, None),
        "l10_setup":        layer_info_entry(t.l10_setup, None),
        "l11_digest":       layer_info_entry(t.l11_digest, None),
        "l12_hygiene":      layer_info_entry(
            t.l12_hygiene,
            if t.l12_hygiene { None } else { Some("opt-in hygiene layer") },
        ),
    })
}

pub struct Audit;

impl Tool for Audit {
    fn name(&self) -> &'static str {
        "crux_audit"
    }
    fn call(&self, ctx: &AppContext, _args: &Value) -> Result<String, String> {
        audit(ctx)
    }
}

fn layer_info_entry(enabled: bool, reason: Option<&str>) -> Value {
    match reason {
        Some(r) => json!({ "available": true, "enabled": enabled, "reason": r }),
        None => json!({ "available": true, "enabled": enabled }),
    }
}
