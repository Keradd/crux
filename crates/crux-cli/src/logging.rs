//! `tracing-subscriber` setup. Default level is `info` unless overridden by
//! `RUST_LOG`, the `--log` flag, or the global config (resolved later).
//!
//! Logs go to stderr so stdout stays clean for MCP / hook protocols.

use anyhow::Result;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn init(level_override: Option<&str>) -> Result<()> {
    let env_filter = match level_override {
        Some(level) => EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info")),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };

    let layer = fmt::layer()
        .with_target(false)
        .with_writer(std::io::stderr)
        .compact();

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(layer)
        .try_init();
    Ok(())
}
