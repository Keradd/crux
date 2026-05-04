use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::scanner::{classify_lines, detect_lang, is_comment_kind, is_generated, walk, Lang};
use crate::types::{HygieneOptions, StripReport};

pub fn strip_comments(root: &Path, options: &HygieneOptions) -> io::Result<StripReport> {
    let mut report = StripReport::default();
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
        if !lang_is_strippable(lang) {
            continue;
        }
        let (new_content, removed) = strip_in_source(&content, lang);
        if removed > 0 && new_content != content {
            fs::write(file, &new_content)?;
            report.files_stripped += 1;
            report.lines_removed += removed;
            report.stripped_files.push(file.clone());
        }
    }
    Ok(report)
}

fn lang_is_strippable(lang: Lang) -> bool {
    matches!(lang, Lang::Rust | Lang::JsTs)
}

fn strip_in_source(src: &str, lang: Lang) -> (String, usize) {
    let lines = classify_lines(src, lang);
    let mut drop = vec![false; lines.len()];

    let mut preserve_doc_run = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        use crate::scanner::LineKind::*;
        let kind = lines[i].kind.clone();
        if !matches!(kind, DocComment | ModuleDoc) {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].kind == kind {
            i += 1;
        }
        let end = i;
        let has_fence = (start..end).any(|k| {
            let b = lines[k].body.trim_start();
            b.starts_with("```") || b.starts_with("~~~")
        });
        if has_fence {
            preserve_doc_run
                .iter_mut()
                .take(end)
                .skip(start)
                .for_each(|p| *p = true);
        }
    }

    for (i, ln) in lines.iter().enumerate() {
        if !is_comment_kind(&ln.kind) {
            continue;
        }
        if preserve_doc_run[i] {
            continue;
        }
        if is_strip_critical(&ln.body) {
            continue;
        }
        drop[i] = true;
    }

    let mut removed = drop.iter().filter(|d| **d).count();
    if removed == 0 {
        return (src.to_string(), 0);
    }

    let mut kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !drop[*i])
        .map(|(_, l)| l.raw.as_str())
        .collect();

    while kept.first().map(|s| s.trim().is_empty()).unwrap_or(false) {
        kept.remove(0);
        removed += 1;
    }

    let mut collapsed: Vec<&str> = Vec::with_capacity(kept.len());
    let mut last_blank = false;
    for line in kept {
        let blank = line.trim().is_empty();
        if blank && last_blank {
            removed += 1;
            continue;
        }
        collapsed.push(line);
        last_blank = blank;
    }

    let mut out = collapsed.join("\n");
    if src.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    (out, removed)
}

fn is_strip_critical(body: &str) -> bool {
    let lower = body.trim_start().to_ascii_lowercase();
    lower.starts_with("safety:")
        || lower.starts_with("security:")
        || lower.starts_with("warning:")
        || lower.starts_with("invariant:")
        || lower.starts_with("todo:")
        || lower.starts_with("fixme:")
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
    fn strip_removes_line_and_module_comments() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
//! Module doc.
//! More.

// inline comment
fn main() {
    // body comment
    let x = 1;
}
";
        write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        let report = strip_comments(dir.path(), &opts).unwrap();
        assert_eq!(report.files_stripped, 1);
        let after = std::fs::read_to_string(dir.path().join("src.rs")).unwrap();
        assert!(!after.contains("//"));
        assert!(after.contains("fn main()"));
        assert!(after.contains("let x = 1;"));
    }

    #[test]
    fn strip_preserves_safety_comment() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
fn raw() {
    // SAFETY: ptr is valid for read of T.
    unsafe { *std::ptr::null::<u32>() };
}
";
        write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        strip_comments(dir.path(), &opts).unwrap();
        let after = std::fs::read_to_string(dir.path().join("src.rs")).unwrap();
        assert!(after.contains("// SAFETY:"));
    }

    #[test]
    fn strip_preserves_security_warning_invariant_todo_fixme() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
fn f() {
    // SECURITY: checks the token.
    // WARNING: not thread-safe.
    // invariant: len == cap.
    // TODO: wire this up.
    // FIXME: refactor.
    // just a normal note
}
";
        write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        strip_comments(dir.path(), &opts).unwrap();
        let after = std::fs::read_to_string(dir.path().join("src.rs")).unwrap();
        assert!(after.contains("SECURITY:"));
        assert!(after.contains("WARNING:"));
        assert!(after.contains("invariant:"));
        assert!(after.contains("TODO:"));
        assert!(after.contains("FIXME:"));
        assert!(!after.contains("just a normal note"));
    }

    #[test]
    fn strip_preserves_doctest_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
/// Adds two numbers.
///
/// ```
/// assert_eq!(my_crate::add(1, 2), 3);
/// ```
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Plain prose without a fence.
pub fn noop() {}
";
        write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        strip_comments(dir.path(), &opts).unwrap();
        let after = std::fs::read_to_string(dir.path().join("src.rs")).unwrap();
        assert!(after.contains("Adds two numbers."));
        assert!(after.contains("```"));
        assert!(after.contains("assert_eq!(my_crate::add(1, 2), 3)"));
        assert!(!after.contains("Plain prose without a fence."));
    }

    #[test]
    fn strip_does_not_touch_code_lines() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
// drop me
fn f() -> &'static str {
    \"// not a comment\"
}
";
        write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        strip_comments(dir.path(), &opts).unwrap();
        let after = std::fs::read_to_string(dir.path().join("src.rs")).unwrap();
        assert!(after.contains("\"// not a comment\""));
        assert!(!after.contains("// drop me"));
    }

    #[test]
    fn strip_leaves_string_literal_contents_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let body = "fn fixture() {\n    let body = \"\\\n//! CRUX Layer 10\n// banner inside string\n\";\n    drop(body);\n}\n";
        write(dir.path(), "src.rs", body);
        let before = std::fs::read_to_string(dir.path().join("src.rs")).unwrap();
        let opts = HygieneOptions::for_root(dir.path());
        let report = strip_comments(dir.path(), &opts).unwrap();
        assert_eq!(report.files_stripped, 0);
        let after = std::fs::read_to_string(dir.path().join("src.rs")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn strip_collapses_blank_runs_and_leading_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
// banner 1
// banner 2
// banner 3

fn f() {
    let x = 1;


    let y = 2;
}
";
        write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        strip_comments(dir.path(), &opts).unwrap();
        let after = std::fs::read_to_string(dir.path().join("src.rs")).unwrap();
        assert!(after.starts_with("fn f()"));
        assert!(!after.contains("\n\n\n"));
    }

    #[test]
    fn strip_ignores_non_rust_files() {
        let dir = tempfile::tempdir().unwrap();
        let before = "# header\nkey = \"v\"\n";
        write(dir.path(), "config.toml", before);
        let opts = HygieneOptions::for_root(dir.path());
        strip_comments(dir.path(), &opts).unwrap();
        let after = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn strip_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\
//! drop me
// drop me too
fn f() {}
";
        write(dir.path(), "src.rs", body);
        let opts = HygieneOptions::for_root(dir.path());
        let r1 = strip_comments(dir.path(), &opts).unwrap();
        let r2 = strip_comments(dir.path(), &opts).unwrap();
        assert_eq!(r1.files_stripped, 1);
        assert_eq!(r2.files_stripped, 0);
    }
}
