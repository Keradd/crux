//! Filesystem path resolution.
//!
//! Resolution order for the CRUX home directory:
//! 1. `$CRUX_HOME` environment variable (explicit override)
//! 2. `$XDG_DATA_HOME/crux` if XDG is set
//! 3. `~/.crux` fallback
//!
//! All paths returned here are absolute; callers are not expected to do
//! further canonicalization unless they need to defeat symlinks.

use std::path::{Path, PathBuf};

use path_clean::PathClean;

use crate::error::{CruxError, Result};

const ENV_HOME: &str = "CRUX_HOME";

/// Return the CRUX home directory, creating nothing.
pub fn crux_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(ENV_HOME) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    if let Some(home) = dirs::home_dir() {
        return Ok(home.join(".crux"));
    }
    Err(CruxError::other(
        "could not resolve CRUX home (set $CRUX_HOME or $HOME)",
    ))
}

pub fn global_config_path() -> Result<PathBuf> {
    Ok(crux_home()?.join("config.toml"))
}

pub fn db_path() -> Result<PathBuf> {
    Ok(crux_home()?.join("db").join("crux.sqlite"))
}

pub fn log_path() -> Result<PathBuf> {
    Ok(crux_home()?.join("logs").join("crux.log"))
}

/// Walk upward from `start` looking for a `.crux/` directory. Returns the
/// directory containing `.crux/`, not `.crux/` itself.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let start = start.absolutize().ok()?;
    let mut cur: &Path = &start;
    loop {
        if cur.join(".crux").is_dir() {
            return Some(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

/// True if `child` is the same path as `parent` or strictly inside it.
/// Used by Layer 7 sandbox to prevent writes outside the project root.
pub fn path_is_within(child: &Path, parent: &Path) -> bool {
    match (child.absolutize(), parent.absolutize()) {
        (Ok(c), Ok(p)) => c.starts_with(&p),
        _ => false,
    }
}

// Tiny helper trait so we don't drag a dep just for canonicalize.
trait Absolutize {
    fn absolutize(&self) -> std::io::Result<PathBuf>;
}

impl Absolutize for Path {
    fn absolutize(&self) -> std::io::Result<PathBuf> {
        let p = if self.is_absolute() {
            self.to_path_buf()
        } else {
            std::env::current_dir()?.join(self)
        };
        Ok(p.clean())
    }
}

impl Absolutize for PathBuf {
    fn absolutize(&self) -> std::io::Result<PathBuf> {
        self.as_path().absolutize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crux_home_respects_env() {
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var(ENV_HOME).ok();
        // SAFETY: tests in this module aren't run concurrently with code
        // that depends on $CRUX_HOME.
        std::env::set_var(ENV_HOME, dir.path());
        let h = crux_home().unwrap();
        assert_eq!(h, dir.path());
        match prev {
            Some(v) => std::env::set_var(ENV_HOME, v),
            None => std::env::remove_var(ENV_HOME),
        }
    }

    #[test]
    fn find_project_root_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(dir.path().join(".crux")).unwrap();
        let found = find_project_root(&nested).unwrap();
        // canonicalize tempdir to dodge macOS /private prefix
        let want = dir
            .path()
            .canonicalize()
            .unwrap_or(dir.path().to_path_buf());
        let got = found.canonicalize().unwrap_or(found.clone());
        assert_eq!(got, want);
    }

    #[test]
    fn within_check() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("sub");
        std::fs::create_dir_all(&inner).unwrap();
        assert!(path_is_within(&inner, dir.path()));
        assert!(!path_is_within(dir.path(), &inner));
    }
}
