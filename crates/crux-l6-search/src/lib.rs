pub mod chunker;
pub mod embed;
pub mod index;
pub mod merkle;
pub mod search;
pub mod types;

pub use chunker::{
    chunks_from_ast, chunks_from_ast_filtered, chunks_from_memory, chunks_from_memory_filtered,
    chunks_from_prose, chunks_from_prose_filtered, list_ast_files, list_memory_files,
    list_prose_files,
};
#[cfg(feature = "fastembed")]
pub use embed::FastEmbedder;
pub use embed::{cosine_normalized, pack_vector, unpack_vector, Embedder, HashEmbedder};
pub use index::Indexer;
pub use merkle::{FileChangeSet, FileSnapshot, MerkleSync};
pub use search::{SearchEngine, SearchOptions};
pub use types::{Chunk, ContentType, HybridResult, IndexStats, StoredChunk};

use crux_core::config::L6Config;
#[cfg(not(feature = "fastembed"))]
use crux_core::error::CruxError;
use crux_core::error::Result;

pub fn build_embedder(cfg: &L6Config) -> Result<Box<dyn Embedder>> {
    match cfg.embedding_provider.as_str() {
        "hash" => Ok(Box::new(HashEmbedder::new(cfg.embedding_dim as usize))),
        #[cfg(feature = "fastembed")]
        "fastembed" => {
            let dim = cfg.embedding_dim as usize;
            let e = FastEmbedder::try_new(&cfg.embedding_model, dim)?;
            Ok(Box::new(e))
        }
        #[cfg(not(feature = "fastembed"))]
        "fastembed" => Err(CruxError::other(
            "embedding_provider='fastembed' but the binary was built without the 'fastembed' \
             feature. Rebuild with `cargo build --features crux-l6-search/fastembed` or set \
             embedding_provider='hash'.",
        )),
        other => {
            tracing::warn!(
                provider = %other,
                "unknown l6.embedding_provider; falling back to HashEmbedder"
            );
            Ok(Box::new(HashEmbedder::new(cfg.embedding_dim as usize)))
        }
    }
}
