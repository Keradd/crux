//! `crux` — command-line entry point.
//!
//! Phase-1 commands implemented:
//!   - `crux init`        — scaffold a project (Layer 10)
//!   - `crux profile …`   — list profiles
//!   - `crux config …`    — show / locate config
//!   - `crux audit`       — health snapshot (placeholder, mostly Layer 9)
//!   - `crux stats`       — per-layer telemetry summary
//!   - `crux hook pre-tool` — PreToolUse hook (Layer 4 read-cache routing)
//!   - `crux doctor`      — diagnose setup
//!   - `crux version`
//!
//! Stubs that return "not implemented" with hints for later phases:
//!   - `mcp`, `mcp-shrink`, `bash`, `daemon`, `index`, `search`, `find`,
//!     `symbol`, `impact`, `remember`, `recall`, `execute`, `capture`,
//!     `trust`, `export`, `migrate`, `purge`.

mod commands;
mod logging;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "crux",
    version,
    about = "Token optimization runtime: ten layers, one binary.",
    long_about = "CRUX is a local-first token optimization runtime. \
                  See `crux <command> --help` for subcommand details."
)]
struct Cli {
    /// Path to project root (defaults to autodetect from current directory).
    #[arg(long, global = true, env = "CRUX_PROJECT")]
    project: Option<PathBuf>,

    /// Logging level override (error/warn/info/debug/trace).
    #[arg(long, global = true, env = "CRUX_LOG")]
    log: Option<String>,

    /// Emit machine-readable output (JSON) where applicable.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Scaffold a project: writes CLAUDE.md, .crux/, .claudeignore.
    Init(commands::init::Args),

    /// Profile management (coding/analysis/agents/...).
    #[command(subcommand)]
    Profile(commands::profile::Cmd),

    /// Show or set configuration.
    #[command(subcommand)]
    Config(commands::config::Cmd),

    /// Show health snapshot (Layer 9 Coach).
    Audit(commands::audit::Args),

    /// Show telemetry stats per layer.
    Stats(commands::stats::Args),

    /// Hook handler entry points (called by agent integrations).
    #[command(subcommand)]
    Hook(commands::hook::Cmd),

    /// Diagnose CRUX setup.
    Doctor,

    /// Print version information.
    Version,

    /// Run MCP server (not implemented in Phase 1).
    Mcp,

    /// Wrap upstream MCP server with description shrinker (Phase 8).
    #[command(name = "mcp-shrink")]
    McpShrink {
        /// Upstream command followed by its args.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        upstream: Vec<String>,
    },

    /// Run a bash command through Layer 3 filters.
    Bash(commands::bash::Args),

    /// Remember an observation (Layer 8).
    Remember(commands::memory::RememberArgs),

    /// Recall observations matching a query (Layer 8).
    Recall(commands::memory::RecallArgs),

    /// Memory subcommands (list/decay/archive/delete/kinds).
    #[command(subcommand)]
    Memory(commands::memory::MemoryCmd),

    /// Coach subcommands (snapshot/record/loop/drift).
    #[command(subcommand)]
    Coach(commands::coach::Cmd),

    /// Index codebase into the AST graph (Layer 5).
    Index(commands::ast::IndexArgs),

    /// Find symbols by name or substring (Layer 5).
    Find(commands::ast::FindArgs),

    /// Show one symbol by qualified name (Layer 5).
    Symbol(commands::ast::SymbolArgs),

    /// Show callers (blast radius) of a symbol (Layer 5).
    Impact(commands::ast::ImpactArgs),

    /// Build / refresh the hybrid-search chunk store (Layer 6).
    Reindex(commands::search::ReindexArgs),

    /// Hybrid search across chunks (BM25 + dense + RRF, Layer 6).
    Search(commands::search::SearchArgs),

    /// Execute code in a sandboxed subprocess (Layer 7).
    Execute(commands::execute::ExecuteArgs),

    /// Register CRUX as an MCP server (and hooks where supported)
    /// in third-party AI agents (Claude Code, Cursor, Windsurf, etc.).
    Setup(commands::setup::Args),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(e) = logging::init(cli.log.as_deref()) {
        eprintln!("crux: failed to init logging: {e}");
    }

    match dispatch(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("crux: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(cli: &Cli) -> anyhow::Result<()> {
    match &cli.cmd {
        Cmd::Init(a) => commands::init::run(cli, a),
        Cmd::Profile(c) => commands::profile::run(cli, c),
        Cmd::Config(c) => commands::config::run(cli, c),
        Cmd::Audit(a) => commands::audit::run(cli, a),
        Cmd::Stats(a) => commands::stats::run(cli, a),
        Cmd::Hook(c) => commands::hook::run(cli, c),
        Cmd::Doctor => commands::doctor::run(cli),
        Cmd::Version => commands::version::run(cli),
        Cmd::Mcp => commands::mcp::run(cli),
        Cmd::McpShrink { upstream } => {
            let code = crux_mcp::run_shrink_proxy(upstream)?;
            std::process::exit(code);
        }
        Cmd::Bash(a) => commands::bash::run(cli, a),
        Cmd::Remember(a) => commands::memory::run_remember(cli, a),
        Cmd::Recall(a) => commands::memory::run_recall(cli, a),
        Cmd::Memory(c) => commands::memory::run_memory(cli, c),
        Cmd::Coach(c) => commands::coach::run(cli, c),
        Cmd::Index(a) => commands::ast::run_index(cli, a),
        Cmd::Find(a) => commands::ast::run_find(cli, a),
        Cmd::Symbol(a) => commands::ast::run_symbol(cli, a),
        Cmd::Impact(a) => commands::ast::run_impact(cli, a),
        Cmd::Reindex(a) => commands::search::run_reindex(cli, a),
        Cmd::Search(a) => commands::search::run_search(cli, a),
        Cmd::Execute(a) => commands::execute::run(cli, a),
        Cmd::Setup(a) => commands::setup::run(cli, a),
    }
}
