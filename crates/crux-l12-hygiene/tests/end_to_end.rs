use std::fs;

use crux_l12_hygiene::{fix_comments, scan_comments, HygieneOptions};

fn write(dir: &std::path::Path, name: &str, body: &str) {
    if let Some(parent) = dir.join(name).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(dir.join(name), body).unwrap();
}

#[test]
fn scan_then_fix_then_scan_clean() {
    let dir = tempfile::tempdir().unwrap();
    let body = "\
//! Title.
//!
//! Goal: explain the thing.
//!
//! Public surface:
//!
//! - foo
//! - bar
//!
//! Trailing notes.

// ────────────────────────────────
// section A
fn a() {}
";
    write(dir.path(), "src/lib.rs", body);

    let opts = HygieneOptions {
        max_module_doc_lines: 2,
        ..HygieneOptions::for_root(dir.path())
    };

    let scan1 = scan_comments(dir.path(), &opts).unwrap();
    assert!(!scan1.is_clean(), "expected violations on first scan");
    assert!(scan1
        .violations
        .iter()
        .any(|v| v.rule_id == "decorative-banner"));
    assert!(scan1.violations.iter().any(|v| v.rule_id == "goal-section"));
    assert!(scan1
        .violations
        .iter()
        .any(|v| v.rule_id == "public-surface-section"));
    assert!(scan1
        .violations
        .iter()
        .any(|v| v.rule_id == "long-module-doc"));

    let fix = fix_comments(dir.path(), &opts).unwrap();
    assert_eq!(fix.files_fixed, 1);
    assert!(fix.lines_removed >= 5);

    let after = fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(!after.contains("Goal:"));
    assert!(!after.contains("Public surface:"));
    assert!(!after.contains("───"));
    assert!(after.contains("fn a()"));
    assert!(after.contains("// section A"));

    let scan2 = scan_comments(dir.path(), &opts).unwrap();
    assert!(
        scan2.is_clean(),
        "expected clean after fix, got: {:?}",
        scan2.violations
    );
}

#[test]
fn check_mode_clean_project_returns_no_violations() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main.rs",
        "//! One-line module doc.\nfn main() {}\n",
    );
    let opts = HygieneOptions::for_root(dir.path());
    let report = scan_comments(dir.path(), &opts).unwrap();
    assert!(report.is_clean());
    assert!(report.files_scanned >= 1);
}

#[test]
fn fix_mode_clean_project_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/main.rs", "//! Short doc.\nfn main() {}\n");
    let before = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    let opts = HygieneOptions::for_root(dir.path());
    let report = fix_comments(dir.path(), &opts).unwrap();
    assert_eq!(report.files_fixed, 0);
    let after = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn fix_preserves_comment_shaped_text_inside_string_literals() {
    let dir = tempfile::tempdir().unwrap();
    let body = "fn fixture() {\n    let body = \"\\\n//! CRUX Layer 10 — scaffolding.\n//!\n//! Goal: exercise the fixer.\n//!\n//! Public surface:\n//! - foo\n//! - bar\n//! ────────────────────────────────\nfn inner() {}\n\";\n    drop(body);\n}\n";
    write(dir.path(), "src/lib.rs", body);

    let before = fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();

    let opts = HygieneOptions {
        max_module_doc_lines: 2,
        ..HygieneOptions::for_root(dir.path())
    };

    let scan = scan_comments(dir.path(), &opts).unwrap();
    assert!(
        scan.is_clean(),
        "scanner must not flag strings-only file, got: {:?}",
        scan.violations
    );

    let fix = fix_comments(dir.path(), &opts).unwrap();
    assert_eq!(fix.files_fixed, 0, "fixer must not touch strings-only file");

    let after = fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert_eq!(
        before, after,
        "fix_comments rewrote a file that only contained comment-shaped text inside a string literal"
    );
}

#[test]
fn rules_per_file_kind_coverage() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "lib.rs",
        "//! Module doc with revolutionary tone.\nfn main() {}\n",
    );
    write(
        dir.path(),
        "config.toml",
        "# ====================================\nkey = \"v\"\n",
    );
    write(dir.path(), "doc.md", "Our cutting-edge platform.\n");
    write(
        dir.path(),
        "x.ts",
        "// Pattern adapted from foo crate\nexport const x = 1;\n",
    );
    write(
        dir.path(),
        "m.py",
        "# Layer 7 wrapper\ndef x():\n    pass\n",
    );

    let opts = HygieneOptions::for_root(dir.path());
    let report = scan_comments(dir.path(), &opts).unwrap();
    assert!(!report.is_clean());

    let ids: Vec<_> = report
        .violations
        .iter()
        .map(|v| v.rule_id.clone())
        .collect();
    assert!(ids.contains(&"marketing-phrase".to_string()));
    assert!(ids.contains(&"decorative-banner".to_string()));
    assert!(ids.contains(&"pattern-adapted-from".to_string()));
    assert!(ids.contains(&"layer-label".to_string()));
}
