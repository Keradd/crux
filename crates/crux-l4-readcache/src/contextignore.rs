//! `.contextignore` engine — gitignore-style hard block on file reads.
//!
//! Two scopes scanned, in order:
//! 1. Project: `<project_root>/.crux/contextignore`
//! 2. User:    `<crux_home>/contextignore`
//!
//! Both files are optional. Empty/missing → no block. Pattern matching
//! supports `*`, `?`, `**` against either the basename or the absolute
//! path, matching gitignore semantics for the common cases. Pre-compiled
//! regexes are cached per pattern so a hot cache hit avoids ~1k regex
//! compilations per session (alex's measurement).

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use regex::Regex;

const MAX_PATTERNS: usize = 200;

#[derive(Debug, Clone, Default)]
pub struct ContextIgnore {
    patterns: Vec<String>,
}

impl ContextIgnore {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load both scopes, project first, user second. `crux_home` may be
    /// `None` if the caller doesn't want the user-scope check (tests).
    pub fn load(project_root: &Path, crux_home: Option<&Path>) -> Self {
        let mut patterns: Vec<String> = Vec::new();
        let project_file = project_root.join(".crux").join("contextignore");
        Self::push_file(&project_file, &mut patterns);
        if let Some(home) = crux_home {
            let user_file = home.join("contextignore");
            Self::push_file(&user_file, &mut patterns);
        }
        if patterns.len() > MAX_PATTERNS {
            patterns.truncate(MAX_PATTERNS);
        }
        Self { patterns }
    }

    fn push_file(path: &Path, out: &mut Vec<String>) {
        let Ok(raw) = fs::read_to_string(path) else {
            return;
        };
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            out.push(trimmed.to_string());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// True if `file_path` matches any pattern.
    pub fn matches(&self, file_path: &Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let abs = file_path.to_string_lossy();
        let base = file_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for pat in &self.patterns {
            let re = compile(pat);
            if re.is_match(&abs) || re.is_match(&base) {
                return true;
            }
        }
        false
    }
}

// ─────────────────────────────────────────────────────────────────────────
// fnmatch-style glob → regex, cached per pattern
// ─────────────────────────────────────────────────────────────────────────

fn cache() -> &'static Mutex<HashMap<String, Regex>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn compile(pattern: &str) -> Regex {
    let mut guard = cache().lock().expect("contextignore regex cache poisoned");
    if let Some(re) = guard.get(pattern) {
        return re.clone();
    }
    let regex_src = glob_to_regex(pattern);
    let re = Regex::new(&regex_src).unwrap_or_else(|_| {
        // Hopeless pattern → never match. Safer than refusing to load.
        Regex::new("$.^").unwrap()
    });
    guard.insert(pattern.to_string(), re.clone());
    re
}

/// Translate a glob pattern (gitignore subset) into a Rust regex. Supports
/// `*` (any non-slash chars), `**` (any path), `?` (single non-slash char).
fn glob_to_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if matches!(chars.peek(), Some('*')) {
                    chars.next();
                    out.push_str(".*");
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('$');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn empty_when_files_missing() {
        let dir = tempfile::tempdir().unwrap();
        let ci = ContextIgnore::load(dir.path(), None);
        assert!(ci.is_empty());
    }

    #[test]
    fn project_patterns_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crux")).unwrap();
        write(
            &dir.path().join(".crux"),
            "contextignore",
            "secrets.json\n*.key\n# a comment\n",
        );

        let ci = ContextIgnore::load(dir.path(), None);
        assert!(!ci.is_empty());
        assert!(ci.matches(Path::new("/anywhere/secrets.json")));
        assert!(ci.matches(Path::new("/anywhere/foo.key")));
        assert!(!ci.matches(Path::new("/anywhere/foo.txt")));
    }

    #[test]
    fn user_patterns_layer_on_top() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crux")).unwrap();
        write(&dir.path().join(".crux"), "contextignore", "secrets.json\n");

        let user_home = tempfile::tempdir().unwrap();
        write(user_home.path(), "contextignore", "*.private.key\n");

        let ci = ContextIgnore::load(dir.path(), Some(user_home.path()));
        assert!(ci.matches(Path::new("secrets.json")));
        assert!(ci.matches(Path::new("/x/y/foo.private.key")));
    }

    #[test]
    fn double_star_matches_path() {
        assert!(compile("**/secrets.json").is_match("/x/y/z/secrets.json"));
        assert!(!compile("nope").is_match("/x/y/z/secrets.json"));
    }
}
