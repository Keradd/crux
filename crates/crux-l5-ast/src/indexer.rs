use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crux_core::error::{CruxError, Result};
use crux_core::merkle::{FileSnapshot, MerkleSync, SCOPE_AST};

use crate::extract::{self, ProjectFileTypes};
use crate::graph::GraphStore;
use crate::sig_cache;
use crate::types::{IndexStats, Language};

const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024; // 5 MB hard cap

struct ScannedFile {
    rel: String,
    content: String,
    hash: String,
    size_bytes: u64,
    mtime_epoch: i64,
    language: Language,
    changed: bool,
    #[allow(dead_code)]
    path: PathBuf,
}

pub fn index_project(conn: &Connection, project_root: &Path) -> Result<IndexStats> {
    index_project_with(conn, project_root, false)
}

pub fn index_project_with(
    conn: &Connection,
    project_root: &Path,
    force: bool,
) -> Result<IndexStats> {
    if !project_root.is_dir() {
        return Err(CruxError::other(format!(
            "project root not a directory: {}",
            project_root.display()
        )));
    }
    let project_key = project_root.to_string_lossy().to_string();
    let store = GraphStore::new(conn);
    let sync = MerkleSync::new(conn, project_root, SCOPE_AST);

    if force {
        store.purge_project(&project_key)?;
        sync.purge()?;
        sig_cache::purge_project(conn, &project_key)?;
    }

    let mut stats = IndexStats::default();
    let mut present: HashSet<String> = HashSet::new();

    let mut scanned: Vec<ScannedFile> = Vec::new();
    let mut project_types = ProjectFileTypes::new();

    for entry in WalkBuilder::new(project_root)
        .standard_filters(true)
        .hidden(false)
        .build()
        .flatten()
    {
        if !entry.file_type().map(|f| f.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let ext = match path.extension().and_then(|s| s.to_str()) {
            Some(e) => e,
            None => continue,
        };
        let lang = match Language::from_extension(ext) {
            Some(l) => l,
            None => continue,
        };
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => {
                stats.files_skipped += 1;
                continue;
            }
        };
        if meta.len() > MAX_FILE_BYTES {
            stats.files_skipped += 1;
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => {
                stats.files_skipped += 1;
                continue;
            }
        };
        let hash = hash_hex(&content);
        let rel = path
            .strip_prefix(project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        present.insert(rel.clone());

        let changed = if force {
            true
        } else {
            match sync.hash_for(&rel)? {
                Some(stored) => stored != hash,
                None => true,
            }
        };

        let cached = if force {
            None
        } else {
            match sig_cache::load(conn, &project_key, &rel, &hash) {
                Ok(ft) => ft,
                Err(e) => {
                    tracing::warn!(
                        file = %rel,
                        error = %e,
                        "signature cache lookup failed; falling back to re-parse"
                    );
                    None
                }
            }
        };

        let file_types = if let Some(ft) = cached {
            stats.files_signature_cached += 1;
            ft
        } else {
            let lang_for_panic = lang.clone();
            let content_ref = content.clone();
            let sigs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                extract::collect_file_signatures(lang_for_panic, &content_ref)
            }));
            match sigs {
                Ok(ft) => {
                    if let Err(e) = sig_cache::store(conn, &project_key, &rel, &hash, &ft) {
                        tracing::warn!(
                            file = %rel,
                            error = %e,
                            "failed to persist file signature; next run will re-parse"
                        );
                    }
                    ft
                }
                Err(_) => {
                    tracing::warn!(
                        file = %rel,
                        "parser panicked during signature scan, skipping file"
                    );
                    stats.files_skipped += 1;
                    continue;
                }
            }
        };
        project_types.add(&file_types);

        let mtime_epoch = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        scanned.push(ScannedFile {
            rel,
            content,
            hash,
            size_bytes: meta.len(),
            mtime_epoch,
            language: lang,
            changed,
            path: path.to_path_buf(),
        });
    }

    for sf in &scanned {
        if !sf.changed {
            stats.files_unchanged += 1;
            continue;
        }

        let lang_for_panic = sf.language.clone();
        let content_ref = sf.content.clone();
        let rel_ref = sf.rel.clone();
        let project_ref = &project_types;
        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            extract::parse_with_project(
                lang_for_panic,
                &content_ref,
                Path::new(&rel_ref),
                Some(project_ref),
            )
        }));
        let parsed = match parsed {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(file = %sf.rel, "parser panicked during emit, skipping file");
                stats.files_skipped += 1;
                continue;
            }
        };
        store.purge_file(&project_key, &sf.rel)?;
        let (n, e) = store.write(
            &project_key,
            &sf.rel,
            sf.language.as_str(),
            &sf.hash,
            &parsed,
        )?;

        let snap = FileSnapshot {
            file_path: sf.rel.clone(),
            content_hash: sf.hash.clone(),
            size_bytes: sf.size_bytes,
            mtime_epoch: sf.mtime_epoch,
        };
        sync.commit_one(&snap)?;
        stats.files_scanned += 1;
        stats.nodes_upserted += n as u64;
        stats.edges_upserted += e as u64;
    }

    let stored = sync.load()?;
    let mut removed: Vec<String> = stored
        .keys()
        .filter(|p| !present.contains(p.as_str()))
        .cloned()
        .collect();
    removed.sort();
    if !removed.is_empty() {
        for p in &removed {
            store.purge_file(&project_key, p)?;
        }
        sync.remove(&removed)?;
        sig_cache::purge_files(conn, &project_key, &removed)?;
        stats.files_removed = removed.len() as u64;
    }

    let _ = store.resolve_cross_file_calls(&project_key)?;
    Ok(stats)
}

