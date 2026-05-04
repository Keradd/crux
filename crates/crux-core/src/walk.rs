pub const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".next",
    "vendor",
    "_refs",
    "third_party",
    "third-party",
    "external",
    "submodules",
    "deps",
    ".venv",
    "venv",
    "env",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    ".cache",
    ".idea",
    ".vscode",
];

pub fn is_excluded_dir(name: &str) -> bool {
    DEFAULT_EXCLUDE_DIRS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_excludes_common_heavy_dirs() {
        for d in &[
            "node_modules",
            "target",
            ".git",
            "dist",
            "build",
            ".venv",
            "__pycache__",
        ] {
            assert!(is_excluded_dir(d), "expected {d} to be excluded");
        }
    }

    #[test]
    fn default_does_not_exclude_source_dirs() {
        for d in &["src", "crates", "tests", "docs", "benches"] {
            assert!(!is_excluded_dir(d), "expected {d} to NOT be excluded");
        }
    }
}
