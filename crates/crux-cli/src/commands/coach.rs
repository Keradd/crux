//! `crux coach` — Layer 9 health snapshot + loop-check + drift-check.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

use crux_core::paths::expand_user_path;
use crux_core::Runtime;
use crux_l9_coach::{audit_openclaw, openclaw_category_label, CoachEngine, DriftTracker, LoopDetector};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Print current health score + patterns (default when no sub given).
    Snapshot(SnapshotArgs),
    /// Persist the snapshot to `quality_scores` so history grows.
    Record(SnapshotArgs),
    /// Check a session for repetition / loops via token Jaccard.
    Loop(LoopArgs),
    /// Check CLAUDE.md for drift vs last session.
    Drift,
    /// Audit an OpenClaw directory (port of alex/token-optimizer's
    /// context-audit). Reports per-component token cost +
    /// actionable trim/archive/disable recommendations.
    Openclaw(OpenclawArgs),
}

#[derive(Debug, Default, ClapArgs)]
pub struct OpenclawArgs {
    /// OpenClaw directory to audit. Defaults to `~/.openclaw`, then
    /// `<project>/.openclaw` if the home dir is missing.
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Default, ClapArgs)]
pub struct SnapshotArgs {
    #[arg(long)]
    pub session: Option<String>,
}

#[derive(Debug, ClapArgs)]
pub struct LoopArgs {
    #[arg(long)]
    pub session: String,
    #[arg(long, default_value = "")]
    pub user: String,
    #[arg(long, default_value = "")]
    pub tool: String,
    /// Clear loop state for this session first.
    #[arg(long)]
    pub reset: bool,
}

pub fn run(cli: &Cli, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::Snapshot(a) => snapshot(cli, a, /*persist=*/ false),
        Cmd::Record(a) => snapshot(cli, a, /*persist=*/ true),
        Cmd::Loop(a) => loop_check(cli, a),
        Cmd::Drift => drift_check(cli),
        Cmd::Openclaw(a) => openclaw_audit(cli, a),
    }
}

fn snapshot(cli: &Cli, args: &SnapshotArgs, persist: bool) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project.clone())
    } else {
        None
    };
    let runtime = Runtime::open(project_opt.clone())?;
    let coach = CoachEngine::new(&runtime.conn, &runtime.config, project_opt.as_deref());

    let data = if persist {
        coach.persist(args.session.as_deref())?
    } else {
        coach.snapshot()?
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }

    println!("CRUX coach — score {} ({})", data.health_score, data.grade);
    if let Some(p) = &project_opt {
        println!("project: {}", p.display());
    }
    println!();
    println!("snapshot:");
    println!("  ctx window    : {}", data.snapshot.context_window);
    println!(
        "  CLAUDE.md     : {} tok ({:.2}% of ctx)",
        data.snapshot.claude_md_tokens, data.snapshot.claude_md_pct
    );
    println!(
        "  telemetry     : {} events, {} tok saved ({:.1}%)",
        data.snapshot.telemetry_events,
        data.snapshot.total_savings_tokens,
        data.snapshot.savings_pct
    );
    println!("  L4 cache hits : {}", data.snapshot.l4_cache_hits);
    println!("  observations  : {}", data.snapshot.memory_observations);
    println!(
        "  layers active : {}/10 ({} unused)",
        data.snapshot.active_layers, data.snapshot.unused_layers
    );
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

    if persist {
        println!();
        println!("(persisted to quality_scores + claude_md_history)");
    }
    Ok(())
}

fn loop_check(cli: &Cli, args: &LoopArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project.clone())
    } else {
        None
    };
    let runtime = Runtime::open(project_opt)?;
    let detector = LoopDetector::new(&runtime.conn);
    if args.reset {
        detector.reset(&args.session)?;
    }
    let r = detector.check(&args.session, &args.user, &args.tool)?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    if r.is_loop {
        println!("loop: YES (jaccard {:.2})", r.similarity);
        if let Some(w) = &r.warning {
            println!("warning: {}", w);
        }
    } else {
        println!("loop: no (jaccard {:.2})", r.similarity);
    }
    Ok(())
}

fn drift_check(cli: &Cli) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    if !project.join(".crux").is_dir() {
        return Err(anyhow::anyhow!(
            "no .crux/ in {}; run `crux init` first",
            project.display()
        ));
    }
    let runtime = Runtime::open(Some(project.clone()))?;
    let tracker = DriftTracker::new(&runtime.conn);
    let Some(r) = tracker.check(&project)? else {
        println!("(no CLAUDE.md to track)");
        return Ok(());
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }

    println!("CLAUDE.md drift:");
    println!("  current hash : {}", &r.current_hash[..12]);
    if let Some(prev) = &r.previous_hash {
        println!("  previous hash: {}", &prev[..12]);
    } else {
        println!("  previous hash: (none — first run)");
    }
    println!(
        "  size         : {} bytes, ~{} tokens",
        r.byte_size, r.tokens_est
    );
    println!("  history depth: {}", r.history_depth);
    println!("  changed      : {}", if r.changed { "yes" } else { "no" });
    Ok(())
}

fn openclaw_audit(cli: &Cli, args: &OpenclawArgs) -> Result<()> {
    let dir = resolve_openclaw_dir(cli, args)?;
    let report = audit_openclaw(&dir)?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("OpenClaw audit — {}", report.openclaw_dir.display());
    println!(
        "total: {} tokens (editable {}, ~{:.1}% of 200k window)",
        report.total_tokens,
        report.editable_tokens,
        (report.total_tokens as f64 / 200_000.0) * 100.0
    );
    println!(
        "skills: {} active, {} archived  ·  mcp: {} servers",
        report.active_skills, report.archived_skills, report.mcp_servers
    );
    println!();

    println!("components:");
    for c in &report.components {
        println!(
            "  {:<32} {:>7} tok  [{}]",
            truncate(&c.name, 32),
            c.tokens,
            openclaw_category_label(c.category)
        );
    }
    println!();

    println!("recommendations:");
    for r in &report.recommendations {
        println!("  [{}] {}", r.action, r.message);
    }
    Ok(())
}

/// Resolve the OpenClaw directory: explicit `--dir` first, then
/// `~/.openclaw`, then `<project>/.openclaw`. Returns the first
/// existing directory.
fn resolve_openclaw_dir(cli: &Cli, args: &OpenclawArgs) -> Result<PathBuf> {
    if let Some(p) = &args.dir {
        if p.is_dir() {
            return Ok(p.clone());
        }
        return Err(anyhow::anyhow!(
            "openclaw dir does not exist: {}",
            p.display()
        ));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home_oc) = expand_user_path("~/.openclaw") {
        candidates.push(home_oc);
    }
    let project = resolve_project_root(cli.project.as_deref());
    candidates.push(project.join(".openclaw"));

    for c in &candidates {
        if c.is_dir() {
            return Ok(c.clone());
        }
    }

    Err(anyhow::anyhow!(
        "no OpenClaw dir found. Tried: {}. Pass --dir to override.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
