use anyhow::Result;
use clap::Subcommand;

use crux_core::config;

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    Show,
    Paths,
    Validate,
}

pub fn run(cli: &Cli, cmd: &Cmd) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt: Option<&std::path::Path> = if project.join(".crux").is_dir() {
        Some(&project)
    } else {
        None
    };

    match cmd {
        Cmd::Show => show(cli, project_opt),
        Cmd::Paths => paths(cli, project_opt),
        Cmd::Validate => validate(cli, project_opt),
    }
}

fn show(cli: &Cli, project: Option<&std::path::Path>) -> Result<()> {
    let loaded = config::load(project)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&loaded.config)?);
    } else {
        let s = toml::to_string_pretty(&loaded.config)?;
        println!("# global = {}", loaded.global_path.display());
        if let Some(pp) = &loaded.project_path {
            println!("# project = {}", pp.display());
        }
        println!();
        println!("{}", s);
    }
    Ok(())
}

fn paths(cli: &Cli, project: Option<&std::path::Path>) -> Result<()> {
    let loaded = config::load(project)?;
    let crux_home = crux_core::paths::crux_home()?;
    let db = loaded
        .config
        .general
        .db_path
        .clone()
        .unwrap_or(crux_core::paths::db_path()?);

    if cli.json {
        let payload = serde_json::json!({
            "crux_home": crux_home.display().to_string(),
            "global_config": loaded.global_path.display().to_string(),
            "project_config": loaded.project_path.map(|p| p.display().to_string()),
            "db": db.display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("crux_home      : {}", crux_home.display());
    println!("global config  : {}", loaded.global_path.display());
    match loaded.project_path {
        Some(p) => println!("project config : {}", p.display()),
        None => println!("project config : (none)"),
    }
    println!("database       : {}", db.display());
    Ok(())
}

fn validate(_cli: &Cli, project: Option<&std::path::Path>) -> Result<()> {
    let _ = config::load(project)?;
    println!("ok");
    Ok(())
}
