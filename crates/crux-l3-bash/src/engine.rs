//! Filter loading + dispatch.
//!
//! `FilterEngine::builtin()` loads the filters baked into the binary at
//! build time. `load_user_filters` extends the engine with files from
//! `~/.config/crux/filters/*.toml` and project-local `.crux/filters/*.toml`
//! (the latter only after `crux trust`, enforced by the CLI layer).

use std::path::Path;

use crux_core::error::{CruxError, Result};

use crate::pipeline::{Filter, FilterOutput, OutputKind};
use crate::spec::{FilterFile, FilterSpec};

// Built-in filter TOML — embedded so the binary is self-sufficient.
//
// Order matters: earlier entries are tried first by `FilterEngine::find`.
// Tool-specific filters (git/cargo/openclaw/...) must precede `generic`,
// which is the last-resort catch-all.
const BUILTIN_FILTERS: &[(&str, &str)] = &[
    ("git", include_str!("../filters/git.toml")),
    ("cargo", include_str!("../filters/cargo.toml")),
    ("npm", include_str!("../filters/npm.toml")),
    ("jest", include_str!("../filters/jest.toml")),
    ("openclaw", include_str!("../filters/openclaw.toml")),
    ("generic", include_str!("../filters/generic.toml")),
];

/// In-memory collection of compiled filters, scanned linearly per command.
pub struct FilterEngine {
    filters: Vec<Filter>,
}

impl FilterEngine {
    /// Empty engine, useful for tests that hand-pick filters.
    pub fn empty() -> Self {
        Self { filters: vec![] }
    }

    /// All built-in filters compiled and ready to use.
    pub fn builtin() -> Result<Self> {
        let mut engine = Self::empty();
        for (origin, raw) in BUILTIN_FILTERS {
            engine
                .add_from_str(raw)
                .map_err(|e| CruxError::other(format!("builtin filter '{origin}': {e}")))?;
        }
        Ok(engine)
    }

    /// Append filters parsed from a TOML string. Order matters: earlier
    /// filters are tried first.
    pub fn add_from_str(&mut self, toml_src: &str) -> Result<()> {
        let parsed: FilterFile = toml::from_str(toml_src)?;
        for (name, spec) in parsed.filters {
            self.add(name, spec)?;
        }
        Ok(())
    }

    /// Append filters from a TOML file on disk.
    pub fn add_from_file(&mut self, path: &Path) -> Result<()> {
        let s = std::fs::read_to_string(path).map_err(|e| CruxError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        self.add_from_str(&s)
    }

    /// Append every `*.toml` file in `dir` (non-recursive). Missing dirs
    /// are not an error — they simply contribute nothing.
    pub fn add_from_dir(&mut self, dir: &Path) -> Result<usize> {
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => {
                return Err(CruxError::Io {
                    path: dir.to_path_buf(),
                    source: e,
                })
            }
        };
        let mut count = 0usize;
        let mut paths: Vec<_> = read
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|ext| ext == "toml").unwrap_or(false))
            .collect();
        paths.sort(); // deterministic load order
        for p in paths {
            self.add_from_file(&p)?;
            count += 1;
        }
        Ok(count)
    }

    pub fn add(&mut self, name: String, spec: FilterSpec) -> Result<()> {
        let f = Filter::compile(name.clone(), spec)
            .map_err(|e| CruxError::other(format!("compile filter '{name}': {e}")))?;
        self.filters.push(f);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.filters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    pub fn names(&self) -> Vec<&str> {
        self.filters.iter().map(|f| f.name.as_str()).collect()
    }

    /// Find the first filter whose `match_command` matches `command_line`.
    pub fn find(&self, command_line: &str) -> Option<&Filter> {
        self.filters.iter().find(|f| f.matches(command_line))
    }

    /// Run a command line's output through the appropriate filter. If no
    /// filter matches, the input is returned unchanged with `Passthrough`.
    pub fn process(&self, command_line: &str, output: &str) -> ProcessResult {
        match self.find(command_line) {
            Some(f) => {
                let out = f.apply(output);
                ProcessResult {
                    filter_name: Some(f.name.clone()),
                    output: out,
                }
            }
            None => ProcessResult {
                filter_name: None,
                output: FilterOutput {
                    text: output.to_string(),
                    kind: OutputKind::Passthrough,
                },
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub filter_name: Option<String>,
    pub output: FilterOutput,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_loads_without_error() {
        let e = FilterEngine::builtin().unwrap();
        assert!(e.len() >= 5);
        // generic is the last-resort matcher; it must be present.
        assert!(e.names().contains(&"generic"));
    }

    #[test]
    fn git_status_match_short_circuits() {
        let e = FilterEngine::builtin().unwrap();
        let r = e.process(
            "git status",
            "On branch main\nnothing to commit, working tree clean\n",
        );
        assert_eq!(r.filter_name.as_deref(), Some("git-status"));
        assert_eq!(r.output.text, "git status: clean");
    }

    #[test]
    fn cargo_build_fallback_to_on_empty() {
        let e = FilterEngine::builtin().unwrap();
        let raw = "   Compiling crux-core v0.1.0\n    Finished `dev` profile in 0.5s\n";
        let r = e.process("cargo build", raw);
        assert_eq!(r.filter_name.as_deref(), Some("cargo-build"));
        assert_eq!(r.output.text, "cargo: ok");
    }

    #[test]
    fn unknown_command_falls_to_generic() {
        let e = FilterEngine::builtin().unwrap();
        let r = e.process("totally-unknown-tool --flag", "x\n");
        // "generic" matches everything as a last resort.
        assert_eq!(r.filter_name.as_deref(), Some("generic"));
        assert_eq!(r.output.text, "x");
    }

    #[test]
    fn add_from_str_then_dispatch() {
        let mut e = FilterEngine::empty();
        e.add_from_str(
            r#"
[filters.demo]
match_command = "^demo\\b"
on_empty      = "ok"
[[filters.demo.match_output]]
pattern = "^success$"
message = "ok"
"#,
        )
        .unwrap();
        let r = e.process("demo run", "success\n");
        assert_eq!(r.output.text, "ok");
    }
}
