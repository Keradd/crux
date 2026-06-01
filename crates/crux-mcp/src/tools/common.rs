use std::path::{Path, PathBuf};

use crate::dispatch::AppContext;

pub fn project_root(ctx: &AppContext) -> Option<String> {
    ctx.project_root.as_ref().map(|p| p.display().to_string())
}

pub fn project_root_path(ctx: &AppContext) -> Option<PathBuf> {
    ctx.project_root.clone()
}

pub fn resolve_in_project(project_root: &Path, user_path: &str) -> Result<PathBuf, String> {
    let cr = project_root
        .canonicalize()
        .map_err(|e| format!("bad project root: {e}"))?;
    let target = Path::new(user_path);
    let abs = if target.is_relative() {
        cr.join(target)
    } else {
        target.to_path_buf()
    };
    let ct = abs
        .canonicalize()
        .map_err(|e| format!("path '{}' cannot be resolved: {e}", user_path))?;
    if ct.starts_with(&cr) {
        Ok(ct)
    } else {
        Err(format!(
            "path '{}' escapes project root '{}'",
            user_path,
            project_root.display()
        ))
    }
}

pub fn truncate_one_line(s: &str, n: usize) -> String {
    let line = s.lines().next().unwrap_or(s);
    if line.len() <= n {
        line.to_string()
    } else {
        format!("{}\u{2026}", &line[..n])
    }
}

pub fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

pub fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

pub fn file_path_variants(project_path: &Path, file_path: &str) -> Vec<String> {
    let mut out = vec![file_path.to_string()];
    let as_path = Path::new(file_path);
    if as_path.is_absolute() {
        if let Ok(rel) = as_path.strip_prefix(project_path) {
            let rel_s = rel.display().to_string();
            if rel_s != file_path {
                out.push(rel_s);
            }
        }
    } else {
        let abs = project_path.join(file_path).display().to_string();
        if abs != file_path {
            out.push(abs);
        }
    }
    out
}

pub fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

pub fn serialize_nodes(nodes: &[crux_l5_ast::GraphNode]) -> serde_json::Value {
    serde_json::Value::Array(
        nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "kind": n.kind.as_str(),
                    "name": n.name,
                    "qualified_name": n.qualified_name,
                    "file_path": n.file_path,
                    "line_start": n.line_start,
                    "line_end": n.line_end,
                    "signature": n.signature,
                    "is_test": n.is_test,
                })
            })
            .collect(),
    )
}
