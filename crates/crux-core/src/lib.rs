//! `crux-core` — shared foundation for every CRUX layer.
//!
//! What lives here:
//! - `error`     — the `CruxError` enum used across all crates
//! - `paths`     — filesystem path resolution (`$CRUX_HOME`, project root)
//! - `config`    — TOML config schema + load/save with global+project merge
//! - `db`        — rusqlite open + WAL pragmas + numbered migrations
//! - `telemetry` — per-layer event recording (read by Layer 9 Coach)
//! - `tokens`    — chars/4 token estimator
//!
//! Higher-level layers (`crux-l4-readcache`, `crux-l10-setup`, …) depend on
//! this crate and on each other only via their public APIs.

pub mod config;
pub mod config_watch;
pub mod db;
pub mod error;
pub mod merkle;
pub mod paths;
pub mod telemetry;
pub mod tokens;

pub use config::{Config, LayerMode, LoadedConfig};
pub use config_watch::{ConfigWatcher, WatcherHandle, DEFAULT_POLL_INTERVAL};
pub use error::{CruxError, Result};

/// Convenience initializer used by the CLI: load config, open DB.
pub struct Runtime {
    pub config: Config,
    pub conn: rusqlite::Connection,
    pub project_root: Option<std::path::PathBuf>,
    pub global_config_path: std::path::PathBuf,
    pub project_config_path: Option<std::path::PathBuf>,
}

impl Runtime {
    /// Load config (global+project merge), open DB, return runtime.
    ///
    /// `project_root` is the directory whose `.crux/config.toml` should be
    /// considered. Pass `None` for "no project context".
    pub fn open(project_root: Option<std::path::PathBuf>) -> Result<Self> {
        let loaded = config::load(project_root.as_deref())?;
        let db_path = loaded
            .config
            .general
            .db_path
            .clone()
            .unwrap_or(db::default_db_path()?);
        let conn = db::open(&db_path)?;
        Ok(Self {
            config: loaded.config,
            conn,
            project_root,
            global_config_path: loaded.global_path,
            project_config_path: loaded.project_path,
        })
    }
}
