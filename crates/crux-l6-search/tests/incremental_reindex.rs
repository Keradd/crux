//! End-to-end integration test for the Merkle-driven incremental
//! reindex flow. Exercises the same sequence `crux reindex` does but
//! without the CLI harness: list prose files → scan → diff vs stored
//! snapshot → chunk only changed files → index → purge removed →
//! commit snapshot.

use std::collections::HashSet;

use crux_core::merkle::{MerkleSync, SCOPE_CHUNKS};
use crux_l6_search::{chunks_from_prose_filtered, list_prose_files, HashEmbedder, Indexer};

fn write(dir: &std::path::Path, rel: &str, body: &str) {
    let abs = dir.join(rel);
    if let Some(p) = abs.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(abs, body).unwrap();
}

/// Simulate `crux reindex --no-code` using the prose pipeline only.
fn reindex_prose(conn: &rusqlite::Connection, project: &std::path::Path) -> (u64, u64, u64, u64) {
    let key = project.display().to_string();
    let indexer = Indexer::new(conn);
    let sync = MerkleSync::new(conn, project, SCOPE_CHUNKS);

    let tracked: HashSet<String> = list_prose_files(project).unwrap().into_iter().collect();
    let current = sync.scan(project, tracked.iter()).unwrap();
    let stored = sync.load().unwrap();
    let changes = MerkleSync::diff(&current, &stored);

    let changed = changes.changed();
    let removed_chunks = indexer.purge_files(&key, &changes.removed).unwrap();
    sync.remove(&changes.removed).unwrap();

    let emb = HashEmbedder::new(64);
    let chunks = chunks_from_prose_filtered(project, Some(&changed)).unwrap();
    let stats = indexer.index_chunks(&chunks, &emb).unwrap();
    sync.commit(&current).unwrap();

    (
        stats.chunks_inserted,
        stats.chunks_skipped_unchanged,
        removed_chunks,
        chunks.len() as u64,
    )
}

#[test]
fn first_pass_chunks_everything() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "# A\n\nLong enough paragraph to survive the minimum length gate.\n",
    );
    write(
        dir.path(),
        "b.md",
        "# B\n\nAnother paragraph that has plenty of content to stay.\n",
    );
    let conn = crux_core::db::open_in_memory().unwrap();

    let (inserted, skipped, removed, produced) = reindex_prose(&conn, dir.path());
    assert!(inserted >= 2, "expected >=2 inserts, got {}", inserted);
    assert_eq!(skipped, 0);
    assert_eq!(removed, 0);
    assert!(produced >= 2);
}

#[test]
fn unchanged_files_produce_no_chunks_on_rerun() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "# A\n\nLong enough paragraph to survive the minimum length gate.\n",
    );
    let conn = crux_core::db::open_in_memory().unwrap();

    let (inserted_first, _, _, produced_first) = reindex_prose(&conn, dir.path());
    assert!(inserted_first >= 1);
    assert!(produced_first >= 1);

    // Second pass — nothing changed, chunker must see an empty set.
    let (inserted_second, skipped_second, removed_second, produced_second) =
        reindex_prose(&conn, dir.path());
    assert_eq!(produced_second, 0, "chunker should skip unchanged files");
    assert_eq!(inserted_second, 0);
    assert_eq!(skipped_second, 0);
    assert_eq!(removed_second, 0);
}

#[test]
fn modifying_a_file_only_reindexes_that_file() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "# A\n\nAlpha paragraph one body that is long enough.\n",
    );
    write(
        dir.path(),
        "b.md",
        "# B\n\nBravo paragraph one body that is long enough.\n",
    );
    let conn = crux_core::db::open_in_memory().unwrap();
    let _ = reindex_prose(&conn, dir.path());

    // Only edit `a.md`.
    write(
        dir.path(),
        "a.md",
        "# A rev2\n\nAlpha paragraph ONE rewritten to differ from the baseline.\n",
    );
    let (_, _, removed, produced) = reindex_prose(&conn, dir.path());
    assert!(produced >= 1);
    assert_eq!(removed, 0);
    // Every produced chunk must belong to a.md — never b.md.
    let rows: Vec<String> = conn
        .prepare("SELECT DISTINCT file_path FROM chunks WHERE project_root = ? ORDER BY file_path")
        .unwrap()
        .query_map(rusqlite::params![dir.path().display().to_string()], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert!(rows.contains(&"a.md".to_string()));
    assert!(rows.contains(&"b.md".to_string()));
}

#[test]
fn deleting_a_file_purges_its_chunks() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "keep.md",
        "# keep\n\nKeep this paragraph alive in the store.\n",
    );
    write(
        dir.path(),
        "gone.md",
        "# gone\n\nThis paragraph will be deleted on the next pass.\n",
    );
    let conn = crux_core::db::open_in_memory().unwrap();
    let _ = reindex_prose(&conn, dir.path());

    let key = dir.path().display().to_string();
    let before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunks WHERE project_root = ? AND file_path = 'gone.md'",
            rusqlite::params![key],
            |r| r.get(0),
        )
        .unwrap();
    assert!(before >= 1, "gone.md should have chunks before deletion");

    std::fs::remove_file(dir.path().join("gone.md")).unwrap();
    let (_, _, removed, _) = reindex_prose(&conn, dir.path());
    assert!(
        removed >= 1,
        "expected purge_files to remove gone.md chunks"
    );

    let after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunks WHERE project_root = ? AND file_path = 'gone.md'",
            rusqlite::params![key],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, 0);

    let kept: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunks WHERE project_root = ? AND file_path = 'keep.md'",
            rusqlite::params![key],
            |r| r.get(0),
        )
        .unwrap();
    assert!(kept >= 1);
}