fn hash_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let bytes = h.finalize();
    hex::encode(&bytes[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_rust_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            r#"pub fn answer() -> i32 { 42 }
pub struct Foo;
impl Foo { pub fn bar(&self) {} }
"#,
        )
        .unwrap();
        let conn = crux_core::db::open_in_memory().unwrap();
        let stats = index_project(&conn, dir.path()).unwrap();
        assert!(stats.files_scanned >= 1);
        let store = GraphStore::new(&conn);
        let project_key = dir.path().to_string_lossy().to_string();
        let nodes = store.find_symbol(&project_key, "answer", None).unwrap();
        assert!(!nodes.is_empty());
        assert!(store.count_nodes(&project_key).unwrap() >= 3);
    }

    #[test]
    fn second_index_skips_unchanged_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn answer() -> i32 { 42 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/util.rs"),
            "pub fn helper() -> bool { true }\n",
        )
        .unwrap();
        let conn = crux_core::db::open_in_memory().unwrap();

        let first = index_project(&conn, dir.path()).unwrap();
        assert_eq!(first.files_scanned, 2);
        assert_eq!(first.files_unchanged, 0);
        assert_eq!(
            first.files_signature_cached, 0,
            "fresh DB cannot hit the signature cache on the first run"
        );

        let second = index_project(&conn, dir.path()).unwrap();
        assert_eq!(
            second.files_scanned, 0,
            "unchanged files should not re-parse"
        );
        assert_eq!(second.files_unchanged, 2);
        assert_eq!(second.files_removed, 0);
        assert_eq!(
            second.files_signature_cached, 2,
            "L5.12.5: every unchanged file should serve its FileTypes from the sig cache"
        );
    }

    #[test]
    fn modifying_a_file_reindexes_only_that_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn answer() -> i32 { 42 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/util.rs"),
            "pub fn helper() -> bool { true }\n",
        )
        .unwrap();
        let conn = crux_core::db::open_in_memory().unwrap();
        let _ = index_project(&conn, dir.path()).unwrap();

        std::fs::write(
            dir.path().join("src/util.rs"),
            "pub fn helper() -> bool { false }\npub fn other() {}\n",
        )
        .unwrap();
        let stats = index_project(&conn, dir.path()).unwrap();
        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.files_unchanged, 1);
        assert_eq!(stats.files_removed, 0);
        assert_eq!(
            stats.files_signature_cached, 1,
            "the untouched file's signature should be served from the cache"
        );

        let store = GraphStore::new(&conn);
        let key = dir.path().to_string_lossy().to_string();
        let nodes = store.find_symbol(&key, "other", None).unwrap();
        assert!(
            !nodes.is_empty(),
            "expected `other` to appear after re-index"
        );
    }

    #[test]
    fn deleting_a_file_purges_its_nodes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/keep.rs"),
            "pub fn kept() -> i32 { 1 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/gone.rs"),
            "pub fn vanishing_symbol() -> i32 { 2 }\n",
        )
        .unwrap();
        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let store = GraphStore::new(&conn);
        let key = dir.path().to_string_lossy().to_string();
        assert!(!store
            .find_symbol(&key, "vanishing_symbol", None)
            .unwrap()
            .is_empty());

        let sig_rows_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ast_file_signatures
                  WHERE project_root = ? AND file_path = ?",
                rusqlite::params![&key, "src/gone.rs"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sig_rows_before, 1);

        std::fs::remove_file(dir.path().join("src/gone.rs")).unwrap();
        let stats = index_project(&conn, dir.path()).unwrap();
        assert_eq!(stats.files_scanned, 0);
        assert_eq!(stats.files_removed, 1);
        assert!(store
            .find_symbol(&key, "vanishing_symbol", None)
            .unwrap()
            .is_empty());
        let sig_rows_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ast_file_signatures
                  WHERE project_root = ? AND file_path = ?",
                rusqlite::params![&key, "src/gone.rs"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            sig_rows_after, 0,
            "L5.12.5: deleted files must have their cached signature purged"
        );
    }

    #[test]
    fn force_rebuilds_everything() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn answer() -> i32 { 42 }\n",
        )
        .unwrap();
        let conn = crux_core::db::open_in_memory().unwrap();

        let first = index_project(&conn, dir.path()).unwrap();
        assert_eq!(first.files_scanned, 1);
        let forced = index_project_with(&conn, dir.path(), true).unwrap();
        assert_eq!(forced.files_scanned, 1);
        assert_eq!(forced.files_unchanged, 0);
        assert_eq!(
            forced.files_signature_cached, 0,
            "--force must bypass and wipe the signature cache"
        );
    }

    #[test]
    fn cross_file_call_edges_get_resolved_after_index() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("main.rs"),
            "fn main() { let _ = compute_delta(); }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("delta.rs"),
            "pub fn compute_delta() -> i32 { 1 }\n",
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::main'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            target.ends_with("::compute_delta"),
            "expected resolved FQN, got {target}"
        );
        assert_eq!(tier, "RESOLVED");
    }

    #[test]
    fn cross_file_let_binding_resolves_via_project_signatures() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("file_a.rs"),
            r#"
                pub struct Foo;
                impl Foo { pub fn bar(&self) {} }
                pub fn make_foo() -> Foo { Foo }
            "#,
        )
        .unwrap();
        std::fs::write(
            src.join("file_b.rs"),
            r#"
                pub fn caller() {
                    let x = make_foo();
                    x.bar();
                }
            "#,
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let targets: Vec<String> = conn
            .prepare(
                "SELECT target_qn FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::caller'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected cross-file Foo::bar via make_foo's return type, got {:?}",
            targets
        );
    }

    #[test]
    fn cross_file_conflicting_fn_return_types_remain_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.rs"),
            r#"
                pub struct Foo;
                impl Foo { pub fn run(&self) {} }
                pub fn make() -> Foo { Foo }
            "#,
        )
        .unwrap();
        std::fs::write(
            src.join("b.rs"),
            r#"
                pub struct Bar;
                impl Bar { pub fn run(&self) {} }
                pub fn make() -> Bar { Bar }
            "#,
        )
        .unwrap();
        std::fs::write(
            src.join("c.rs"),
            r#"
                pub fn caller() {
                    let x = make();
                    x.run();
                }
            "#,
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let targets: Vec<String> = conn
            .prepare(
                "SELECT target_qn FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::caller'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            !targets.iter().any(|t| t.ends_with("::Foo::run")),
            "ambiguous make() should not bind x to Foo, got {:?}",
            targets
        );
        assert!(
            !targets.iter().any(|t| t.ends_with("::Bar::run")),
            "ambiguous make() should not bind x to Bar, got {:?}",
            targets
        );
    }

    #[test]
    fn cross_file_method_return_type_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.rs"),
            r#"
                pub struct Foo;
                pub struct Bar;
                impl Foo { pub fn produce(&self) -> Bar { Bar } }
                impl Bar { pub fn run(&self) {} }
            "#,
        )
        .unwrap();
        std::fs::write(
            src.join("b.rs"),
            r#"
                pub fn caller(foo: &Foo) {
                    let x = foo.produce();
                    x.run();
                }
            "#,
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let targets: Vec<String> = conn
            .prepare(
                "SELECT target_qn FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::caller'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            targets.iter().any(|t| t.ends_with("::Bar::run")),
            "expected cross-file Bar::run via Foo::produce, got {:?}",
            targets
        );
    }

    #[test]
    fn cross_file_user_enum_variant_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.rs"),
            r#"
                pub struct Foo;
                pub struct Bar;
                impl Foo { pub fn run(&self) {} }
                pub enum MyResult<T, E> { Hit(T), Miss, Err(E) }
                pub fn make() -> MyResult<Foo, Bar> { MyResult::Hit(Foo) }
            "#,
        )
        .unwrap();
        std::fs::write(
            src.join("b.rs"),
            r#"
                pub fn caller() {
                    if let Hit(x) = make() {
                        x.run();
                    }
                }
            "#,
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let targets: Vec<String> = conn
            .prepare(
                "SELECT target_qn FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::caller'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected cross-file Foo::run via MyResult::Hit + make() generics, got {:?}",
            targets
        );
    }

    #[test]
    fn cached_signature_still_feeds_cross_file_inference() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.rs"),
            r#"
                pub struct Foo;
                impl Foo { pub fn bar(&self) {} }
                pub fn make_foo() -> Foo { Foo }
            "#,
        )
        .unwrap();
        std::fs::write(
            src.join("b.rs"),
            r#"
                pub fn caller() {
                    let x = make_foo();
                    x.bar();
                }
            "#,
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        let first = index_project(&conn, dir.path()).unwrap();
        assert_eq!(first.files_scanned, 2);
        assert_eq!(first.files_signature_cached, 0);

        std::fs::write(
            src.join("b.rs"),
            r#"
                pub fn caller() {
                    let x = make_foo();
                    x.bar(); // identical call, whitespace delta only below
                    let _ = 1 + 2;
                }
            "#,
        )
        .unwrap();

        let second = index_project(&conn, dir.path()).unwrap();
        assert_eq!(second.files_scanned, 1, "only b.rs should have re-parsed");
        assert_eq!(
            second.files_signature_cached, 1,
            "a.rs should be served from the sig cache",
        );

        let key = dir.path().to_string_lossy().to_string();
        let targets: Vec<String> = conn
            .prepare(
                "SELECT target_qn FROM ast_edges
                 WHERE kind='CALLS' AND project_root = ? AND source_qn LIKE '%::caller'",
            )
            .unwrap()
            .query_map(rusqlite::params![&key], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "cached signature should still feed cross-file inference, got {:?}",
            targets
        );
    }

    #[test]
    fn modified_file_refreshes_its_cached_signature() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn first() -> i32 { 1 }\n").unwrap();
        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();
        let key = dir.path().to_string_lossy().to_string();

        let hash_before: String = conn
            .query_row(
                "SELECT content_hash FROM ast_file_signatures
                  WHERE project_root = ? AND file_path = 'src/lib.rs'",
                rusqlite::params![&key],
                |r| r.get(0),
            )
            .unwrap();

        std::fs::write(
            src.join("lib.rs"),
            "pub fn first() -> i32 { 1 }\npub fn second() -> i32 { 2 }\n",
        )
        .unwrap();
        let stats = index_project(&conn, dir.path()).unwrap();
        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.files_signature_cached, 0);

        let hash_after: String = conn
            .query_row(
                "SELECT content_hash FROM ast_file_signatures
                  WHERE project_root = ? AND file_path = 'src/lib.rs'",
                rusqlite::params![&key],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(
            hash_before, hash_after,
            "hash should refresh on content change"
        );

        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ast_file_signatures
                  WHERE project_root = ? AND file_path = 'src/lib.rs'",
                rusqlite::params![&key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count, 1, "UPSERT must not duplicate rows");
    }

    #[test]
    fn cross_file_user_enum_struct_variant_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.rs"),
            r#"
                pub struct Header;
                impl Header { pub fn parse(&self) {} }
                pub enum Payload { Ping { header: Header }, Pong }
                pub fn make() -> Payload {
                    Payload::Ping { header: Header }
                }
            "#,
        )
        .unwrap();
        std::fs::write(
            src.join("b.rs"),
            r#"
                pub fn caller() {
                    if let Payload::Ping { header } = make() {
                        header.parse();
                    }
                }
            "#,
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let targets: Vec<String> = conn
            .prepare(
                "SELECT target_qn FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::caller'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            targets.iter().any(|t| t.ends_with("::Header::parse")),
            "expected cross-file ::Header::parse via Payload::Ping struct variant, got {:?}",
            targets
        );
    }

    #[test]
    fn cross_file_tuple_typed_local_destructure_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.rs"),
            r#"
                pub struct Foo;
                pub struct Bar;
                impl Foo { pub fn run(&self) {} }
                impl Bar { pub fn run(&self) {} }
                pub fn make_pair() -> (Foo, Bar) { (Foo, Bar) }
            "#,
        )
        .unwrap();
        std::fs::write(
            src.join("b.rs"),
            r#"
                use crate::a::{Foo, Bar, make_pair};
                pub fn caller() {
                    let x = make_pair();
                    let (a, b) = x;
                    a.run();
                    b.run();
                }
            "#,
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let targets: Vec<String> = conn
            .prepare(
                "SELECT target_qn FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::caller'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected cross-file Foo::run via tuple-typed local, got {:?}",
            targets
        );
        assert!(
            targets.iter().any(|t| t.ends_with("::Bar::run")),
            "expected cross-file Bar::run via tuple-typed local, got {:?}",
            targets
        );
    }

    #[test]
    fn force_clears_signature_rows_for_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(src.join("b.rs"), "pub fn b() {}\n").unwrap();
        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();
        let key = dir.path().to_string_lossy().to_string();

        let before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ast_file_signatures WHERE project_root = ?",
                rusqlite::params![&key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(before, 2);

        let stats = index_project_with(&conn, dir.path(), true).unwrap();
        assert_eq!(stats.files_signature_cached, 0);

        let after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ast_file_signatures WHERE project_root = ?",
                rusqlite::params![&key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            after, 2,
            "--force should wipe then rewrite signature rows (net count unchanged)"
        );
    }

    #[test]
    fn ts_default_import_via_tsconfig_alias_resolves_after_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": { "@/*": ["src/*"] }
              }
            }"#,
        )
        .unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("utils")).unwrap();
        std::fs::write(
            src.join("utils/x.ts"),
            "export default function bar() { return 1; }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("y.ts"),
            "import Foo from '@/utils/x';\nexport function main() { Foo(); }\n",
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::main'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "src/utils/x::bar");
        assert_eq!(tier, "RESOLVED");
    }

    #[test]
    fn ts_named_import_via_tsconfig_alias_resolves_after_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": { "@/*": ["src/*"] }
              }
            }"#,
        )
        .unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("utils")).unwrap();
        std::fs::create_dir_all(src.join("other")).unwrap();
        std::fs::write(
            src.join("utils/x.ts"),
            "export function bar() { return 1; }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("other/z.ts"),
            "export function bar() { return 2; }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("y.ts"),
            "import { bar } from '@/utils/x';\nexport function main() { bar(); }\n",
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::main'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "src/utils/x::bar");
        assert_eq!(tier, "RESOLVED");
    }

    #[test]
    fn ts_named_import_relative_resolves_after_index() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("x.ts"), "export function foo() { return 1; }\n").unwrap();
        std::fs::write(
            src.join("other.ts"),
            "export function foo() { return 2; }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("y.ts"),
            "import { foo } from './x';\nexport function main() { foo(); }\n",
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::main'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "src/x::foo");
        assert_eq!(tier, "RESOLVED");
    }

    #[test]
    fn ts_namespace_import_via_tsconfig_alias_resolves_after_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": { "@/*": ["src/*"] }
              }
            }"#,
        )
        .unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("utils")).unwrap();
        std::fs::create_dir_all(src.join("other")).unwrap();
        std::fs::write(
            src.join("utils/x.ts"),
            "export function bar() { return 1; }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("other/z.ts"),
            "export function bar() { return 2; }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("y.ts"),
            "import * as utils from '@/utils/x';\n\
             export function main() { utils.bar(); }\n",
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::main'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "src/utils/x::bar");
        assert_eq!(tier, "RESOLVED");
    }

    #[test]
    fn ts_namespace_import_relative_resolves_after_index() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("x.ts"), "export function foo() { return 1; }\n").unwrap();
        std::fs::write(
            src.join("other.ts"),
            "export function foo() { return 2; }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("y.ts"),
            "import * as ns from './x';\n\
             export function main() { ns.foo(); }\n",
        )
        .unwrap();

        let conn = crux_core::db::open_in_memory().unwrap();
        index_project(&conn, dir.path()).unwrap();

        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges
                 WHERE kind='CALLS' AND source_qn LIKE '%::main'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "src/x::foo");
        assert_eq!(tier, "RESOLVED");
    }
}
