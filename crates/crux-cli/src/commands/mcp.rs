use anyhow::Result;

use crux_core::Runtime;

use super::resolve_project_root;
use crate::Cli;

pub fn run(cli: &Cli) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project)
    } else {
        None
    };
    let runtime = Runtime::open(project_opt)?;
    crux_mcp::serve_stdio(runtime)?;
    Ok(())
}
