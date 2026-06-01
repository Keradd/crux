use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crux_core::error::Result;
use crux_core::tokens;

use crate::embed::{pack_vector, Embedder};
use crate::types::{Chunk, IndexStats};

pub struct Indexer<'c> {
    conn: &'c Connection,
}

impl<'c> Indexer<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    pub fn index_chunks(&self, chunks: &[Chunk], embedder: &dyn Embedder) -> Result<IndexStats> {
        let mut stats = IndexStats::default();
        let now = chrono::Utc::now().timestamp();
        for chunk in chunks {
            let hash = content_hash(chunk);
            let existing: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM chunks WHERE project_root = ? AND content_hash = ?",
                    params![chunk.project_root, hash],
                    |r| r.get(0),
                )
                .optional()?;

            let chunk_id = match existing {
                Some(id) => {
                    stats.chunks_skipped_unchanged += 1;
                    id
                }
                None => {
                    let tokens_est = tokens::estimate(&chunk.content) as i64;
                    self.conn.execute(
                        r#"INSERT INTO chunks
                            (project_root, source_id, file_path, language, content_type,
                             title, content, line_start, line_end, tokens_est,
                             content_hash, created_at_epoch)
                           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
                        params![
                            chunk.project_root,
                            chunk.source_id,
                            chunk.file_path,
                            chunk.language,
                            chunk.content_type.as_str(),
                            chunk.title,
                            chunk.content,
                            chunk.line_start as i64,
                            chunk.line_end as i64,
                            tokens_est,
                            hash,
                            now,
                        ],
                    )?;
                    stats.chunks_inserted += 1;
                    self.conn.last_insert_rowid()
                }
            };

            let vec = embedder.embed(&chunk.content)?;
            let norm = vec.iter().map(|x| x * x).sum::<f32>().sqrt() as f64;
            let blob = pack_vector(&vec);
            self.conn.execute(
                r#"INSERT INTO chunk_embeddings
                    (chunk_id, provider, model, dim, vector, norm, updated_at_epoch)
                   VALUES (?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(chunk_id) DO UPDATE SET
                     provider = excluded.provider,
                     model    = excluded.model,
                     dim      = excluded.dim,
                     vector   = excluded.vector,
                     norm     = excluded.norm,
                     updated_at_epoch = excluded.updated_at_epoch"#,
                params![
                    chunk_id,
                    embedder.provider(),
                    embedder.model(),
                    embedder.dim() as i64,
                    blob,
                    norm,
                    now,
                ],
            )?;
            stats.embeddings_written += 1;
        }
        Ok(stats)
    }

    pub fn purge_project(&self, project_root: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM chunks WHERE project_root = ?",
            params![project_root],
        )?;
        Ok(())
    }

    pub fn purge_files(&self, project_root: &str, file_paths: &[String]) -> Result<u64> {
        if file_paths.is_empty() {
            return Ok(0);
        }
        // SAFETY: Indexer<'c> borrows &'c Connection which is !Send/!Sync.
        // Single-threaded use, no other mutable borrow exists.
        let tx = self.conn.unchecked_transaction()?;
        let mut removed: u64 = 0;
        for p in file_paths {
            let n = tx.execute(
                "DELETE FROM chunks WHERE project_root = ? AND file_path = ?",
                params![project_root, p],
            )?;
            removed += n as u64;
        }
        tx.commit()?;
        Ok(removed)
    }

    pub fn count_chunks(&self, project_root: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM chunks WHERE project_root = ?",
            params![project_root],
            |r| r.get(0),
        )?)
    }
}

fn content_hash(chunk: &Chunk) -> String {
    let mut h = Sha256::new();
    h.update(chunk.project_root.as_bytes());
    h.update(b"\x00");
    h.update(chunk.file_path.as_bytes());
    h.update(b"\x00");
    h.update(chunk.content_type.as_str().as_bytes());
    h.update(b"\x00");
    h.update(chunk.line_start.to_le_bytes());
    h.update(chunk.line_end.to_le_bytes());
    h.update(b"\x00");
    h.update(chunk.content.as_bytes());
    let bytes = h.finalize();
    hex::encode(&bytes[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;
    use crate::types::ContentType;

    fn sample_chunk() -> Chunk {
        Chunk {
            project_root: "/tmp/p".into(),
            source_id: None,
            file_path: "src/lib.rs".into(),
            language: Some("rust".into()),
            content_type: ContentType::Code,
            title: Some("fn add".into()),
            content: "pub fn add(a: i32, b: i32) -> i32 { a + b }".into(),
            line_start: 1,
            line_end: 1,
        }
    }

    #[test]
    fn indexes_a_chunk_idempotently() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let idx = Indexer::new(&conn);
        let emb = HashEmbedder::new(64);
        let s1 = idx.index_chunks(&[sample_chunk()], &emb).unwrap();
        assert_eq!(s1.chunks_inserted, 1);
        let s2 = idx.index_chunks(&[sample_chunk()], &emb).unwrap();
        assert_eq!(s2.chunks_inserted, 0);
        assert_eq!(s2.chunks_skipped_unchanged, 1);
        assert_eq!(idx.count_chunks("/tmp/p").unwrap(), 1);
    }
}
