//! `crux audit` — health snapshot via Layer 9 Coach.
//!
//! Thin wrapper around [`crux_l9_coach::CoachEngine::snapshot`] that
//! renders the result as a human-readable or JSON report alongside the
//! raw per-layer telemetry table. `crux coach snapshot` is equivalent.

use anyhow::Result;
use clap::Args as ClapArgs;

use crux_core::{telemetry, Runtime};
use crux_l9_coach::CoachEngine;

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct Args {}

pub fn run(cli: &Cli, _args: &Args) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project.clone())
    } else {
        None
    };
    let runtime = Runtime::open(project_opt.clone())?;

    let coach = CoachEngine::new(&runtime.conn, &runtime.config, project_opt.as_deref());
    let data = coach.snapshot()?;

    let pr_str = project_opt.as_ref().map(|p| p.display().to_string());
    let stats = telemetry::stats_by_layer(&runtime.conn, pr_str.as_deref())?;

    if cli.json {
        let payload = serde_json::json!({
            "project": pr_str,
            "coach": data,
            "layers_toggled": active_layer_summary(&runtime.config.layers),
            "telemetry": stats.iter().map(|s| serde_json::json!({
                "layer": s.layer,
                "events": s.events,
                "original_tokens": s.original_tokens,
                "compressed_tokens": s.compressed_tokens,
                "savings": s.savings,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("CRUX audit — score {} ({})", data.health_score, data.grade);
    match &project_opt {
        Some(p) => println!("project: {}", p.display()),
        None => println!("project: (none — running outside a CRUX-initialized project)"),
    }
    println!();
    println!("active layers:");
    print_layer(&runtime.config.layers.l1_output, "L1  output compression");
    print_layer(
        &runtime.config.layers.l2_mcp_shrink,
        "L2  MCP description shrinker",
    );
    print_layer(&runtime.config.layers.l3_bash_filter, "L3  bash filter");
    print_layer(&runtime.config.layers.l4_read_cache, "L4  read cache");
    print_layer(&runtime.config.layers.l5_ast_graph, "L5  AST graph");
    print_layer(&runtime.config.layers.l6_hybrid_search, "L6  hybrid search");
    print_layer(&runtime.config.layers.l7_sandbox, "L7  sandbox");
    print_layer(&runtime.config.layers.l8_memory, "L8  memory");
    print_layer(&runtime.config.layers.l9_coach, "L9  coach");
    print_layer(&runtime.config.layers.l10_setup, "L10 setup");
    print_layer(&runtime.config.layers.l11_digest, "L11 digest");
    println!();

    println!("snapshot:");
    println!("  ctx window   : {}", data.snapshot.context_window);
    println!(
        "  CLAUDE.md    : {} tok ({:.2}% of ctx)",
        data.snapshot.claude_md_tokens, data.snapshot.claude_md_pct
    );
    println!(
        "  telemetry    : {} events, {} tok saved ({:.1}%)",
        data.snapshot.telemetry_events,
        data.snapshot.total_savings_tokens,
        data.snapshot.savings_pct
    );
    println!("  observations : {}", data.snapshot.memory_observations);
    println!();

    if !data.patterns_good.is_empty() {
        println!("good:");
        for p in &data.patterns_good {
            println!("  + {} — {}", p.name, p.detail);
        }
    }
    if !data.patterns_bad.is_empty() {
        println!("bad:");
        for p in &data.patterns_bad {
            let sev = p
                .severity
                .map(|s| format!("{:?}", s).to_lowercase())
                .unwrap_or_default();
            println!("  - [{}] {} — {}", sev, p.name, p.detail);
            if let Some(fix) = &p.fix {
                println!("      fix: {}", fix);
            }
            if let Some(sv) = &p.savings {
                println!("      savings: {}", sv);
            }
        }
    }

    if !stats.is_empty() {
        println!();
        println!("telemetry by layer:");
        println!(
            "  {:<8} {:>10} {:>16} {:>14}",
            "layer", "events", "original tok", "saved tok"
        );
        for s in &stats {
            println!(
                "  {:<8} {:>10} {:>16} {:>14}",
                s.layer, s.events, s.original_tokens, s.savings
            );
        }
    }

    Ok(())
}

fn print_layer(active: &bool, label: &str) {
    let marker = if *active { "ON " } else { "off" };
    println!("  [{}] {}", marker, label);
}

fn active_layer_summary(t: &crux_core::config::LayerToggles) -> serde_json::Value {
    serde_json::json!({
        "l1_output": t.l1_output,
        "l2_mcp_shrink": t.l2_mcp_shrink,
        "l3_bash_filter": t.l3_bash_filter,
        "l4_read_cache": t.l4_read_cache,
        "l5_ast_graph": t.l5_ast_graph,
        "l6_hybrid_search": t.l6_hybrid_search,
        "l7_sandbox": t.l7_sandbox,
        "l8_memory": t.l8_memory,
        "l9_coach": t.l9_coach,
        "l10_setup": t.l10_setup,
        "l11_digest": t.l11_digest,
    })
}
