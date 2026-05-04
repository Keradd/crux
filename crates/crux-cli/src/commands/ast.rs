use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

use crux_core::Runtime;
use crux_l5_ast::{index_project_with, GraphStore, NodeKind};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct IndexArgs {
    #[arg(long)]
    pub dir: Option<PathBuf>,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, ClapArgs)]
pub struct FindArgs {
    #[arg(value_name = "QUERY")]
    pub query: String,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long, default_value_t = 30)]
    pub limit: usize,
    #[arg(long)]
    pub exact: bool,
}

#[derive(Debug, ClapArgs)]
pub struct SymbolArgs {
    #[arg(value_name = "QUALIFIED_NAME")]
    pub qn: String,
    #[arg(long)]
    pub source: bool,
}

#[derive(Debug, ClapArgs)]
pub struct ImpactArgs {
    #[arg(value_name = "QUALIFIED_NAME")]
    pub qn: String,
    #[arg(long, default_value_t = 2)]
    pub depth: u32,
    #[arg(long, default_value_t = 100)]
    pub max: u32,
}

pub fn run_index(cli: &Cli, args: &IndexArgs) -> Result<()> {
    let project = args
        .dir
        .clone()
        .unwrap_or_else(|| resolve_project_root(cli.project.as_deref()));
    if !project.is_dir() {
        return Err(anyhow::anyhow!(
            "project root not a directory: {}",
            project.display()
        ));
    }
    let runtime = Runtime::open(Some(project.clone()))?;
    let stats = index_project_with(&runtime.conn, &project, args.force)?;
    if cli.json {
        let payload = serde_json::json!({
            "project":         project.display().to_string(),
            "files_scanned":   stats.files_scanned,
            "files_skipped":   stats.files_skipped,
            "files_unchanged": stats.files_unchanged,
            "files_removed":   stats.files_removed,
            "nodes_upserted":  stats.nodes_upserted,
            "edges_upserted":  stats.edges_upserted,
            "forced":          args.force,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "indexed {}: {} re-parsed (+{} skipped, ={} unchanged, -{} removed), {} nodes, {} edges",
            project.display(),
            stats.files_scanned,
            stats.files_skipped,
            stats.files_unchanged,
            stats.files_removed,
            stats.nodes_upserted,
            stats.edges_upserted
        );
    }
    Ok(())
}

pub fn run_find(cli: &Cli, args: &FindArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let store = GraphStore::new(&runtime.conn);
    let key = project.display().to_string();

    let kind: Option<NodeKind> = args
        .kind
        .as_deref()
        .map(|s| s.parse::<NodeKind>())
        .transpose()
        .map_err(anyhow::Error::msg)?;

    let nodes = if args.exact {
        store.find_symbol(&key, &args.query, kind)?
    } else {
        store.find_symbol_like(&key, &args.query, args.limit)?
    };

    if cli.json {
        let arr: Vec<_> = nodes
            .iter()
            .map(|n| serde_json::to_value(n).unwrap())
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }
    if nodes.is_empty() {
        println!("(no matches — run `crux index` first if you haven't)");
        return Ok(());
    }
    for n in &nodes {
        println!(
            "{:<8} {} ({}:{}-{})",
            n.kind.as_str(),
            n.qualified_name,
            n.file_path,
            n.line_start,
            n.line_end,
        );
        if let Some(sig) = &n.signature {
            println!("  {}", sig.lines().next().unwrap_or(""));
        }
    }
    Ok(())
}

pub fn run_symbol(cli: &Cli, args: &SymbolArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let store = GraphStore::new(&runtime.conn);
    let key = project.display().to_string();
    let n = store
        .get_by_qn(&key, &args.qn)?
        .ok_or_else(|| anyhow::anyhow!("symbol '{}' not found", args.qn))?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&n)?);
        return Ok(());
    }
    println!(
        "{} {}\n  file: {}\n  lines: {}-{}",
        n.kind.as_str(),
        n.qualified_name,
        n.file_path,
        n.line_start,
        n.line_end
    );
    if let Some(sig) = &n.signature {
        println!("  signature: {}", sig.lines().next().unwrap_or(""));
    }
    if args.source {
        let abs = project.join(&n.file_path);
        let content =
            std::fs::read_to_string(&abs).with_context(|| format!("read {}", abs.display()))?;
        let lines: Vec<&str> = content.lines().collect();
        let lo = (n.line_start.saturating_sub(1)) as usize;
        let hi = (n.line_end as usize).min(lines.len());
        println!();
        for (i, line) in lines[lo..hi].iter().enumerate() {
            println!("{:>5}  {}", lo + i + 1, line);
        }
    }
    Ok(())
}

pub fn run_impact(cli: &Cli, args: &ImpactArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let store = GraphStore::new(&runtime.conn);
    let key = project.display().to_string();
    let nodes = store.impact_radius(&key, &args.qn, args.depth, args.max)?;

    if cli.json {
        let arr: Vec<_> = nodes
            .iter()
            .map(|n| serde_json::to_value(n).unwrap())
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }
    if nodes.is_empty() {
        println!(
            "{} has no callers within depth {} (or symbol does not exist)",
            args.qn, args.depth
        );
        return Ok(());
    }
    println!(
        "blast radius from {} (depth ≤ {}, max {} nodes):",
        args.qn, args.depth, args.max
    );
    for n in &nodes {
        println!(
            "  {} {} ({}:{})",
            n.kind.as_str(),
            n.qualified_name,
            n.file_path,
            n.line_start
        );
    }
    Ok(())
}
