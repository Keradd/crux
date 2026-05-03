//! File-level Merkle-ish sync shared by every layer that walks the
//! project tree.
//!
//! The reindexer used to re-walk every project file on every call and
//! lean on downstream content-hash dedup to short-circuit inserts.
//! [`MerkleSync`] compares the on-disk SHA-256 of each tracked file
//! against the last committed snapshot in the `file_snapshots` table
//! and tells the caller exactly which paths are added / modified /
//! removed / unchanged.
//!
//! Multiple layers (Layer 5 AST ingestion, Layer 6 chunk indexing, …)
//! share the table; each identifies its own view with a `scope` string
//! (`"ast"`, `"chunks"`, …). Rows for different scopes don't interact.
//!
//! Design doc: §7.6 ("Merkle sync"). The claude-context reference uses
//! a full DAG keyed by path; we collapse that to a flat per-file hash
//! map because the retrieval unit we care about is the file, not an
//! interior tree node.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::error::Result;

/// Well-known scope for Layer 6 (chunks + embeddings).
pub const SCOPE_CHUNKS: &str = "chunks";
/// Well-known scope for Layer 5 (AST graph ingestion).
pub const SCOPE_AST: &str = "ast";

/// Persisted state for a single tracked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSnapshot {
    /// Project-relative path (forward slashes are not enforced — we
    /// store whatever the caller supplied).
    pub file_path: String,
    /// Hex-encoded SHA-256 of the file bytes.
    pub content_hash: String,
    pub size_bytes: u64,
    pub mtime_epoch: i64,
}

/// Result of comparing a fresh scan against the stored snapshot.
#[derive(Debug, Default, Clone)]
pub struct FileChangeSet {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: Vec<String>,
}

impl FileChangeSet {
    /// Any path that either did not exist in the previous snapshot or
    /// changed content. Callers use this to scope downstream work.
    pub fn changed(&self) -> HashSet<String> {
        let mut out = HashSet::with_capacity(self.added.len() + self.modified.len());
        out.extend(self.added.iter().cloned());
        out.extend(self.modified.iter().cloned());
        out
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }
}

/// Per-(project, scope) snapshot manager backed by the `file_snapshots`
/// table.
pub struct MerkleSync<'c> {
    conn: &'c Connection,
    project_root: String,
    scope: String,
}

