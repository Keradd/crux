use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

use crux_l12_hygiene::{scan_comments, HygieneOptions};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "PATH")]
    pub root: Option<PathBuf>,

    #[arg(long)]
    pub skip_hygiene: bool,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cargo_args: Vec<String>,
}

pub fn run(cli: &Cli, args: &Args) -> Result<()> {
    let cargo_root = resolve_cargo_root(cli, args)?;

    if !args.skip_hygiene {
        let options = HygieneOptions::for_root(&cargo_root);
        let report = scan_comments(&cargo_root, &options).context("running hygiene scan")?;
        if !report.is_clean() {
            eprintln!(
                "crux build: hygiene check failed ({} violation(s) across {} file(s)).",
                report.violation_count(),
                report.files_with_violations
            );
            for v in report.violations.iter().take(10) {
                eprintln!(
                    "  {}:{} [{}] {}",
                    v.file.display(),
                    v.line,
                    v.rule_id,
                    v.reason
                );
            }
            if report.violations.len() > 10 {
                eprintln!("  ... and {} more", report.violations.len() - 10);
            }
            eprintln!();
            eprintln!(
                "run `crux hygiene comments --fix` or `crux hygiene comments --strip` first, \
                 or pass `--skip-hygiene` to bypass."
            );
            std::process::exit(1);
        }
        println!(
            "crux build: hygiene clean ({} files scanned). running `cargo build`...",
            report.files_scanned
        );
    } else {
        eprintln!("crux build: --skip-hygiene set; bypassing L12 check.");
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("build").current_dir(&cargo_root);
    for a in &args.cargo_args {
        cmd.arg(a);
    }
    let status = cmd
        .status()
        .with_context(|| format!("invoking `cargo build` in {}", cargo_root.display()))?;
    let code = status.code().unwrap_or(1);
    std::process::exit(code);
}

fn resolve_cargo_root(cli: &Cli, args: &Args) -> Result<PathBuf> {
    if let Some(p) = &args.root {
        return Ok(p.clone());
    }
    let cwd = std::env::current_dir().context("getting current directory")?;
    if let Some(root) = find_cargo_root(&cwd) {
        return Ok(root);
    }
    Ok(resolve_project_root(cli.project.as_deref()))
}

fn find_cargo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    loop {
        if cur.join("Cargo.toml").is_file() {
            return Some(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return None,
        }
    }
}
