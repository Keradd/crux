use anyhow::{Context, Result};
use clap::Args as ClapArgs;

use crux_core::Runtime;
use crux_l11_digest::DigestEngine;

use super::resolve_project_root;
use crate::Cli;

const DEFAULT_SESSION: &str = "default";

#[derive(Debug, ClapArgs)]
pub struct DigestArgs {
    #[arg(long, env = "CRUX_SESSION")]
    pub session: Option<String>,

    #[arg(long)]
    pub pending: bool,

    #[arg(long)]
    pub history: bool,

    #[arg(long, default_value_t = 10)]
    pub limit: usize,
}

#[derive(Debug, ClapArgs)]
pub struct CompactArgs {
    #[arg(long, env = "CRUX_SESSION")]
    pub session: Option<String>,
}

pub fn run_digest(cli: &Cli, args: &DigestArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone())).context("opening CRUX runtime")?;
    let engine = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());

    if args.history {
        let project_s = project.display().to_string();
        let digests = engine.list_digests(&project_s, args.limit.max(1))?;
        if cli.json {
            let arr: Vec<_> = digests
                .iter()
                .map(|d| serde_json::to_value(d).unwrap())
                .collect();
            println!("{}", serde_json::to_string_pretty(&arr)?);
            return Ok(());
        }
        if digests.is_empty() {
            println!("(no digests recorded for {})", project_s);
            return Ok(());
        }
        for d in &digests {
            println!(
                "#{:<5} session={} events={} ts=[{}..{}] obs={:?}",
                d.id,
                d.session_id,
                d.event_count,
                d.ts_start_epoch,
                d.ts_end_epoch,
                d.observation_id
            );
        }
        return Ok(());
    }

    let session = args
        .session
        .clone()
        .unwrap_or_else(|| DEFAULT_SESSION.to_string());

    let rendered = if args.pending {
        engine.summarize(&session)?
    } else {
        engine.latest_summary(&session)?
    };

    if cli.json {
        println!(
            "{}",
            serde_json::json!({
                "session": session,
                "summary": rendered,
            })
        );
    } else {
        println!("{}", rendered);
    }
    Ok(())
}

pub fn run_compact(cli: &Cli, args: &CompactArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone())).context("opening CRUX runtime")?;
    let engine = DigestEngine::new(&runtime.conn, runtime.config.layer.l11.clone());

    let session = args
        .session
        .clone()
        .unwrap_or_else(|| DEFAULT_SESSION.to_string());

    let pending = engine.pending_count(&session)?;
    let digest = engine.compact(&session)?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&digest)?);
    } else if pending == 0 {
        println!(
            "compacted session={} (no pending events; digest #{} contains 0 events)",
            session, digest.id
        );
    } else {
        println!(
            "compacted session={} → digest #{} ({} events, observation_id={:?})",
            session, digest.id, digest.event_count, digest.observation_id
        );
    }
    Ok(())
}
