pub mod profiles;
pub mod setup;
pub mod templates;

use std::fs;
use std::path::{Path, PathBuf};

use crux_core::config::{self, Config};
use crux_core::error::{CruxError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectType {
    RustCargo,
    NodeNpm,
    Python,
    Ruby,
    Go,
    Java,
    Generic,
}

impl ProjectType {
    pub fn label(self) -> &'static str {
        match self {
            ProjectType::RustCargo => "Rust (Cargo)",
            ProjectType::NodeNpm => "Node.js",
            ProjectType::Python => "Python",
            ProjectType::Ruby => "Ruby",
            ProjectType::Go => "Go",
            ProjectType::Java => "Java",
            ProjectType::Generic => "generic",
        }
    }
}

pub fn detect_project_type(root: &Path) -> ProjectType {
    if root.join("Cargo.toml").is_file() {
        return ProjectType::RustCargo;
    }
    if root.join("package.json").is_file() {
        return ProjectType::NodeNpm;
    }
    if root.join("pyproject.toml").is_file()
        || root.join("requirements.txt").is_file()
        || root.join("setup.py").is_file()
    {
        return ProjectType::Python;
    }
    if root.join("Gemfile").is_file() {
        return ProjectType::Ruby;
    }
    if root.join("go.mod").is_file() {
        return ProjectType::Go;
    }
    if root.join("pom.xml").is_file() || root.join("build.gradle").is_file() {
        return ProjectType::Java;
    }
    ProjectType::Generic
}

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub project_root: PathBuf,
    pub profile: String,
    pub project_type: Option<String>,
    pub stack: Option<String>,
    pub features: Option<String>,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct InitReport {
    pub written: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, &'static str)>,
    pub project_type: ProjectType,
    pub profile: String,
}

pub fn init(opts: &InitOptions) -> Result<InitReport> {
    let root = &opts.project_root;
    if !root.is_dir() {
        return Err(CruxError::other(format!(
            "project root does not exist or is not a directory: {}",
            root.display()
        )));
    }

    let profile = profiles::get(&opts.profile)
        .ok_or_else(|| CruxError::other(format!("unknown profile: {}", opts.profile)))?;

    let project_type = detect_project_type(root);
    let project_type_label = opts
        .project_type
        .clone()
        .unwrap_or_else(|| project_type.label().to_string());
    let stack = opts.stack.clone().unwrap_or_else(|| "(unspecified)".into());
    let features = opts
        .features
        .clone()
        .unwrap_or_else(|| "(describe project features here)".into());

    let mut written = Vec::new();
    let mut skipped = Vec::new();

    for sub in [
        ".crux",
        ".crux/completions",
        ".crux/sessions",
        ".crux/sessions/active",
        ".crux/sessions/archive",
        ".crux/captures",
    ] {
        let p = root.join(sub);
        if !p.exists() {
            fs::create_dir_all(&p).map_err(|e| CruxError::Io {
                path: p.clone(),
                source: e,
            })?;
        }
    }

    let meta = templates::ProjectMeta {
        project_type: &project_type_label,
        stack: &stack,
        features: &features,
        profile_name: profile.name,
    };
    let claude_md = templates::render_claude_md(&meta, profile.claude_md);
    write_file(
        &root.join("CLAUDE.md"),
        &claude_md,
        opts.force,
        &mut written,
        &mut skipped,
    )?;

    write_file(
        &root.join(".crux/COMMON_MISTAKES.md"),
        templates::COMMON_MISTAKES,
        opts.force,
        &mut written,
        &mut skipped,
    )?;
    write_file(
        &root.join(".crux/QUICK_START.md"),
        templates::QUICK_START,
        opts.force,
        &mut written,
        &mut skipped,
    )?;
    write_file(
        &root.join(".crux/ARCHITECTURE_MAP.md"),
        templates::ARCHITECTURE_MAP,
        opts.force,
        &mut written,
        &mut skipped,
    )?;
    write_file(
        &root.join(".crux/completions/README.md"),
        templates::COMPLETIONS_README,
        opts.force,
        &mut written,
        &mut skipped,
    )?;
    write_file(
        &root.join(".crux/sessions/README.md"),
        templates::SESSIONS_README,
        opts.force,
        &mut written,
        &mut skipped,
    )?;
    write_file(
        &root.join(".crux/contextignore"),
        templates::CRUX_IGNORE,
        opts.force,
        &mut written,
        &mut skipped,
    )?;

    write_file(
        &root.join(".claudeignore"),
        templates::CLAUDEIGNORE,
        opts.force,
        &mut written,
        &mut skipped,
    )?;

    let cfg_path = root.join(".crux/config.toml");
    if !cfg_path.exists() || opts.force {
        let mut cfg = Config::default();
        cfg.layer.l1.profile = profile.name.into();
        config::save(&cfg, &cfg_path)?;
        written.push(cfg_path);
    } else {
        skipped.push((cfg_path, "exists"));
    }

    Ok(InitReport {
        written,
        skipped,
        project_type,
        profile: profile.name.into(),
    })
}

fn write_file(
    path: &Path,
    contents: &str,
    force: bool,
    written: &mut Vec<PathBuf>,
    skipped: &mut Vec<(PathBuf, &'static str)>,
) -> Result<()> {
    if path.exists() && !force {
        skipped.push((path.to_path_buf(), "exists"));
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| CruxError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    fs::write(path, contents).map_err(|e| CruxError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    written.push(path.to_path_buf());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::RustCargo);
    }

    #[test]
    fn init_writes_expected_files() {
        let dir = tempfile::tempdir().unwrap();
        let opts = InitOptions {
            project_root: dir.path().to_path_buf(),
            profile: "coding".into(),
            project_type: None,
            stack: None,
            features: None,
            force: false,
        };
        let report = init(&opts).unwrap();

        for fname in [
            "CLAUDE.md",
            ".crux/COMMON_MISTAKES.md",
            ".crux/QUICK_START.md",
            ".crux/ARCHITECTURE_MAP.md",
            ".crux/contextignore",
            ".crux/config.toml",
            ".claudeignore",
        ] {
            assert!(
                dir.path().join(fname).is_file(),
                "missing scaffolded file: {}",
                fname
            );
        }
        assert!(report.written.iter().any(|p| p.ends_with("CLAUDE.md")));
        assert_eq!(report.profile, "coding");
    }

    #[test]
    fn init_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let opts = InitOptions {
            project_root: dir.path().to_path_buf(),
            profile: "coding".into(),
            project_type: None,
            stack: None,
            features: None,
            force: false,
        };
        let r1 = init(&opts).unwrap();
        let r2 = init(&opts).unwrap();
        assert!(!r1.written.is_empty());
        assert!(
            r2.skipped.len() >= r1.written.len(),
            "expected skip count >= initial write count"
        );
    }

    #[test]
    fn force_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = InitOptions {
            project_root: dir.path().to_path_buf(),
            profile: "coding".into(),
            project_type: None,
            stack: None,
            features: None,
            force: false,
        };
        init(&opts).unwrap();
        let claude = dir.path().join("CLAUDE.md");
        std::fs::write(&claude, "tampered").unwrap();
        opts.force = true;
        init(&opts).unwrap();
        let after = std::fs::read_to_string(&claude).unwrap();
        assert!(after.contains("CLAUDE.md"));
        assert!(!after.contains("tampered"));
    }
}
