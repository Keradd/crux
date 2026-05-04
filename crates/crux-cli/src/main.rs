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
    #[arg(long, global = true, env = "CRUX_PROJECT")]
    project: Option<PathBuf>,

    #[arg(long, global = true, env = "CRUX_LOG")]
    log: Option<String>,

    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    Init(commands::init::Args),

    #[command(subcommand)]
    Profile(commands::profile::Cmd),

    #[command(subcommand)]
    Config(commands::config::Cmd),

    Audit(commands::audit::Args),

    Stats(commands::stats::Args),

    #[command(subcommand)]
    Hook(commands::hook::Cmd),

    Doctor,

    Version,

    Mcp,

    #[command(name = "mcp-shrink")]
    McpShrink {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        upstream: Vec<String>,
    },

    Bash(commands::bash::Args),

    Remember(commands::memory::RememberArgs),

    Recall(commands::memory::RecallArgs),

    #[command(subcommand)]
    Memory(commands::memory::MemoryCmd),

    #[command(subcommand)]
    Coach(commands::coach::Cmd),

    Index(commands::ast::IndexArgs),

    Find(commands::ast::FindArgs),

    Symbol(commands::ast::SymbolArgs),

    Impact(commands::ast::ImpactArgs),

    Reindex(commands::search::ReindexArgs),

    Search(commands::search::SearchArgs),

    Execute(commands::execute::ExecuteArgs),

    Setup(commands::setup::Args),

    Digest(commands::digest::DigestArgs),

    Compact(commands::digest::CompactArgs),

    #[command(subcommand)]
    Hygiene(commands::hygiene::Cmd),
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
        Cmd::Digest(a) => commands::digest::run_digest(cli, a),
        Cmd::Compact(a) => commands::digest::run_compact(cli, a),
        Cmd::Hygiene(c) => commands::hygiene::run(cli, c),
    }
}
