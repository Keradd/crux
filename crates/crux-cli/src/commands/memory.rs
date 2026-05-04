//! `crux remember` / `crux recall` / `crux memory ...` — Layer 8 surface.

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, Subcommand};

use std::path::PathBuf;

use crux_core::Runtime;
use crux_l8_memory::{
    export_memory_md, ExportOptions, MemoryEngine, NewObservation, Observation, ObservationKind,
    RankedObservation, RecallQuery,
};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct RememberArgs {
    /// Observation kind: user/feedback/project/reference/guardrail/error_pattern/decision/convention.
    #[arg(short, long)]
    pub kind: String,

    /// Short title (one line).
    #[arg(short, long)]
    pub title: String,

    /// Body / content. If omitted with --stdin, reads from stdin.
    #[arg(short, long)]
    pub content: Option<String>,

    /// Read content from stdin instead of --content.
    #[arg(long)]
    pub stdin: bool,

    /// Optional rationale ("why").
    #[arg(long)]
    pub why: Option<String>,

    /// Optional how-to-apply note.
    #[arg(long = "how")]
    pub how_to_apply: Option<String>,

    /// Optional symbol (qualified name) this observation relates to.
    #[arg(long)]
    pub symbol: Option<String>,

    /// Optional file path it relates to.
    #[arg(long)]
    pub file: Option<String>,

    /// Comma-separated tags.
    #[arg(long, value_delimiter = ',')]
    pub tags: Vec<String>,

    /// 1..=10. Default 5.
    #[arg(long, default_value_t = 5)]
    pub importance: u8,

    /// Mark observation as private (excluded from inter-agent bus).
    #[arg(long)]
    pub private: bool,
}

#[derive(Debug, ClapArgs)]
pub struct RecallArgs {
    /// Free-text query. Empty = list most-relevant.
    #[arg(value_name = "QUERY", default_value = "")]
    pub query: String,

    /// Filter to specific kinds (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub kind: Vec<String>,

    /// Filter to a specific symbol.
    #[arg(long)]
    pub symbol: Option<String>,

    /// Limit results.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,

    /// Include archived observations.
    #[arg(long)]
    pub include_archived: bool,
}

#[derive(Debug, Subcommand)]
pub enum MemoryCmd {
    /// Show available observation kinds.
    Kinds,
    /// List observations for the current project.
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Run the periodic decay pass and report stats.
    Decay,
    /// Archive an observation (it survives but recall skips it).
    Archive {
        #[arg(value_name = "ID")]
        id: i64,
    },
    /// Permanently delete an observation.
    Delete {
        #[arg(value_name = "ID")]
        id: i64,
    },
    /// Render L8 observations to a `MEMORY.md` view that OpenClaw /
    /// Claude Code can pick up at startup. Direction is one-way:
    /// L8 → MEMORY.md. Hand-written files are refused unless `--force`.
    Export {
        /// Target path. Defaults to `<project_root>/MEMORY.md`.
        #[arg(long)]
        target: Option<PathBuf>,
        /// Cap rendered observations. Default = 200.
        #[arg(long)]
        limit: Option<usize>,
        /// Overwrite an existing hand-written `MEMORY.md`. Destructive.
        #[arg(long)]
        force: bool,
    },
}

// ─────────────────────────────────────────────────────────────────────────
// remember
// ─────────────────────────────────────────────────────────────────────────

