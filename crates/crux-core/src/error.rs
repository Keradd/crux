//! Crate-wide error type for `crux-core` and downstream layers.
//!
//! Library code returns `Result<T, CruxError>`. The CLI maps these into
//! human-readable messages.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CruxError {
    #[error("config error: {0}")]
    Config(String),

    #[error("config file not found: {0}")]
    ConfigNotFound(PathBuf),

    #[error("invalid config at {path}: {message}")]
    ConfigInvalid { path: PathBuf, message: String },

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("migration {version} failed: {message}")]
    Migration { version: u32, message: String },

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("io error: {0}")]
    IoBare(#[from] std::io::Error),

    #[error("toml parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("toml serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("project root not found (looked in {0} and parents)")]
    ProjectRootNotFound(PathBuf),

    #[error("path is not safe: {0}")]
    UnsafePath(String),

    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CruxError>;

impl CruxError {
    pub fn config<S: Into<String>>(msg: S) -> Self {
        CruxError::Config(msg.into())
    }
    pub fn other<S: Into<String>>(msg: S) -> Self {
        CruxError::Other(msg.into())
    }
}
