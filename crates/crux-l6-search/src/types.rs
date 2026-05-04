use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Code,
    Prose,
    Symbol,
    Memory,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::Code => "code",
            ContentType::Prose => "prose",
            ContentType::Symbol => "symbol",
            ContentType::Memory => "memory",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "code" => Self::Code,
            "prose" => Self::Prose,
            "symbol" => Self::Symbol,
            "memory" => Self::Memory,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub project_root: String,
    pub source_id: Option<i64>,
    pub file_path: String,
    pub language: Option<String>,
    pub content_type: ContentType,
    pub title: Option<String>,
    pub content: String,
    pub line_start: u32,
    pub line_end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredChunk {
    pub id: i64,
    pub project_root: String,
    pub source_id: Option<i64>,
    pub file_path: String,
    pub language: Option<String>,
    pub content_type: ContentType,
    pub title: Option<String>,
    pub content: String,
    pub line_start: u32,
    pub line_end: u32,
    pub tokens_est: u32,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridResult {
    pub chunk: StoredChunk,
    pub score: f64,
    pub bm25_porter_rank: Option<usize>,
    pub bm25_trigram_rank: Option<usize>,
    pub vector_rank: Option<usize>,
    pub snippet: String,
}

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub chunks_inserted: u64,
    pub chunks_updated: u64,
    pub chunks_skipped_unchanged: u64,
    pub embeddings_written: u64,
}
