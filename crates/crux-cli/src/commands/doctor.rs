//! `crux doctor` — diagnose the local installation.

use anyhow::Result;

use crux_core::{config, paths, Runtime};

use super::resolve_project_root;
use crate::Cli;

pub fn run(cli: &Cli) -> Result<()> {
    let mut ok = true;

    let crux_home = paths::crux_home()?;
    println!("crux_home : {}", crux_home.display());
    if !crux_home.exists() {
        println!("  (will be created on first write)");
    }

    let global = paths::global_config_path()?;
    println!(
        "global cfg: {}{}",
        global.display(),
        if global.exists() { "" } else { "  (missing)" }
    );

    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project.clone())
    } else {
        None
    };
    if let Some(p) = &project_opt {
        println!("project   : {}", p.display());
    } else {
        println!("project   : (none — run `crux init` to set up)");
    }

    // Try to load config
    match config::load(project_opt.as_deref()) {
        Ok(_) => println!("config    : ok"),
        Err(e) => {
            ok = false;
            println!("config    : ERROR — {}", e);
        }
    }

    // Try to open the DB
    match Runtime::open(project_opt.clone()) {
        Ok(rt) => {
            let db_path = rt
                .config
                .general
                .db_path
                .clone()
                .unwrap_or(paths::db_path()?);
            println!("database  : ok at {}", db_path.display());
        }
        Err(e) => {
            ok = false;
            println!("database  : ERROR — {}", e);
        }
    }

    if ok {
        println!("\nall checks passed.");
    } else {
        println!("\nfailures found — fix the errors above and re-run.");
        std::process::exit(1);
    }

    Ok(())
}
