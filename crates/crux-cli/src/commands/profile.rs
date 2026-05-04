use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use clap::Subcommand;

use crux_core::config;
use crux_l10_setup::{profiles, templates};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    List,
    Show {
        #[arg(value_name = "NAME")]
        name: String,
    },
    Apply {
        #[arg(value_name = "NAME")]
        name: String,
    },
    Current,
}

pub fn run(cli: &Cli, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::List => list(cli),
        Cmd::Show { name } => show(cli, name),
        Cmd::Apply { name } => apply(cli, name),
        Cmd::Current => current(cli),
    }
}

fn list(cli: &Cli) -> Result<()> {
    if cli.json {
        let arr: Vec<_> = profiles::ALL
            .iter()
            .map(|p| serde_json::json!({"name": p.name, "description": p.description}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }
    println!("available profiles:");
    for p in profiles::ALL {
        println!("  {:<10} — {}", p.name, p.description);
    }
    Ok(())
}

fn show(_cli: &Cli, name: &str) -> Result<()> {
    let p = profiles::get(name).ok_or_else(|| anyhow::anyhow!("unknown profile: {name}"))?;
    println!("# profile: {} — {}\n", p.name, p.description);
    println!("{}", p.claude_md);
    Ok(())
}

fn apply(cli: &Cli, name: &str) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    if !project.join(".crux").is_dir() {
        return Err(anyhow::anyhow!(
            "no .crux/ directory found in {} — run `crux init` first",
            project.display()
        ));
    }

    let profile = profiles::get(name).ok_or_else(|| anyhow::anyhow!("unknown profile: {name}"))?;

    let claude_path = project.join("CLAUDE.md");
    let (project_type, stack, features) = parse_existing_meta(&claude_path);

    let meta = templates::ProjectMeta {
        project_type: project_type.as_deref().unwrap_or("(unspecified)"),
        stack: stack.as_deref().unwrap_or("(unspecified)"),
        features: features
            .as_deref()
            .unwrap_or("(describe project features here)"),
        profile_name: profile.name,
    };
    let rendered = templates::render_claude_md(&meta, profile.claude_md);
    fs::write(&claude_path, &rendered)
        .with_context(|| format!("writing {}", claude_path.display()))?;

    let cfg_path = project.join(".crux/config.toml");
    let mut cfg = config::load(Some(&project))?.config;
    cfg.layer.l1.profile = profile.name.into();
    config::save(&cfg, &cfg_path)?;

    if cli.json {
        let payload = serde_json::json!({
            "applied": profile.name,
            "claude_md": claude_path.display().to_string(),
            "config": cfg_path.display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("applied profile: {}", profile.name);
        println!("  rewrote {}", claude_path.display());
        println!("  updated {}", cfg_path.display());
    }
    Ok(())
}

fn current(cli: &Cli) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt: Option<&Path> = if project.join(".crux").is_dir() {
        Some(&project)
    } else {
        None
    };
    let cfg = config::load(project_opt)?.config;
    let name = cfg.layer.l1.profile.clone();
    let desc = profiles::get(&name)
        .map(|p| p.description)
        .unwrap_or("(custom)");
    if cli.json {
        let payload = serde_json::json!({"profile": name, "description": desc});
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("{} — {}", name, desc);
    }
    Ok(())
}

fn parse_existing_meta(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let Ok(content) = fs::read_to_string(path) else {
        return (None, None, None);
    };
    let mut t = None;
    let mut s = None;
    let mut f = None;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("- **Type**: ") {
            t = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("- **Stack**: ") {
            s = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("- **Features**: ") {
            f = Some(rest.trim().to_string());
        }
    }
    (t, s, f)
}
