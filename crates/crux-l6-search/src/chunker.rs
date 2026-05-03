//! Build [`Chunk`]s from on-disk artefacts.
//!
//! Two flavors today:
//! - [`chunks_from_ast`] reads `ast_nodes` (already produced by Layer 5)
//!   and slices the underlying source file into one chunk per indexable
//!   symbol (functions, methods, classes, types, constants).
//! - [`chunks_from_prose`] walks markdown / text files and breaks each
//!   into paragraph-sized chunks.
//!
//! Both flavors produce content suitable for both BM25 and dense
//! embedding paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crux_core::error::Result;

use crate::types::{Chunk, ContentType};

const PROSE_EXTENSIONS: &[&str] = &["md", "markdown", "txt", "rst", "mdx"];
const MAX_PROSE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PARAGRAPH_CHARS: usize = 4000;
const MIN_PARAGRAPH_CHARS: usize = 16;
/// Filenames owned by the memory scanner, not the prose walker. Skipped
/// inside the generic prose walk so the two pipelines don't double-index
/// the same file under different content types.
const MEMORY_RESERVED_FILENAMES: &[&str] = &["CLAUDE.md", "MEMORY.md", "CLAUDE.local.md"];

/// Build chunks for every indexable AST node belonging to `project_root`.
///
/// Reads from `ast_nodes` directly so the Layer 5 graph remains the
/// single source of truth for symbol metadata.
pub fn chunks_from_ast(conn: &Connection, project_root: &Path) -> Result<Vec<Chunk>> {
    chunks_from_ast_filtered(conn, project_root, None)
}

/// Same as [`chunks_from_ast`] but skips any node whose `file_path` is
/// not in `only` (when `Some`). `None` disables the filter. Used by the
/// reindexer after Merkle diff to avoid re-chunking unchanged files.
pub fn chunks_from_ast_filtered(
    conn: &Connection,
    project_root: &Path,
    only: Option<&HashSet<String>>,
) -> Result<Vec<Chunk>> {
    let key = project_root.display().to_string();
    let mut stmt = conn.prepare(
        "SELECT id, kind, name, qualified_name, file_path,
                line_start, line_end, language, signature
           FROM ast_nodes
          WHERE project_root = ?
            AND kind IN ('Function','Method','Class','Type','Constant','Module')
          ORDER BY file_path, line_start",
    )?;
    let rows = stmt.query_map(params![key], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)? as u32,
            row.get::<_, i64>(6)? as u32,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })?;

    let mut out = Vec::new();
    let mut current_file: Option<(String, Vec<String>)> = None;
    for row in rows {
        let (
            ast_id,
            kind,
            name,
            qualified_name,
            file_path,
            line_start,
            line_end,
            language,
            signature,
        ) = row?;

        if let Some(set) = only {
            if !set.contains(&file_path) {
                continue;
            }
        }

        // Cache the most recently read file so consecutive nodes from the
        // same file don't trigger repeated IO.
        let lines = match &current_file {
            Some((p, ls)) if p == &file_path => ls.clone(),
            _ => {
                let abs = project_root.join(&file_path);
                let body = match std::fs::read_to_string(&abs) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let ls: Vec<String> = body.lines().map(|s| s.to_string()).collect();
                current_file = Some((file_path.clone(), ls.clone()));
                ls
            }
        };

        let lo = line_start.saturating_sub(1) as usize;
        let hi = (line_end as usize).min(lines.len());
        if lo >= hi {
            continue;
        }
        let body = lines[lo..hi].join("\n");
        let title = format!(
            "{} {}{}",
            kind,
            name,
            signature
                .as_deref()
                .map(|s| format!(" — {}", s.lines().next().unwrap_or("")))
                .unwrap_or_default()
        );
        let mut content = String::new();
        content.push_str(&qualified_name);
        content.push('\n');
        if let Some(s) = &signature {
            content.push_str(s.lines().next().unwrap_or(""));
            content.push('\n');
        }
        content.push_str(&body);

        out.push(Chunk {
            project_root: key.clone(),
            source_id: Some(ast_id),
            file_path,
            language,
            content_type: ContentType::Code,
            title: Some(title),
            content,
            line_start,
            line_end,
        });
    }
    Ok(out)
}

