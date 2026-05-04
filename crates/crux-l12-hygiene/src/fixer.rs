use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::rules::{is_decorative_banner, is_goal_header, is_public_surface_header};
use crate::scanner::{
    classify_lines, detect_lang, is_comment_kind, is_generated, walk, ClassifiedLine, Lang,
    LineKind,
};
use crate::types::{FixReport, HygieneOptions};

pub fn fix_comments(root: &Path, options: &HygieneOptions) -> io::Result<FixReport> {
    let mut report = FixReport::default();
    let mut files: Vec<PathBuf> = Vec::new();
    walk(root, options, &mut files)?;
    files.sort();
    for file in &files {
        let content = match fs::read_to_string(file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if is_generated(&content) {
            continue;
        }
        report.files_scanned += 1;
        let lang = detect_lang(file);
        if !lang_is_fixable(lang) {
            continue;
        }
        let lines = classify_lines(&content, lang);
        let (new_content, removed) = rewrite(&content, &lines, lang, options);
        if removed > 0 && new_content != content {
            fs::write(file, &new_content)?;
            report.files_fixed += 1;
            report.lines_removed += removed;
            report.fixed_files.push(file.clone());
        }
    }
    Ok(report)
}

fn lang_is_fixable(lang: Lang) -> bool {
    matches!(
        lang,
        Lang::Rust | Lang::Toml | Lang::Yaml | Lang::JsTs | Lang::Python
    )
}

fn rewrite(
    src: &str,
    lines: &[ClassifiedLine],
    lang: Lang,
    options: &HygieneOptions,
) -> (String, usize) {
    let mut drop = vec![false; lines.len()];
    for (i, ln) in lines.iter().enumerate() {
        if !is_comment_kind(&ln.kind) {
            continue;
        }
        if is_safety(&ln.body) {
            continue;
        }
        if is_decorative_banner(&ln.body, options.min_banner_run) {
            drop[i] = true;
            continue;
        }
        if is_goal_header(&ln.body) || is_public_surface_header(&ln.body) {
            drop_section_run(lines, i, &mut drop);
        }
    }
    if matches!(lang, Lang::Rust) {
        compress_long_module_doc(lines, options.max_module_doc_lines, &mut drop);
    }
    let removed = drop.iter().filter(|d| **d).count();
    if removed == 0 {
        return (src.to_string(), 0);
    }
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !drop[*i])
        .map(|(_, l)| l.raw.as_str())
        .collect();
    let mut out = kept.join("\n");
    if src.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    (out, removed)
}

fn is_safety(body: &str) -> bool {
    let lower = body.trim_start().to_ascii_lowercase();
    lower.starts_with("safety:")
        || lower.starts_with("unsafe:")
        || lower.starts_with("security:")
        || lower.starts_with("warning:")
        || lower.starts_with("todo:")
        || lower.starts_with("fixme:")
        || lower.starts_with("xxx:")
        || lower.starts_with("hack:")
        || lower.starts_with("note:")
}

fn drop_section_run(lines: &[ClassifiedLine], header: usize, drop: &mut [bool]) {
    drop[header] = true;
    let head_kind = lines[header].kind.clone();
    let mut j = header + 1;
    while j < lines.len() && lines[j].kind == head_kind {
        let body = lines[j].body.trim_start();
        let is_bullet = body.starts_with('-')
            || body.starts_with('*')
            || body.starts_with('+')
            || body.starts_with('[')
            || body.starts_with("1.")
            || body.starts_with("2.")
            || body.starts_with("3.");
        if body.is_empty() || is_bullet {
            drop[j] = true;
            j += 1;
        } else {
            break;
        }
    }
}

fn compress_long_module_doc(lines: &[ClassifiedLine], max_run: usize, drop: &mut [bool]) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != LineKind::ModuleDoc {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].kind == LineKind::ModuleDoc {
            i += 1;
        }
        let end = i;
        let alive: Vec<usize> = (start..end).filter(|k| !drop[*k]).collect();
        if alive.len() <= max_run {
            continue;
        }
        let keeper = alive
            .iter()
            .copied()
            .find(|k| !lines[*k].body.trim().is_empty() || is_safety(&lines[*k].body))
            .unwrap_or(alive[0]);
        for k in alive {
            if k != keeper {
                drop[k] = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn fix_drops_banner_in_rust() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
// ─────────────────────────────
// section header
fn a() {}
// ─────────────────────────────
fn b() {}
";
        let path = write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        let report = fix_comments(dir.path(), &opts).unwrap();
        assert_eq!(report.files_fixed, 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("───"));
        assert!(after.contains("fn a()"));
        assert!(after.contains("fn b()"));
    }

    #[test]
    fn fix_compresses_long_module_doc() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
//! Title sentence.
//!
//! Extra paragraph one.
//!
//! Extra paragraph two.
fn main() {}
";
        let path = write(dir.path(), "src.rs", body);
        let opts = HygieneOptions {
            max_module_doc_lines: 2,
            ..HygieneOptions::for_root(dir.path())
        };
        let report = fix_comments(dir.path(), &opts).unwrap();
        assert_eq!(report.files_fixed, 1);
        let after = std::fs::read_to_string(&path).unwrap();
        let mod_doc_lines = after.lines().filter(|l| l.starts_with("//!")).count();
        assert_eq!(mod_doc_lines, 1, "after = {after}");
        assert!(after.contains("//! Title sentence."));
        assert!(after.contains("fn main()"));
    }

    #[test]
    fn fix_removes_goal_and_public_surface_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
//! Title.
//!
//! Goal: do the thing.
//!
//! Public surface:
//!
//! - foo
//! - bar
//!
//! Trailing notes.
fn main() {}
";
        let path = write(dir.path(), "src.rs", body);
        let opts = HygieneOptions {
            max_module_doc_lines: 99,
            ..HygieneOptions::for_root(dir.path())
        };
        let report = fix_comments(dir.path(), &opts).unwrap();
        assert_eq!(report.files_fixed, 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("Goal:"));
        assert!(!after.contains("Public surface:"));
        assert!(!after.contains("- foo"));
        assert!(after.contains("Trailing notes."));
    }

    #[test]
    fn fix_preserves_safety_comment() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
// SAFETY: ──────── invariant: ptr non-null
fn x() {}
";
        let path = write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        let _ = fix_comments(dir.path(), &opts).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("SAFETY:"));
    }

    #[test]
    fn fix_does_not_touch_code_lines() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
fn x() {
    let s = \"────────────────────\";
    println!(\"{s}\");
}
";
        let path = write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        let report = fix_comments(dir.path(), &opts).unwrap();
        assert_eq!(report.files_fixed, 0);
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, body);
    }

    #[test]
    fn fix_skips_markdown_files() {
        let dir = tempfile::tempdir().unwrap();
        let body = "## Title\n\nOur revolutionary platform.\n";
        let path = write(dir.path(), "doc.md", body);
        let opts = HygieneOptions::for_root(dir.path());
        let _ = fix_comments(dir.path(), &opts).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, body, "markdown is never auto-fixed");
    }

    #[test]
    fn fix_drops_banner_in_toml() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
# ─────────────────────────────
# Helpers
key = \"v\"
";
        let path = write(dir.path(), "config.toml", body);
        let opts = HygieneOptions::for_root(dir.path());
        let report = fix_comments(dir.path(), &opts).unwrap();
        assert_eq!(report.files_fixed, 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("─"));
        assert!(after.contains("# Helpers"));
        assert!(after.contains("key = \"v\""));
    }
}
