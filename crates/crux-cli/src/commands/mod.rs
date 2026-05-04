pub mod ast;
pub mod audit;
pub mod bash;
pub mod coach;
pub mod config;
pub mod digest;
pub mod doctor;
pub mod execute;
pub mod hook;
pub mod hygiene;
pub mod init;
pub mod mcp;
pub mod memory;
pub mod profile;
pub mod search;
pub mod setup;
pub mod stats;
pub mod version;

use std::path::{Path, PathBuf};

pub fn resolve_project_root(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crux_core::paths::find_project_root(&cwd).unwrap_or(cwd)
}