/// Walk a directory and emit prose chunks (markdown, text, …).
pub fn chunks_from_prose(project_root: &Path) -> Result<Vec<Chunk>> {
    chunks_from_prose_filtered(project_root, None)
}

/// Same as [`chunks_from_prose`] but skips any file whose
/// project-relative path is not in `only` (when `Some`). Used by the
/// reindexer after Merkle diff.
pub fn chunks_from_prose_filtered(
    project_root: &Path,
    only: Option<&HashSet<String>>,
) -> Result<Vec<Chunk>> {
    let key = project_root.display().to_string();
    let mut out = Vec::new();
    walk(project_root, project_root, &key, only, &mut out)?;
    Ok(out)
}

/// List the project-relative paths of every prose file that
/// [`chunks_from_prose`] would visit (extension match, size under
/// `MAX_PROSE_BYTES`, skipping hidden / target / node_modules dirs).
pub fn list_prose_files(project_root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    list_prose_walk(project_root, project_root, &mut out)?;
    out.sort();
    Ok(out)
}

/// Project-relative paths referenced by the Layer 5 graph for
/// `project_root`. Feeds `MerkleSync::scan` so the reindexer can diff
/// every source file that could contribute code chunks.
pub fn list_ast_files(conn: &Connection, project_root: &Path) -> Result<Vec<String>> {
    let key = project_root.display().to_string();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT file_path
           FROM ast_nodes
          WHERE project_root = ?
            AND kind IN ('Function','Method','Class','Type','Constant','Module')
          ORDER BY file_path",
    )?;
    let rows = stmt.query_map(params![key], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Absolute paths to every memory document the scanner would read for
/// this project. Order is stable: project-scope first, then home-scope.
///
/// Sources, in order:
/// 1. `<project>/CLAUDE.md`, `<project>/MEMORY.md`, `<project>/CLAUDE.local.md`
/// 2. `<project>/.crux/memory/*.md`
/// 3. `<crux_home>/CLAUDE.md`, `<crux_home>/MEMORY.md`
/// 4. `<crux_home>/memory/*.md`
///
/// Non-existent entries are silently skipped. Caller treats the result
/// as opaque absolute paths suitable for `MerkleSync::scan`; returned
/// strings are deduplicated while preserving the order above.
pub fn list_memory_files(project_root: &Path, crux_home: Option<&Path>) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push_if_file = |p: PathBuf, out: &mut Vec<String>| {
        if !p.is_file() {
            return;
        }
        if let Ok(meta) = p.metadata() {
            if meta.len() > MAX_PROSE_BYTES {
                return;
            }
        }
        let key = p.display().to_string();
        if seen.insert(key.clone()) {
            out.push(key);
        }
    };

    // Project-scope singletons.
    for name in MEMORY_RESERVED_FILENAMES {
        push_if_file(project_root.join(name), &mut out);
    }
    // Project-scope memory/*.md directory.
    if let Ok(entries) = std::fs::read_dir(project_root.join(".crux").join("memory")) {
        let mut dir_out = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if is_prose(&p) {
                dir_out.push(p);
            }
        }
        dir_out.sort();
        for p in dir_out {
            push_if_file(p, &mut out);
        }
    }

    if let Some(home) = crux_home {
        for name in MEMORY_RESERVED_FILENAMES {
            push_if_file(home.join(name), &mut out);
        }
        if let Ok(entries) = std::fs::read_dir(home.join("memory")) {
            let mut dir_out = Vec::new();
            for entry in entries.flatten() {
                let p = entry.path();
                if is_prose(&p) {
                    dir_out.push(p);
                }
            }
            dir_out.sort();
            for p in dir_out {
                push_if_file(p, &mut out);
            }
        }
    }

    Ok(out)
}