impl<'c> MerkleSync<'c> {
    /// Create a handle for the given project and scope. Use the
    /// `SCOPE_CHUNKS` / `SCOPE_AST` constants (or any other stable
    /// string) to separate layer namespaces.
    pub fn new(conn: &'c Connection, project_root: &Path, scope: &str) -> Self {
        Self {
            conn,
            project_root: project_root.display().to_string(),
            scope: scope.to_string(),
        }
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Hash a single file on disk. Returns `Ok(None)` when the path does
    /// not resolve to a regular file or the read fails (the caller
    /// treats that the same as "file does not exist").
    pub fn hash_file(abs: &Path) -> Result<Option<FileSnapshot>> {
        let meta = match std::fs::metadata(abs) {
            Ok(m) => m,
            Err(_) => return Ok(None),
        };
        if !meta.is_file() {
            return Ok(None);
        }
        let body = match std::fs::read(abs) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    path = %abs.display(),
                    error = %e,
                    "merkle: read failed; treating as missing"
                );
                return Ok(None);
            }
        };
        let mut h = Sha256::new();
        h.update(&body);
        let content_hash = hex::encode(h.finalize());
        let mtime_epoch = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Ok(Some(FileSnapshot {
            file_path: String::new(),
            content_hash,
            size_bytes: meta.len(),
            mtime_epoch,
        }))
    }

    /// Hash the given project-relative paths. Paths that cannot be read
    /// are silently skipped — they will surface as `removed` in the
    /// diff against the previous snapshot.
    pub fn scan<I, S>(
        &self,
        project_root: &Path,
        rel_paths: I,
    ) -> Result<HashMap<String, FileSnapshot>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = HashMap::new();
        for rel in rel_paths {
            let rel = rel.as_ref();
            if rel.is_empty() {
                continue;
            }
            let abs = project_root.join(rel);
            if let Some(mut snap) = Self::hash_file(&abs)? {
                snap.file_path = rel.to_string();
                out.insert(rel.to_string(), snap);
            }
        }
        Ok(out)
    }

    /// Load the last committed snapshot for this project + scope.
    pub fn load(&self) -> Result<HashMap<String, FileSnapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_path, content_hash, size_bytes, mtime_epoch
               FROM file_snapshots
              WHERE project_root = ? AND scope = ?",
        )?;
        let rows = stmt.query_map(params![self.project_root, self.scope], |r| {
            Ok(FileSnapshot {
                file_path: r.get::<_, String>(0)?,
                content_hash: r.get::<_, String>(1)?,
                size_bytes: r.get::<_, i64>(2)? as u64,
                mtime_epoch: r.get::<_, i64>(3)?,
            })
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let s = row?;
            map.insert(s.file_path.clone(), s);
        }
        Ok(map)
    }

    /// Fetch the stored hash for a single path, if any. Cheap enough to
    /// call inside the per-file loop of an incremental indexer.
    pub fn hash_for(&self, file_path: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT content_hash FROM file_snapshots
                  WHERE project_root = ? AND scope = ? AND file_path = ?",
                params![self.project_root, self.scope, file_path],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Diff a fresh scan against a stored snapshot. Pure function —
    /// does not touch the database.
    pub fn diff(
        current: &HashMap<String, FileSnapshot>,
        stored: &HashMap<String, FileSnapshot>,
    ) -> FileChangeSet {
        let mut set = FileChangeSet::default();
        for (path, cur) in current {
            match stored.get(path) {
                None => set.added.push(path.clone()),
                Some(old) if old.content_hash != cur.content_hash => {
                    set.modified.push(path.clone())
                }
                Some(_) => set.unchanged.push(path.clone()),
            }
        }
        for path in stored.keys() {
            if !current.contains_key(path) {
                set.removed.push(path.clone());
            }
        }
        set.added.sort();
        set.modified.sort();
        set.removed.sort();
        set.unchanged.sort();
        set
    }

    /// Upsert every entry in `current` into `file_snapshots`. Rows for
    /// paths not present in `current` are left alone — call
    /// [`MerkleSync::remove`] or [`MerkleSync::purge`] for those.
    pub fn commit(&self, current: &HashMap<String, FileSnapshot>) -> Result<()> {
        if current.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        let tx = self.conn.unchecked_transaction()?;
        for snap in current.values() {
            tx.execute(
                r#"INSERT INTO file_snapshots
                       (project_root, scope, file_path, content_hash,
                        size_bytes, mtime_epoch, indexed_at_epoch)
                   VALUES (?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(project_root, scope, file_path) DO UPDATE SET
                       content_hash     = excluded.content_hash,
                       size_bytes       = excluded.size_bytes,
                       mtime_epoch      = excluded.mtime_epoch,
                       indexed_at_epoch = excluded.indexed_at_epoch"#,
                params![
                    self.project_root,
                    self.scope,
                    snap.file_path,
                    snap.content_hash,
                    snap.size_bytes as i64,
                    snap.mtime_epoch,
                    now,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Upsert a single snapshot row. Handy for incremental indexers
    /// that want to commit as they go instead of batching.
    pub fn commit_one(&self, snap: &FileSnapshot) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            r#"INSERT INTO file_snapshots
                   (project_root, scope, file_path, content_hash,
                    size_bytes, mtime_epoch, indexed_at_epoch)
               VALUES (?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(project_root, scope, file_path) DO UPDATE SET
                   content_hash     = excluded.content_hash,
                   size_bytes       = excluded.size_bytes,
                   mtime_epoch      = excluded.mtime_epoch,
                   indexed_at_epoch = excluded.indexed_at_epoch"#,
            params![
                self.project_root,
                self.scope,
                snap.file_path,
                snap.content_hash,
                snap.size_bytes as i64,
                snap.mtime_epoch,
                now,
            ],
        )?;
        Ok(())
    }

    /// Remove the named paths from this project's scope-partitioned
    /// snapshot.
    pub fn remove(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for p in paths {
            tx.execute(
                "DELETE FROM file_snapshots
                  WHERE project_root = ? AND scope = ? AND file_path = ?",
                params![self.project_root, self.scope, p],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop the entire snapshot for this project + scope. Used by
    /// `--force`.
    pub fn purge(&self) -> Result<()> {
        self.conn.execute(
            "DELETE FROM file_snapshots WHERE project_root = ? AND scope = ?",
            params![self.project_root, self.scope],
        )?;
        Ok(())
    }

    pub fn count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM file_snapshots
              WHERE project_root = ? AND scope = ?",
            params![self.project_root, self.scope],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let abs = dir.join(rel);
        if let Some(p) = abs.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(abs, body).unwrap();
    }

    #[test]
    fn hash_file_handles_missing() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope.txt");
        assert!(MerkleSync::hash_file(&missing).unwrap().is_none());
    }

    #[test]
    fn scan_and_diff_classifies_changes() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.md", "alpha one");
        write(dir.path(), "b.md", "bravo two");
        write(dir.path(), "sub/c.md", "charlie");

        let conn = crate::db::open_in_memory().unwrap();
        let sync = MerkleSync::new(&conn, dir.path(), SCOPE_CHUNKS);

        let first = sync.scan(dir.path(), ["a.md", "b.md", "sub/c.md"]).unwrap();
        assert_eq!(first.len(), 3);
        sync.commit(&first).unwrap();

        // No changes yet.
        let reloaded = sync.load().unwrap();
        let diff_same = MerkleSync::diff(&first, &reloaded);
        assert!(diff_same.is_empty());
        assert_eq!(diff_same.unchanged.len(), 3);

        // Modify one, add one, remove one.
        write(dir.path(), "b.md", "bravo TWO edited");
        write(dir.path(), "d.md", "delta");
        std::fs::remove_file(dir.path().join("a.md")).unwrap();

        let second = sync
            .scan(dir.path(), ["a.md", "b.md", "sub/c.md", "d.md"])
            .unwrap();
        let diff = MerkleSync::diff(&second, &reloaded);
        assert_eq!(diff.added, vec!["d.md"]);
        assert_eq!(diff.modified, vec!["b.md"]);
        assert_eq!(diff.removed, vec!["a.md"]);
        assert_eq!(diff.unchanged, vec!["sub/c.md"]);
        assert_eq!(
            diff.changed(),
            ["b.md", "d.md"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn commit_and_remove_round_trip() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.md", "alpha");
        write(dir.path(), "b.md", "bravo");
        let conn = crate::db::open_in_memory().unwrap();
        let sync = MerkleSync::new(&conn, dir.path(), SCOPE_CHUNKS);

        let scan = sync.scan(dir.path(), ["a.md", "b.md"]).unwrap();
        sync.commit(&scan).unwrap();
        assert_eq!(sync.count().unwrap(), 2);

        sync.remove(&["a.md".to_string()]).unwrap();
        assert_eq!(sync.count().unwrap(), 1);
        let left = sync.load().unwrap();
        assert!(left.contains_key("b.md"));
        assert!(!left.contains_key("a.md"));

        sync.purge().unwrap();
        assert_eq!(sync.count().unwrap(), 0);
    }

    #[test]
    fn commit_is_idempotent_for_unchanged_content() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.md", "alpha");
        let conn = crate::db::open_in_memory().unwrap();
        let sync = MerkleSync::new(&conn, dir.path(), SCOPE_CHUNKS);

        let scan = sync.scan(dir.path(), ["a.md"]).unwrap();
        sync.commit(&scan).unwrap();
        sync.commit(&scan).unwrap();
        assert_eq!(sync.count().unwrap(), 1);
    }

    #[test]
    fn scan_skips_missing_paths_silently() {
        let dir = tempdir().unwrap();
        write(dir.path(), "real.md", "x");
        let conn = crate::db::open_in_memory().unwrap();
        let sync = MerkleSync::new(&conn, dir.path(), SCOPE_CHUNKS);
        let scan = sync.scan(dir.path(), ["real.md", "ghost.md"]).unwrap();
        assert_eq!(scan.len(), 1);
        assert!(scan.contains_key("real.md"));
    }

    #[test]
    fn scopes_do_not_collide() {
        let dir = tempdir().unwrap();
        write(dir.path(), "shared.md", "same content");
        let conn = crate::db::open_in_memory().unwrap();
        let ast = MerkleSync::new(&conn, dir.path(), SCOPE_AST);
        let chunks = MerkleSync::new(&conn, dir.path(), SCOPE_CHUNKS);

        let scan = ast.scan(dir.path(), ["shared.md"]).unwrap();
        ast.commit(&scan).unwrap();
        assert_eq!(ast.count().unwrap(), 1);
        assert_eq!(chunks.count().unwrap(), 0);

        chunks.commit(&scan).unwrap();
        assert_eq!(ast.count().unwrap(), 1);
        assert_eq!(chunks.count().unwrap(), 1);

        ast.purge().unwrap();
        assert_eq!(ast.count().unwrap(), 0);
        assert_eq!(chunks.count().unwrap(), 1);
    }

    #[test]
    fn hash_for_returns_stored_hash_only_for_matching_scope() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.md", "alpha");
        let conn = crate::db::open_in_memory().unwrap();
        let ast = MerkleSync::new(&conn, dir.path(), SCOPE_AST);
        let chunks = MerkleSync::new(&conn, dir.path(), SCOPE_CHUNKS);
        let scan = ast.scan(dir.path(), ["a.md"]).unwrap();
        ast.commit(&scan).unwrap();
        assert!(ast.hash_for("a.md").unwrap().is_some());
        assert!(chunks.hash_for("a.md").unwrap().is_none());
    }
}
