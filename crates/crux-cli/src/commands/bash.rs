use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

use crux_core::{telemetry, tokens, LayerMode, Runtime};
use crux_l3_bash::{FilterEngine, OutputKind};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long)]
    pub filter_only: bool,

    #[arg(long)]
    pub passthrough: bool,

    #[arg(long)]
    pub dry_run: bool,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    pub cmd: Vec<String>,
}

pub fn run(cli: &Cli, args: &Args) -> Result<()> {
    if args.cmd.is_empty() {
        return Err(anyhow::anyhow!("usage: crux bash <command> [args...]"));
    }

    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project.clone())
    } else {
        None
    };
    let runtime = Runtime::open(project_opt.clone())?;

    if !runtime.config.layers.l3_bash_filter && !args.filter_only {
        return passthrough_exec(&args.cmd);
    }

    let engine = FilterEngine::builtin().context("loading built-in filters")?;
    let command_line = args.cmd.join(" ");

    if args.filter_only {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        let result = engine.process(&command_line, &input);
        emit_filtered(
            cli,
            &runtime,
            &command_line,
            &input,
            &result,
            project_opt.as_deref(),
            0,
            args.dry_run,
        )?;
        return Ok(());
    }

    if args.passthrough {
        return passthrough_exec(&args.cmd);
    }

    let started = Instant::now();
    let mut child = Command::new(&args.cmd[0])
        .args(&args.cmd[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawn `{}`", args.cmd[0]))?;

    let mut raw = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut raw)?;
    }
    let status = child.wait()?;
    let elapsed_ms = started.elapsed().as_millis() as i64;

    let result = engine.process(&command_line, &raw);
    let mode = runtime.config.modes.l3_bash_filter;

    let to_print = match mode {
        LayerMode::Shadow => raw.clone(),
        _ => result.output.text.clone(),
    };
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(to_print.as_bytes())?;
    if !to_print.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    drop(stdout);

    if !args.dry_run {
        record_telemetry(
            &runtime,
            &command_line,
            &raw,
            &result,
            project_opt.as_deref(),
            elapsed_ms,
        )?;
    }
    let _ = cli; // silence unused; flag plumbing lands later

    if let Some(code) = status.code() {
        std::process::exit(code);
    } else {
        std::process::exit(1);
    }
}

fn passthrough_exec(cmd: &[String]) -> Result<()> {
    let status = Command::new(&cmd[0]).args(&cmd[1..]).status()?;
    std::process::exit(status.code().unwrap_or(1));
}

#[allow(clippy::too_many_arguments)]
fn emit_filtered(
    _cli: &Cli,
    runtime: &Runtime,
    command_line: &str,
    raw: &str,
    result: &crux_l3_bash::ProcessResult,
    project_opt: Option<&std::path::Path>,
    elapsed_ms: i64,
    dry_run: bool,
) -> Result<()> {
    let mode = runtime.config.modes.l3_bash_filter;
    let to_print = match mode {
        LayerMode::Shadow => raw.to_string(),
        _ => result.output.text.clone(),
    };
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(to_print.as_bytes())?;
    if !to_print.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    drop(stdout);

    if !dry_run {
        record_telemetry(runtime, command_line, raw, result, project_opt, elapsed_ms)?;
    }
    Ok(())
}

fn record_telemetry(
    runtime: &Runtime,
    command_line: &str,
    raw: &str,
    result: &crux_l3_bash::ProcessResult,
    project_opt: Option<&std::path::Path>,
    elapsed_ms: i64,
) -> Result<()> {
    let project_pr = project_opt.map(|p| p.display().to_string());
    let original = tokens::estimate(raw) as i64;
    let compressed = tokens::estimate(&result.output.text) as i64;
    let feature = match &result.filter_name {
        Some(n) => format!("bash:{}", n),
        None => "bash:passthrough".to_string(),
    };
    let detail = match &result.output.kind {
        OutputKind::Matched(_) => "matched",
        OutputKind::OnEmpty => "on_empty",
        OutputKind::Filtered => "filtered",
        OutputKind::Passthrough => "passthrough",
    };
    let _ = telemetry::record(
        &runtime.conn,
        &telemetry::Event {
            project_root: project_pr.as_deref(),
            layer: "l3",
            feature: &feature,
            agent_id: None,
            session_id: None,
            command_pattern: Some(first_word(command_line)),
            original_tokens: original,
            compressed_tokens: compressed,
            exec_time_ms: Some(elapsed_ms),
            quality_preserved: true,
            detail: Some(detail),
        },
    );
    Ok(())
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_word_handles_empty() {
        assert_eq!(first_word(""), "");
        assert_eq!(first_word("git status"), "git");
        assert_eq!(first_word("  cargo  build "), "cargo");
    }
}