/// Read every memory document listed by [`list_memory_files`] and emit
/// paragraph-sized chunks tagged with [`ContentType::Memory`]. When
/// `only` is `Some(set)`, files not in the set are skipped — this is
/// how the Merkle reindexer avoids re-chunking unchanged memory files.
/// The chunk's `file_path` is the absolute path (stable across projects).
pub fn chunks_from_memory_filtered(
    project_root: &Path,
    crux_home: Option<&Path>,
    only: Option<&HashSet<String>>,
) -> Result<Vec<Chunk>> {
    let key = project_root.display().to_string();
    let sources = list_memory_files(project_root, crux_home)?;
    let mut out = Vec::new();
    for abs_key in sources {
        if let Some(set) = only {
            if !set.contains(&abs_key) {
                continue;
            }
        }
        let body = match std::fs::read_to_string(&abs_key) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for chunk in split_text(&body, &abs_key, &key, ContentType::Memory) {
            out.push(chunk);
        }
    }
    Ok(out)
}

/// Convenience wrapper that scans every memory file without filtering.
pub fn chunks_from_memory(project_root: &Path, crux_home: Option<&Path>) -> Result<Vec<Chunk>> {
    chunks_from_memory_filtered(project_root, crux_home, None)
}

fn walk(
    root: &Path,
    dir: &Path,
    project_key: &str,
    only: Option<&HashSet<String>>,
    out: &mut Vec<Chunk>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        let ft = match entry.file_type() {
            Ok(f) => f,
            Err(_) => continue,
        };
        if ft.is_dir() {
            walk(root, &path, project_key, only, out)?;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        if !is_prose(&path) {
            continue;
        }
        if MEMORY_RESERVED_FILENAMES.contains(&name.as_str()) {
            // Owned by the memory scanner.
            continue;
        }
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > MAX_PROSE_BYTES {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        if let Some(set) = only {
            if !set.contains(&rel) {
                continue;
            }
        }
        let body = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for chunk in split_text(&body, &rel, project_key, ContentType::Prose) {
            out.push(chunk);
        }
    }
    Ok(())
}

fn list_prose_walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        let ft = match entry.file_type() {
            Ok(f) => f,
            Err(_) => continue,
        };
        if ft.is_dir() {
            list_prose_walk(root, &path, out)?;
            continue;
        }
        if !ft.is_file() || !is_prose(&path) {
            continue;
        }
        if MEMORY_RESERVED_FILENAMES.contains(&name.as_str()) {
            continue;
        }
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > MAX_PROSE_BYTES {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push(rel);
    }
    Ok(())
}

