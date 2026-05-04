use std::collections::BTreeMap;

use anyhow::{anyhow, Context, Result};
use clap::Args as ClapArgs;

use crux_l10_setup::setup::{
    auto_detect, integrate, Action, AgentKind, IntegrateOptions, IntegrateReport, Scope,
};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(value_name = "AGENT")]
    pub agents: Vec<String>,

    #[arg(long)]
    pub list: bool,

    #[arg(long)]
    pub dry_run: bool,

    #[arg(long, value_parser = parse_scope, default_value = "auto")]
    pub scope: Scope,

    #[arg(long, value_name = "PATH")]
    pub crux_path: Option<String>,

    #[arg(long)]
    pub no_hooks: bool,

    #[arg(long)]
    pub no_skill: bool,

    #[arg(long)]
    pub enable_hygiene_hook: bool,

    #[arg(long, conflicts_with = "enable_hygiene_hook")]
    pub disable_hygiene_hook: bool,

    #[arg(long)]
    pub no_project_env: bool,

    #[arg(long = "env", value_name = "KEY=VAL", value_parser = parse_env_kv)]
    pub envs: Vec<(String, String)>,

    #[arg(long)]
    pub force: bool,
}

fn parse_env_kv(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=VAL, got '{s}'"))?;
    let k = k.trim();
    if k.is_empty() {
        return Err(format!("empty env key in '{s}'"));
    }
    Ok((k.to_string(), v.to_string()))
}

fn parse_scope(s: &str) -> Result<Scope, String> {
    match s.to_ascii_lowercase().as_str() {
        "global" | "user" | "home" => Ok(Scope::Global),
        "project" | "local" => Ok(Scope::Project),
        "auto" => Ok(Scope::Auto),
        other => Err(format!(
            "unknown scope: {other} (expected global|project|auto)"
        )),
    }
}

pub fn run(cli: &Cli, args: &Args) -> Result<()> {
    if args.list {
        return print_agent_list(cli.json);
    }

    let project_root = resolve_project_root(cli.project.as_deref());
    let crux_path = args
        .crux_path
        .clone()
        .unwrap_or_else(crux_l10_setup::setup::default_crux_path);

    let agents: Vec<AgentKind> = if args.agents.is_empty() {
        let found = auto_detect();
        if found.is_empty() {
            return Err(anyhow!(
                "no supported agent detected on this machine. Pass an explicit agent name (see `crux setup --list`)."
            ));
        }
        found
    } else {
        let mut out = Vec::new();
        for raw in &args.agents {
            let kind = AgentKind::parse(raw).ok_or_else(|| {
                anyhow!("unknown agent '{raw}' (see `crux setup --list` for supported names)")
            })?;
            if !out.contains(&kind) {
                out.push(kind);
            }
        }
        out
    };

    let mut env: BTreeMap<String, String> = BTreeMap::new();
    if !args.no_project_env {
        env.insert(
            "CRUX_PROJECT".to_string(),
            project_root.display().to_string(),
        );
    }
    for (k, v) in &args.envs {
        env.insert(k.clone(), v.clone());
    }

    let mut reports: Vec<IntegrateReport> = Vec::with_capacity(agents.len());
    for agent in &agents {
        let opts = IntegrateOptions {
            agent: *agent,
            scope: args.scope,
            project_root: project_root.clone(),
            crux_path: crux_path.clone(),
            env: env.clone(),
            install_hooks: agent.supports_hooks() && !args.no_hooks,
            install_skill: agent.supports_slash_command() && !args.no_skill,
            install_hygiene_hook: agent.supports_hooks() && args.enable_hygiene_hook,
            remove_hygiene_hook: agent.supports_hooks() && args.disable_hygiene_hook,
            dry_run: args.dry_run,
            force: args.force,
        };
        let report = integrate(&opts).with_context(|| format!("integrating {}", agent.label()))?;
        reports.push(report);
    }

    if cli.json {
        emit_json(&reports, args.dry_run);
    } else {
        emit_human(&reports, args.dry_run);
    }
    Ok(())
}

fn print_agent_list(json: bool) -> Result<()> {
    if json {
        let payload: Vec<serde_json::Value> = AgentKind::all()
            .iter()
            .map(|k| {
                serde_json::json!({
                    "slug": k.slug(),
                    "label": k.label(),
                    "supports_hooks": k.supports_hooks(),
                    "supports_slash_command": k.supports_slash_command(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    println!("Supported agents:");
    for k in AgentKind::all() {
        let extras = match (k.supports_hooks(), k.supports_slash_command()) {
            (true, true) => " (hooks + /crux skill)",
            (true, false) => " (hooks)",
            (false, true) => " (skill)",
            _ => "",
        };
        println!("  {:<16} {}{}", k.slug(), k.label(), extras);
    }
    println!();
    println!("Run `crux setup <agent>` to integrate a specific one,");
    println!("or `crux setup` (no arg) to auto-detect every installed agent.");
    Ok(())
}

fn emit_human(reports: &[IntegrateReport], dry_run: bool) {
    let prefix = if dry_run { "[dry-run] " } else { "" };
    for report in reports {
        println!("{prefix}{}:", report.agent);
        for action in &report.actions {
            match action {
                Action::Created(p) => println!("  + {}", p.display()),
                Action::Updated(p) => println!("  ~ {}", p.display()),
                Action::Skipped { path, reason } => {
                    println!("  · {} ({reason})", path.display());
                }
                Action::Note(s) => println!("  ! {s}"),
            }
        }
        println!();
    }
    if dry_run {
        println!("dry-run: no files were modified. Re-run without --dry-run to apply.");
    } else {
        println!(
            "Done. Restart your agent (or run `claude mcp list` / equivalent) to pick up the new MCP server."
        );
    }
}

fn emit_json(reports: &[IntegrateReport], dry_run: bool) {
    let payload = serde_json::json!({
        "dry_run": dry_run,
        "reports": reports
            .iter()
            .map(|r| serde_json::json!({
                "agent": r.agent,
                "changed": r.changed(),
                "actions": r.actions.iter().map(|a| match a {
                    Action::Created(p) => serde_json::json!({ "kind": "created", "path": p.display().to_string() }),
                    Action::Updated(p) => serde_json::json!({ "kind": "updated", "path": p.display().to_string() }),
                    Action::Skipped { path, reason } => serde_json::json!({
                        "kind": "skipped",
                        "path": path.display().to_string(),
                        "reason": *reason,
                    }),
                    Action::Note(s) => serde_json::json!({ "kind": "note", "message": s }),
                }).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&payload).unwrap());
}