pub fn run_remember(cli: &Cli, args: &RememberArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project.clone())
    } else {
        None
    };
    let runtime = Runtime::open(project_opt.clone())?;

    let kind: ObservationKind = args.kind.parse().map_err(anyhow::Error::msg)?;
    let content = if args.stdin {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)
            .context("reading content from stdin")?;
        s
    } else {
        args.content
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--content or --stdin required"))?
    };

    let obs = NewObservation {
        project_root: project.display().to_string(),
        session_id: None,
        agent_id: None,
        kind,
        title: args.title.clone(),
        content,
        why: args.why.clone(),
        how_to_apply: args.how_to_apply.clone(),
        symbol: args.symbol.clone(),
        file_path: args.file.clone(),
        tags: args.tags.clone(),
        importance: args.importance,
        private: args.private,
    };

    let mem = MemoryEngine::new(&runtime.conn)?;
    let id = mem.remember(obs)?;

    if cli.json {
        println!("{}", serde_json::json!({"id": id}));
    } else {
        println!("remembered #{} ({})", id, kind.as_str());
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// recall
// ─────────────────────────────────────────────────────────────────────────

pub fn run_recall(cli: &Cli, args: &RecallArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;

    let kinds = parse_kinds(&args.kind)?;
    let q = RecallQuery {
        query: args.query.clone(),
        project_root: Some(project.display().to_string()),
        kinds,
        symbol: args.symbol.clone(),
        file_paths: Vec::new(),
        limit: args.limit,
        include_archived: args.include_archived,
    };

    let mem = MemoryEngine::new(&runtime.conn)?;
    let results = mem.recall(&q)?;

    if cli.json {
        let arr: Vec<_> = results
            .iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("(no observations found)");
        return Ok(());
    }
    for r in &results {
        print_ranked(r);
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// memory subcommands
// ─────────────────────────────────────────────────────────────────────────

pub fn run_memory(cli: &Cli, cmd: &MemoryCmd) -> Result<()> {
    match cmd {
        MemoryCmd::Kinds => kinds(cli),
        MemoryCmd::List { limit } => list(cli, *limit),
        MemoryCmd::Decay => decay(cli),
        MemoryCmd::Archive { id } => archive(cli, *id),
        MemoryCmd::Delete { id } => delete(cli, *id),
        MemoryCmd::Export {
            target,
            limit,
            force,
        } => export(cli, target.clone(), *limit, *force),
    }
}

fn kinds(cli: &Cli) -> Result<()> {
    if cli.json {
        let arr: Vec<_> = ObservationKind::ALL.iter().map(|k| k.as_str()).collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }
    println!("observation kinds:");
    for k in ObservationKind::ALL {
        println!("  {}", k.as_str());
    }
    Ok(())
}

fn list(cli: &Cli, limit: usize) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let mem = MemoryEngine::new(&runtime.conn)?;
    let items = mem.list(&project.display().to_string(), limit)?;
    if cli.json {
        let arr: Vec<_> = items
            .iter()
            .map(|o| serde_json::to_value(o).unwrap())
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }
    if items.is_empty() {
        println!("(no observations)");
        return Ok(());
    }
    for o in &items {
        print_observation(o);
    }
    Ok(())
}

fn decay(cli: &Cli) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let mem = MemoryEngine::new(&runtime.conn)?;
    let stats = mem.decay_pass(chrono::Utc::now().timestamp())?;
    if cli.json {
        println!(
            "{}",
            serde_json::json!({
                "scanned": stats.scanned,
                "updated": stats.updated,
                "archived": stats.archived,
            })
        );
    } else {
        println!(
            "decay pass — scanned: {}, updated: {}, archived: {}",
            stats.scanned, stats.updated, stats.archived
        );
    }
    Ok(())
}

fn archive(cli: &Cli, id: i64) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let mem = MemoryEngine::new(&runtime.conn)?;
    let ok = mem.archive(id)?;
    if cli.json {
        println!("{}", serde_json::json!({"archived": ok}));
    } else if ok {
        println!("archived #{id}");
    } else {
        println!("no observation with id {id}");
    }
    Ok(())
}

fn delete(cli: &Cli, id: i64) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let mem = MemoryEngine::new(&runtime.conn)?;
    let ok = mem.delete(id)?;
    if cli.json {
        println!("{}", serde_json::json!({"deleted": ok}));
    } else if ok {
        println!("deleted #{id}");
    } else {
        println!("no observation with id {id}");
    }
    Ok(())
}

fn export(cli: &Cli, target: Option<PathBuf>, limit: Option<usize>, force: bool) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let mem = MemoryEngine::new(&runtime.conn)?;

    let target_path = target.unwrap_or_else(|| project.join("MEMORY.md"));
    let opts = ExportOptions { limit, force };
    let report = export_memory_md(&mem, &project.display().to_string(), &target_path, &opts)
        .with_context(|| format!("failed to export MEMORY.md to {}", target_path.display()))?;

    if cli.json {
        println!(
            "{}",
            serde_json::json!({
                "target": report.target,
                "observations_rendered": report.observations_rendered,
                "bytes_written": report.bytes_written,
                "unchanged": report.unchanged,
                "refused_due_to_handwritten": report.refused_due_to_handwritten,
            })
        );
        return Ok(());
    }

    if report.refused_due_to_handwritten {
        println!(
            "refused: {} exists and was not generated by crux. Re-run with --force to overwrite.",
            report.target.display()
        );
    } else if report.unchanged {
        println!(
            "{}: unchanged ({} observations, {} bytes already on disk)",
            report.target.display(),
            report.observations_rendered,
            report.bytes_written
        );
    } else {
        println!(
            "wrote {} ({} observations, {} bytes)",
            report.target.display(),
            report.observations_rendered,
            report.bytes_written
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// helpers
// ─────────────────────────────────────────────────────────────────────────

fn parse_kinds(values: &[String]) -> Result<Vec<ObservationKind>> {
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        let k: ObservationKind = v.parse().map_err(anyhow::Error::msg)?;
        out.push(k);
    }
    Ok(out)
}

fn print_ranked(r: &RankedObservation) {
    let o = &r.observation;
    println!(
        "#{:<4} [{}] importance={} score={:.2}",
        o.id,
        o.kind.as_str(),
        o.importance,
        r.score
    );
    println!("  title  : {}", o.title);
    println!("  content: {}", first_line(&o.content));
    if let Some(why) = &o.why {
        println!("  why    : {}", first_line(why));
    }
    if let Some(s) = &o.symbol {
        println!("  symbol : {}", s);
    }
    if !o.tags.is_empty() {
        println!("  tags   : {}", o.tags.join(", "));
    }
    println!();
}

fn print_observation(o: &Observation) {
    println!(
        "#{:<4} [{}] importance={} score={:.2}",
        o.id,
        o.kind.as_str(),
        o.importance,
        o.relevance_score
    );
    println!("  title  : {}", o.title);
    println!("  content: {}", first_line(&o.content));
    println!();
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}
