//! CLI subcommand implementations. One file per top-level command keeps
//! the surface easy to scan.

pub mod ast;
pub mod audit;
pub mod bash;
pub mod coach;
pub mod config;
pub mod doctor;
pub mod execute;
pub mod hook;
pub mod init;
pub mod mcp;
pub mod memory;
pub mod profile;
pub mod search;
pub mod setup;
pub mod stats;
pub mod version;

use std::path::{Path, PathBuf};

/// Resolve project root: explicit `--project` > walk up from cwd looking
/// for `.crux/` > current dir.
pub fn resolve_project_root(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crux_core::paths::find_project_root(&cwd).unwrap_or(cwd)
}
