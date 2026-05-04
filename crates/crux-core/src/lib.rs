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

pub struct Runtime {
    pub config: Config,
    pub conn: rusqlite::Connection,
    pub project_root: Option<std::path::PathBuf>,
    pub global_config_path: std::path::PathBuf,
    pub project_config_path: Option<std::path::PathBuf>,
}

impl Runtime {
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
