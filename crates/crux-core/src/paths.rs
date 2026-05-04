use std::path::{Path, PathBuf};

use path_clean::PathClean;

use crate::error::{CruxError, Result};

const ENV_HOME: &str = "CRUX_HOME";

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

pub fn path_is_within(child: &Path, parent: &Path) -> bool {
    match (child.absolutize(), parent.absolutize()) {
        (Ok(c), Ok(p)) => c.starts_with(&p),
        _ => false,
    }
}

pub fn expand_user_path(s: &str) -> Option<PathBuf> {
    if s == "~" {
        return dirs::home_dir();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return dirs::home_dir().map(|h| h.join(rest));
    }
    if let Some(rest) = s.strip_prefix("$HOME/") {
        return dirs::home_dir().map(|h| h.join(rest));
    }
    if s == "$HOME" {
        return dirs::home_dir();
    }
    Some(PathBuf::from(s))
}

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

    #[test]
    fn expand_user_path_handles_tilde_and_dollar_home() {
        let home = dirs::home_dir().expect("$HOME required for this test");
        assert_eq!(expand_user_path("~").unwrap(), home);
        assert_eq!(expand_user_path("~/foo/bar").unwrap(), home.join("foo/bar"));
        assert_eq!(expand_user_path("$HOME").unwrap(), home);
        assert_eq!(
            expand_user_path("$HOME/.openclaw").unwrap(),
            home.join(".openclaw")
        );
        assert_eq!(
            expand_user_path("/etc/hosts").unwrap(),
            PathBuf::from("/etc/hosts")
        );
        assert_eq!(
            expand_user_path("relative/dir").unwrap(),
            PathBuf::from("relative/dir")
        );
    }
}
