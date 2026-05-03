//! `crux init` — Layer 10 scaffolding.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use dialoguer::{theme::ColorfulTheme, Input, Select};

use crux_l10_setup::{init, profiles, InitOptions};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Profile name (coding/analysis/agents). Default: coding.
    #[arg(long)]
    pub profile: Option<String>,

    /// Project type label (free text). Default: autodetected.
    #[arg(long)]
    pub project_type: Option<String>,

    /// Stack description.
    #[arg(long)]
    pub stack: Option<String>,

    /// Features description.
    #[arg(long)]
    pub features: Option<String>,

    /// Skip interactive prompts; use defaults / flags.
    #[arg(long)]
    pub non_interactive: bool,

    /// Overwrite existing scaffolded files.
    #[arg(long)]
    pub force: bool,

    /// Project directory (defaults to autodetect from cwd).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
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
