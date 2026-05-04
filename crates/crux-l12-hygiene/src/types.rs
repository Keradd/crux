use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct HygieneOptions {
    pub root: PathBuf,
    pub max_module_doc_lines: usize,
    pub min_banner_run: usize,
    pub include_extensions: Vec<String>,
    pub ignored_dirs: Vec<String>,
    pub ignored_files: Vec<String>,
    pub follow_symlinks: bool,
}

impl HygieneOptions {
    pub fn for_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_module_doc_lines: 5,
            min_banner_run: 10,
            include_extensions: default_extensions(),
            ignored_dirs: default_ignored_dirs(),
            ignored_files: default_ignored_files(),
            follow_symlinks: false,
        }
    }
}

pub fn default_extensions() -> Vec<String> {
    [
        "rs", "toml", "md", "markdown", "yml", "yaml", "js", "jsx", "ts", "tsx", "mjs", "cjs", "py",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

pub fn default_ignored_dirs() -> Vec<String> {
    [
        "target",
        ".git",
        "node_modules",
        "dist",
        "build",
        ".next",
        "vendor",
        ".venv",
        "__pycache__",
        ".cache",
        ".idea",
        ".vscode",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

pub fn default_ignored_files() -> Vec<String> {
    [
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "poetry.lock",
        "uv.lock",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HygieneViolation {
    pub file: PathBuf,
    pub line: usize,
    pub rule_id: String,
    pub severity: Severity,
    pub reason: String,
    pub snippet: String,
    pub suggested_replacement: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HygieneReport {
    pub files_scanned: usize,
    pub files_with_violations: usize,
    pub violations: Vec<HygieneViolation>,
}

impl HygieneReport {
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }

    pub fn violation_count(&self) -> usize {
        self.violations.len()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FixReport {
    pub files_scanned: usize,
    pub files_fixed: usize,
    pub lines_removed: usize,
    pub fixed_files: Vec<PathBuf>,
}

impl FixReport {
    pub fn is_clean(&self) -> bool {
        self.files_fixed == 0
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StripReport {
    pub files_scanned: usize,
    pub files_stripped: usize,
    pub lines_removed: usize,
    pub stripped_files: Vec<PathBuf>,
}

impl StripReport {
    pub fn is_clean(&self) -> bool {
        self.files_stripped == 0
    }
}
