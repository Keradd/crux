//! `crux execute` — Layer 7 sandbox executor.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

use crux_l7_sandbox::{
    agent_perms, ExecRequest, Executor, IsolationLevel, Permissions, RuntimeKind,
};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct ExecuteArgs {
    /// Runtime kind: python | bash | node.
    #[arg(long, default_value = "bash")]
    pub runtime: String,

    /// Inline code to run. Mutually exclusive with `--file` and stdin.
    #[arg(long, short = 'c')]
    pub code: Option<String>,

    /// Read code from this file instead of `--code`.
    #[arg(long, short = 'f', value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Per-execution wall-clock timeout in seconds.
    #[arg(long, default_value_t = 10)]
    pub timeout: u64,

    /// Maximum bytes captured from stdout / stderr each.
    #[arg(long, default_value_t = 65_536)]
    pub max_output_bytes: usize,

    /// Inherit the parent's full environment instead of the scrubbed default.
    #[arg(long)]
    pub inherit_env: bool,

    /// Extra `KEY=VALUE` env entries (repeatable).
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,

    /// Isolation level: `portable` (default) or `hard`. `hard` layers
    /// Linux-only `setrlimit` caps and, when the binary was built with
    /// the `landlock` feature, filesystem confinement on top of the
    /// portable guarantees. Non-Linux targets fall back to portable.
    #[arg(long, default_value = "portable")]
    pub isolate: String,

    /// Load `~/.claude/settings.json` + `~/.openclaw/openclaw.json`
    /// (plus their per-project equivalents) and refuse to spawn if the
    /// runtime + code body matches any unioned `Bash(...)` / `exec`
    /// deny rule. Off by default to preserve the legacy contract;
    /// opt in for parity with how Claude Code / OpenClaw enforce
    /// `permissions.deny` on tool calls.
    #[arg(long = "check-agent-perms")]
    pub check_agent_perms: bool,
}

pub fn run(cli: &Cli, args: &ExecuteArgs) -> Result<()> {
    let runtime = RuntimeKind::parse(&args.runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown --runtime '{}' (want python | bash | node)",
            args.runtime
        )
    })?;

    let code = match (&args.code, &args.file) {
        (Some(c), None) => c.clone(),
        (None, Some(p)) => {
            std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?
        }
        (None, None) => read_stdin()?,
        (Some(_), Some(_)) => {
            return Err(anyhow::anyhow!("pass either --code or --file, not both"));
        }
    };
    if code.trim().is_empty() {
        return Err(anyhow::anyhow!("no code to execute (empty input)"));
    }

    let project = resolve_project_root(cli.project.as_deref());

    let mut env: HashMap<String, String> = HashMap::new();
    for kv in &args.env {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--env expects KEY=VALUE, got '{}'", kv))?;
        env.insert(k.to_string(), v.to_string());
    }

    let isolation = IsolationLevel::parse(&args.isolate).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown --isolate '{}' (want portable | hard)",
            args.isolate
        )
    })?;

    let permissions: Option<Permissions> = if args.check_agent_perms {
        Some(agent_perms::load_for_project(Some(&project)))
    } else {
        None
    };

    let req = ExecRequest {
        runtime,
        code,
        project_root: Some(project),
        timeout: Duration::from_secs(args.timeout),
        max_output_bytes: args.max_output_bytes,
        env,
        inherit_env: args.inherit_env,
        isolation,
        permissions,
    };

    let exec = Executor::new();
    let res = exec.execute(&req)?;

    if cli.json {
        let payload = serde_json::json!({
            "runtime":            res.runtime.as_str(),
            "exit_code":          res.exit_code,
            "timed_out":          res.timed_out,
            "stdout":             res.stdout,
            "stderr":             res.stderr,
            "stdout_truncated":   res.stdout_truncated,
            "stderr_truncated":   res.stderr_truncated,
            "elapsed_ms":         res.elapsed_ms,
            "isolation":          isolation.as_str(),
            "isolation_applied":  res.isolation_applied,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        if !res.stdout.is_empty() {
            print!("{}", res.stdout);
            if !res.stdout.ends_with('\n') {
                println!();
            }
        }
        if !res.stderr.is_empty() {
            eprint!("{}", res.stderr);
            if !res.stderr.ends_with('\n') {
                eprintln!();
            }
        }
        if res.timed_out {
            eprintln!("[crux] timed out after {}s", args.timeout);
        }
        if res.stdout_truncated {
            eprintln!("[crux] stdout truncated at {} bytes", args.max_output_bytes);
        }
        if res.stderr_truncated {
            eprintln!("[crux] stderr truncated at {} bytes", args.max_output_bytes);
        }
    }

    if res.timed_out {
        std::process::exit(124);
    }
    if let Some(code) = res.exit_code {
        if code != 0 {
            std::process::exit(code);
        }
    }
    Ok(())
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("read stdin")?;
    Ok(buf)
}
