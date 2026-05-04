use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, Subcommand};
use serde::Deserialize;

use crux_l12_hygiene::{
    fix_comments, scan_comments, scan_paths, strip_comments, HygieneOptions, HygieneReport,
};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    Comments(CommentArgs),
}

#[derive(Debug, ClapArgs)]
pub struct CommentArgs {
    #[arg(long, value_name = "PATH")]
    pub root: Option<PathBuf>,

    #[arg(long, conflicts_with = "check")]
    pub fix: bool,

    #[arg(long)]
    pub check: bool,

    #[arg(long, conflicts_with_all = ["fix", "check", "changed_from_stdin"])]
    pub strip: bool,

    #[arg(long, conflicts_with = "fix")]
    pub changed_from_stdin: bool,

    #[arg(long = "path", value_name = "PATH", conflicts_with = "fix")]
    pub paths: Vec<PathBuf>,

    #[arg(long, default_value_t = 5)]
    pub max_module_doc_lines: usize,

    #[arg(long, default_value_t = 10)]
    pub min_banner_run: usize,
}

#[derive(Debug, Default, Deserialize)]
struct EditEvent {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: EditToolInput,
}

#[derive(Debug, Default, Deserialize)]
struct EditToolInput {
    #[serde(default)]
    file_path: Option<String>,
}

pub fn run(cli: &Cli, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::Comments(args) => run_comments(cli, args),
    }
}

fn build_options(cli: &Cli, args: &CommentArgs) -> HygieneOptions {
    let root = args
        .root
        .clone()
        .unwrap_or_else(|| resolve_project_root(cli.project.as_deref()));
    HygieneOptions {
        max_module_doc_lines: args.max_module_doc_lines,
        min_banner_run: args.min_banner_run,
        ..HygieneOptions::for_root(root)
    }
}

fn run_comments(cli: &Cli, args: &CommentArgs) -> Result<()> {
    let options = build_options(cli, args);
    if args.fix {
        let report = fix_comments(&options.root, &options)?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else if report.is_clean() {
            println!(
                "hygiene: clean ({} files scanned, no fixes applied)",
                report.files_scanned
            );
        } else {
            println!(
                "hygiene: fixed {} file(s), removed {} line(s)",
                report.files_fixed, report.lines_removed
            );
            for f in &report.fixed_files {
                println!("  fixed {}", f.display());
            }
        }
        return Ok(());
    }
    if args.strip {
        let report = strip_comments(&options.root, &options)?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else if report.is_clean() {
            println!(
                "hygiene: strip clean ({} files scanned, nothing to remove)",
                report.files_scanned
            );
        } else {
            println!(
                "hygiene: stripped {} file(s), removed {} line(s)",
                report.files_stripped, report.lines_removed
            );
            for f in &report.stripped_files {
                println!("  stripped {}", f.display());
            }
        }
        return Ok(());
    }

    let hook_mode = args.changed_from_stdin;
    if hook_mode && !l12_hygiene_enabled(&options.root) {
        let _ = read_stdin_to_string();
        return Ok(());
    }

    let scoped = hook_mode || !args.paths.is_empty();
    let report = if scoped {
        let mut targets: Vec<PathBuf> = args.paths.clone();
        if hook_mode {
            let raw = read_stdin_to_string().context("reading hygiene hook event from stdin")?;
            if let Some(fp) = extract_edited_file_path(&raw)? {
                targets.push(fp);
            }
        }
        if targets.is_empty() {
            HygieneReport::default()
        } else {
            scan_paths(targets.iter().map(|p| p.as_path()), &options)?
        }
    } else {
        scan_comments(&options.root, &options)?
    };

    emit_scan_report(&report, cli.json, scoped, hook_mode);

    if !report.is_clean() {
        let code = if hook_mode { 2 } else { 1 };
        std::process::exit(code);
    }
    Ok(())
}

