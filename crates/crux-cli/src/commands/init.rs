use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use dialoguer::{theme::ColorfulTheme, Input, Select};

use crux_l10_setup::setup::{
    auto_detect as detect_agents, default_crux_path, integrate, AgentKind, IntegrateOptions,
    IntegrateReport, Scope,
};
use crux_l10_setup::{init, profiles, InitOptions};

use super::{ast, resolve_project_root, search};
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long)]
    pub profile: Option<String>,

    #[arg(long)]
    pub project_type: Option<String>,

    #[arg(long)]
    pub stack: Option<String>,

    #[arg(long)]
    pub features: Option<String>,

    #[arg(long)]
    pub non_interactive: bool,

    #[arg(long)]
    pub force: bool,

    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    #[arg(long)]
    pub setup_agents: bool,

    #[arg(long = "agents", value_name = "AGENT")]
    pub agents: Vec<String>,

    #[arg(long)]
    pub index: bool,
}

pub fn run(cli: &Cli, args: &Args) -> Result<()> {
    let root = args
        .dir
        .clone()
        .unwrap_or_else(|| resolve_project_root(cli.project.as_deref()));

    let profile = if let Some(p) = &args.profile {
        p.clone()
    } else if args.non_interactive {
        "coding".into()
    } else {
        prompt_profile()?
    };

    let stack = match (args.stack.clone(), args.non_interactive) {
        (Some(s), _) => Some(s),
        (None, true) => None,
        (None, false) => prompt_optional("Stack (e.g. Rust+Tokio+SQLite)", "")?,
    };
    let features = match (args.features.clone(), args.non_interactive) {
        (Some(s), _) => Some(s),
        (None, true) => None,
        (None, false) => prompt_optional("Main features (one line)", "")?,
    };

    let opts = InitOptions {
        project_root: root.clone(),
        profile: profile.clone(),
        project_type: args.project_type.clone(),
        stack,
        features,
        force: args.force,
    };
    let report = init(&opts).context("scaffolding project")?;

    if cli.json {
        let payload = serde_json::json!({
            "project_root": root.display().to_string(),
            "project_type": report.project_type.label(),
            "profile": report.profile,
            "written": report.written.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "skipped": report.skipped.iter().map(|(p, why)| {
                serde_json::json!({"path": p.display().to_string(), "why": *why})
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("crux init: {}", root.display());
    println!("  type   : {}", report.project_type.label());
    println!("  profile: {}", report.profile);
    println!();
    if !report.written.is_empty() {
        println!("written:");
        for p in &report.written {
            println!("  + {}", display_relative(&root, p));
        }
    }
    if !report.skipped.is_empty() {
        println!("skipped:");
        for (p, why) in &report.skipped {
            println!("  · {} ({})", display_relative(&root, p), why);
        }
        println!();
        println!("re-run with --force to overwrite skipped files.");
    }

    if args.setup_agents {
        println!();
        println!("registering MCP integrations…");
        run_setup_agents_chain(cli, &root, &args.agents).context("chaining crux setup")?;
    }

    if args.index {
        println!();
        println!("running first-time index (L5 AST + L6 chunks)…");
        let idx = ast::IndexArgs {
            dir: Some(root.clone()),
            force: false,
        };
        ast::run_index(cli, &idx).context("chaining crux index")?;
        let rix = search::ReindexArgs {
            dir: Some(root.clone()),
            ..search::ReindexArgs::default()
        };
        search::run_reindex(cli, &rix).context("chaining crux reindex")?;
    }

    Ok(())
}

fn run_setup_agents_chain(
    cli: &Cli,
    project_root: &std::path::Path,
    explicit: &[String],
) -> Result<()> {
    let agents: Vec<AgentKind> = if explicit.is_empty() {
        let found = detect_agents();
        if found.is_empty() {
            println!("  · no supported agent detected — skipping MCP registration");
            return Ok(());
        }
        found
    } else {
        let mut out = Vec::new();
        for raw in explicit {
            let kind = AgentKind::parse(raw).ok_or_else(|| {
                anyhow::anyhow!("unknown agent '{raw}' (see `crux setup --list`)")
            })?;
            if !out.contains(&kind) {
                out.push(kind);
            }
        }
        out
    };

    let crux_path = default_crux_path();
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert(
        "CRUX_PROJECT".to_string(),
        project_root.display().to_string(),
    );

    let mut reports: Vec<IntegrateReport> = Vec::with_capacity(agents.len());
    for agent in &agents {
        let opts = IntegrateOptions {
            agent: *agent,
            scope: Scope::Auto,
            project_root: project_root.to_path_buf(),
            crux_path: crux_path.clone(),
            env: env.clone(),
            install_hooks: agent.supports_hooks(),
            install_skill: agent.supports_slash_command(),
            install_hygiene_hook: false,
            remove_hygiene_hook: false,
            dry_run: false,
            force: false,
        };
        let report = integrate(&opts).with_context(|| format!("integrating {}", agent.label()))?;
        reports.push(report);
    }

    if cli.json {
        let payload = serde_json::json!({
            "setup_chain": reports
                .iter()
                .map(|r| serde_json::json!({
                    "agent":   r.agent,
                    "changed": r.changed(),
                }))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        for r in &reports {
            let state = if r.changed() { "updated" } else { "unchanged" };
            println!("  {} {}", r.agent, state);
        }
    }
    Ok(())
}

fn prompt_profile() -> Result<String> {
    let names: Vec<&str> = profiles::ALL.iter().map(|p| p.name).collect();
    let descs: Vec<String> = profiles::ALL
        .iter()
        .map(|p| format!("{:<10} — {}", p.name, p.description))
        .collect();

    let idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Profile")
        .items(&descs)
        .default(0)
        .interact()
        .context("profile prompt")?;
    Ok(names[idx].to_string())
}

fn prompt_optional(label: &str, default: &str) -> Result<Option<String>> {
    let v: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt(label)
        .default(default.to_string())
        .allow_empty(true)
        .interact_text()
        .context("input prompt")?;
    Ok(if v.trim().is_empty() { None } else { Some(v) })
}

fn display_relative(base: &std::path::Path, p: &std::path::Path) -> String {
    p.strip_prefix(base)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}