fn is_prose(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| PROSE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn split_text(
    body: &str,
    file_path: &str,
    project_key: &str,
    content_type: ContentType,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    // Track 1-indexed line numbers as we walk.
    let mut buf = String::new();
    let mut buf_start_line: u32 = 1;
    let mut current_line: u32 = 1;

    let flush = |buf: &mut String, lo: u32, hi: u32, chunks: &mut Vec<Chunk>| {
        let trimmed = buf.trim();
        if trimmed.len() >= MIN_PARAGRAPH_CHARS {
            let title = trimmed
                .lines()
                .next()
                .map(|l| {
                    l.trim_start_matches(|c: char| c == '#' || c.is_whitespace())
                        .to_string()
                })
                .filter(|s| !s.is_empty());
            chunks.push(Chunk {
                project_root: project_key.to_string(),
                source_id: None,
                file_path: file_path.to_string(),
                language: None,
                content_type,
                title,
                content: trimmed.to_string(),
                line_start: lo,
                line_end: hi,
            });
        }
        buf.clear();
    };

    for line in body.split_inclusive('\n') {
        if line.trim().is_empty() {
            // Paragraph boundary.
            flush(
                &mut buf,
                buf_start_line,
                current_line.saturating_sub(1),
                &mut chunks,
            );
            current_line += 1;
            buf_start_line = current_line;
            continue;
        }
        if buf.is_empty() {
            buf_start_line = current_line;
        }
        if buf.len() + line.len() > MAX_PARAGRAPH_CHARS {
            flush(
                &mut buf,
                buf_start_line,
                current_line.saturating_sub(1),
                &mut chunks,
            );
            buf_start_line = current_line;
        }
        buf.push_str(line);
        current_line += 1;
    }
    flush(
        &mut buf,
        buf_start_line,
        current_line.saturating_sub(1),
        &mut chunks,
    );

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_prose_preserves_paragraphs() {
        let md = "# Title\n\nFirst paragraph that is long enough to be kept.\n\nSecond paragraph also long enough to retain.\n";
        let chunks = split_text(md, "README.md", "/tmp/p", ContentType::Prose);
        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks, got {:?}",
            chunks.len()
        );
        assert!(chunks[0].title.is_some());
        assert_eq!(chunks[0].content_type, ContentType::Prose);
    }

    #[test]
    fn very_short_lines_are_dropped() {
        let md = "ok\n";
        let chunks = split_text(md, "x.md", "/tmp/p", ContentType::Prose);
        assert!(chunks.is_empty());
    }

    #[test]
    fn split_text_honors_content_type() {
        let md = "# Rules\n\nAgent must be terse when caveman mode is on.\n";
        let chunks = split_text(md, "CLAUDE.md", "/tmp/p", ContentType::Memory);
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| c.content_type == ContentType::Memory));
    }

    #[test]
    fn chunks_from_prose_walks_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("notes.md"),
            "# My doc\n\nLong enough paragraph one body. Has plenty of content.\n",
        )
        .unwrap();
        let chunks = chunks_from_prose(dir.path()).unwrap();
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].content_type, ContentType::Prose);
    }

    #[test]
    fn memory_scanner_reads_project_and_home_sources() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("CLAUDE.md"),
            "# Project rules\n\nAgent must prefer minimal diffs when fixing bugs.\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.path().join(".crux").join("memory")).unwrap();
        std::fs::write(
            project.path().join(".crux").join("memory").join("notes.md"),
            "# Notes\n\nProject-local memory note that is long enough to index.\n",
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join("memory")).unwrap();
        std::fs::write(
            home.path().join("memory").join("global.md"),
            "# Global\n\nGlobal agent memory surviving across projects here.\n",
        )
        .unwrap();

        let files = list_memory_files(project.path(), Some(home.path())).unwrap();
        assert_eq!(files.len(), 3, "expected 3 memory sources, got {:?}", files);

        let chunks = chunks_from_memory(project.path(), Some(home.path())).unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| c.content_type == ContentType::Memory));
    }

    #[test]
    fn memory_scanner_filters_by_allowset() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("CLAUDE.md"),
            "# Project rules\n\nRule body one that is long enough to be indexed.\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join("MEMORY.md"),
            "# Memory\n\nSticky memory content body one that also exceeds the gate.\n",
        )
        .unwrap();

        let files = list_memory_files(project.path(), None).unwrap();
        assert_eq!(files.len(), 2);

        // Only allow CLAUDE.md through.
        let claude = project.path().join("CLAUDE.md").display().to_string();
        let allow: HashSet<String> = [claude.clone()].into_iter().collect();
        let chunks = chunks_from_memory_filtered(project.path(), None, Some(&allow)).unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| c.file_path == claude));
    }

    #[test]
    fn prose_walker_skips_memory_reserved_filenames() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("CLAUDE.md"),
            "# Rules\n\nAgent memory content that should be tracked separately.\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("notes.md"),
            "# Notes\n\nGeneric note paragraph that the prose walker should keep.\n",
        )
        .unwrap();
        let paths = list_prose_files(dir.path()).unwrap();
        assert_eq!(paths, vec!["notes.md".to_string()]);
        let chunks = chunks_from_prose(dir.path()).unwrap();
        assert!(chunks.iter().all(|c| c.file_path != "CLAUDE.md"));
    }
}