fn emit_scan_report(report: &HygieneReport, json: bool, scoped: bool, hook_mode: bool) {
    if json {
        let rendered = serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".into());
        if hook_mode {
            eprintln!("{rendered}");
        } else {
            println!("{rendered}");
        }
        return;
    }

    let mut lines: Vec<String> = Vec::new();
    if report.is_clean() {
        let scope = if scoped { " (scoped)" } else { "" };
        lines.push(format!(
            "hygiene: clean ({} files scanned, no violations){scope}",
            report.files_scanned
        ));
    } else {
        lines.push(format!(
            "hygiene: {} violation(s) across {} file(s) ({} files scanned)",
            report.violation_count(),
            report.files_with_violations,
            report.files_scanned
        ));
        for v in &report.violations {
            let suggestion = v
                .suggested_replacement
                .as_deref()
                .map(|s| format!(" → {s}"))
                .unwrap_or_default();
            lines.push(format!(
                "  {}:{} [{}] {}{}",
                v.file.display(),
                v.line,
                v.rule_id,
                v.reason,
                suggestion
            ));
            lines.push(format!("      | {}", v.snippet));
        }
        lines.push(String::new());
        lines.push(
            "run `crux hygiene comments --fix` to auto-clean banners and section blocks.".into(),
        );
    }

    let out = lines.join("\n");
    if hook_mode {
        eprintln!("{out}");
    } else {
        println!("{out}");
    }
}

fn read_stdin_to_string() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

fn l12_hygiene_enabled(project_root: &std::path::Path) -> bool {
    match crux_core::config::load(Some(project_root)) {
        Ok(loaded) => loaded.config.layers.l12_hygiene,
        Err(_) => false,
    }
}

fn extract_edited_file_path(raw: &str) -> Result<Option<PathBuf>> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let event: EditEvent = serde_json::from_str(raw).with_context(|| {
        format!(
            "hygiene hook event was not valid JSON: {}",
            truncate(raw, 200)
        )
    })?;
    if !matches!(
        event.tool_name.as_str(),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit"
    ) {
        return Ok(None);
    }
    Ok(event
        .tool_input
        .file_path
        .filter(|s| !s.is_empty())
        .map(PathBuf::from))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_file_path_edit_event() {
        let raw = r#"{"tool_name":"Edit","tool_input":{"file_path":"/a/b.rs"}}"#;
        let p = extract_edited_file_path(raw).unwrap().unwrap();
        assert_eq!(p, PathBuf::from("/a/b.rs"));
    }

    #[test]
    fn extract_file_path_write_event() {
        let raw = r#"{"tool_name":"Write","tool_input":{"file_path":"/x.py"}}"#;
        let p = extract_edited_file_path(raw).unwrap().unwrap();
        assert_eq!(p, PathBuf::from("/x.py"));
    }

    #[test]
    fn extract_file_path_multiedit_event() {
        let raw = r#"{"tool_name":"MultiEdit","tool_input":{"file_path":"/a.ts"}}"#;
        let p = extract_edited_file_path(raw).unwrap().unwrap();
        assert_eq!(p, PathBuf::from("/a.ts"));
    }

    #[test]
    fn extract_file_path_bash_event_is_none() {
        let raw = r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        assert!(extract_edited_file_path(raw).unwrap().is_none());
    }

    #[test]
    fn extract_file_path_empty_stdin_is_none() {
        assert!(extract_edited_file_path("").unwrap().is_none());
        assert!(extract_edited_file_path("   \n\t").unwrap().is_none());
    }

    #[test]
    fn extract_file_path_missing_path_is_none() {
        let raw = r#"{"tool_name":"Edit","tool_input":{}}"#;
        assert!(extract_edited_file_path(raw).unwrap().is_none());
    }

    #[test]
    fn extract_file_path_empty_path_is_none() {
        let raw = r#"{"tool_name":"Edit","tool_input":{"file_path":""}}"#;
        assert!(extract_edited_file_path(raw).unwrap().is_none());
    }

    #[test]
    fn extract_file_path_invalid_json_errors() {
        let err = extract_edited_file_path("not json").unwrap_err();
        assert!(err.to_string().contains("not valid JSON"));
    }

    #[test]
    fn l12_hygiene_enabled_false_when_toggle_off() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crux")).unwrap();
        std::fs::write(
            dir.path().join(".crux").join("config.toml"),
            "[layers]\nl12_hygiene = false\n",
        )
        .unwrap();
        assert!(!l12_hygiene_enabled(dir.path()));
    }

    #[test]
    fn l12_hygiene_enabled_true_when_opted_in() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crux")).unwrap();
        std::fs::write(
            dir.path().join(".crux").join("config.toml"),
            "[layers]\nl12_hygiene = true\n",
        )
        .unwrap();
        assert!(l12_hygiene_enabled(dir.path()));
    }
}
