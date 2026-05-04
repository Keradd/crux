use anyhow::Result;
use clap::Args as ClapArgs;

use crux_core::{telemetry, Runtime};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long)]
    pub layer: Option<String>,
}

pub fn run(cli: &Cli, args: &Args) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project.clone())
    } else {
        None
    };
    let runtime = Runtime::open(project_opt.clone())?;

    let pr_str = project_opt.as_ref().map(|p| p.display().to_string());
    let mut stats = telemetry::stats_by_layer(&runtime.conn, pr_str.as_deref())?;

    if let Some(filter) = &args.layer {
        stats.retain(|s| &s.layer == filter);
    }

    if cli.json {
        let arr: Vec<_> = stats
            .iter()
            .map(|s| {
                serde_json::json!({
                    "layer": s.layer,
                    "events": s.events,
                    "original_tokens": s.original_tokens,
                    "compressed_tokens": s.compressed_tokens,
                    "savings": s.savings,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }

    if stats.is_empty() {
        println!("(no telemetry recorded yet)");
        return Ok(());
    }

    println!(
        "{:<8} {:>10} {:>16} {:>16} {:>14}",
        "layer", "events", "original tok", "compressed tok", "saved tok"
    );
    for s in &stats {
        println!(
            "{:<8} {:>10} {:>16} {:>16} {:>14}",
            s.layer, s.events, s.original_tokens, s.compressed_tokens, s.savings
        );
    }
    Ok(())
}
