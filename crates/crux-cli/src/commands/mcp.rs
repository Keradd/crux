use anyhow::Result;

use crux_core::Runtime;

use super::resolve_project_root;
use crate::Cli;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long, help = "TCP port for MCP server (default: stdio mode)")]
    pub port: Option<u16>,

    #[arg(long, help = "Bind address for TCP mode (default: 127.0.0.1)")]
    pub host: Option<String>,
}

pub fn run(cli: &Cli, args: &Args) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project)
    } else {
        None
    };
    let runtime = Runtime::open(project_opt)?;
    match args.port {
        Some(port) => {
            let host = args.host.as_deref().unwrap_or("127.0.0.1");
            let addr: std::net::SocketAddr = format!("{host}:{port}").parse()?;
            crux_mcp::serve_tcp(runtime, addr)?;
        }
        None => {
            crux_mcp::serve_stdio(runtime)?;
        }
    }
    Ok(())
}
