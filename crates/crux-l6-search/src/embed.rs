use crux_core::error::Result;

pub trait Embedder: Send + Sync {
    fn provider(&self) -> &str;
    fn model(&self) -> &str;
    fn dim(&self) -> usize;

    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

#[derive(Debug, Clone)]
pub struct HashEmbedder {
    dim: usize,
    model_name: String,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            model_name: format!("hash-{}", dim),
        }
    }

    fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
        let unigrams: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| !w.is_empty())
            .map(|w| w.to_ascii_lowercase())
            .collect();
        let mut all = unigrams.clone();
        for win in unigrams.windows(2) {
            all.push(format!("{}_{}", win[0], win[1]));
        }
        all.into_iter()
    }

    fn fnv1a64(s: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
}

impl Embedder for HashEmbedder {
    fn provider(&self) -> &str {
        "hash"
    }
    fn model(&self) -> &str {
        &self.model_name
    }
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = vec![0f32; self.dim];
        for tok in Self::tokens(text) {
            let h = Self::fnv1a64(&tok);
            let idx = (h as usize) % self.dim;
            let sign = if (h >> 63) & 1 == 1 { 1f32 } else { -1f32 };
            v[idx] += sign;
        }
        let mut sum = 0f32;
        for x in &v {
            sum += x * x;
        }
        if sum > 0.0 {
            let n = sum.sqrt();
            for x in &mut v {
                *x /= n;
            }
        }
        Ok(v)
    }
}

/// In-process semantic embedder backed by ONNX models via `fastembed-rs`.
///
/// Pulls in the ONNX runtime + a model archive at first use; expect
/// the first call to take seconds while the model is downloaded /
/// extracted. Subsequent calls are fast (small models hit ~1–5 ms per
/// passage on CPU).
///
/// Activate via config:
/// ```toml
/// [layer.l6]
/// embedding_provider = "fastembed"
/// embedding_model    = "BGE-small-en-v1.5"   # see fastembed::EmbeddingModel
/// embedding_dim      = 384                    # must match the model
/// ```
///
/// Requires the `fastembed` cargo feature (`cargo build --features
/// crux-l6-search/fastembed`).
#[cfg(feature = "fastembed")]
pub struct FastEmbedder {
    inner: std::sync::Mutex<fastembed::TextEmbedding>,
    model_name: String,
    dim: usize,
}

#[cfg(feature = "fastembed")]
impl FastEmbedder {
    pub fn try_new(model_name: &str, dim: usize) -> Result<Self> {
        use crux_core::error::CruxError;
        use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

        let model = match model_name {
            "BGE-small-en-v1.5" | "bge-small-en-v1.5" => EmbeddingModel::BGESmallENV15,
            "BGE-base-en-v1.5" | "bge-base-en-v1.5" => EmbeddingModel::BGEBaseENV15,
            "BGE-large-en-v1.5" | "bge-large-en-v1.5" => EmbeddingModel::BGELargeENV15,
            "AllMiniLML6V2" | "all-MiniLM-L6-v2" => EmbeddingModel::AllMiniLML6V2,
            other => {
                return Err(CruxError::other(format!(
                    "unsupported fastembed model '{other}' \
                     (try BGE-small-en-v1.5 / BGE-base-en-v1.5 / AllMiniLML6V2)"
                )));
            }
        };
        let inner = TextEmbedding::try_new(InitOptions::new(model))
            .map_err(|e| CruxError::other(format!("fastembed init failed: {e}")))?;
        Ok(Self {
            inner: std::sync::Mutex::new(inner),
            model_name: model_name.to_string(),
            dim,
        })
    }
}

#[cfg(feature = "fastembed")]
impl Embedder for FastEmbedder {
    fn provider(&self) -> &str {
        "fastembed"
    }
    fn model(&self) -> &str {
        &self.model_name
    }
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        use crux_core::error::CruxError;
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| CruxError::other("fastembed mutex poisoned"))?;
        // Wrap in catch_unwind to prevent a panic inside the ONNX runtime
        // from poisoning the mutex permanently.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            guard.embed(vec![text.to_string()], None)
        }));
        let mut out = match result {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                drop(guard);
                return Err(CruxError::other(format!("fastembed embed failed: {e}")));
            }
            Err(_) => {
                drop(guard);
                let _ = self.inner.lock().into_inner();
                return Err(CruxError::other("fastembed panicked during embed"));
            }
        };
        drop(guard);
        let mut v = out
            .pop()
            .ok_or_else(|| CruxError::other("fastembed returned no rows"))?;
        if v.len() != self.dim {
            return Err(CruxError::other(format!(
                "fastembed produced dim={} but config said dim={}",
                v.len(),
                self.dim
            )));
        }
        let mut sum = 0f32;
        for x in &v {
            sum += x * x;
        }
        if sum > 0.0 {
            let n = sum.sqrt();
            for x in &mut v {
                *x /= n;
            }
        }
        Ok(v)
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        use crux_core::error::CruxError;
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_string()).collect();
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| CruxError::other("fastembed mutex poisoned"))?;
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| guard.embed(owned, None)));
        let raw = match result {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                drop(guard);
                return Err(CruxError::other(format!(
                    "fastembed embed_batch failed: {e}"
                )));
            }
            Err(_) => {
                drop(guard);
                let _ = self.inner.lock().into_inner();
                return Err(CruxError::other("fastembed panicked during embed_batch"));
            }
        };
        drop(guard);
        let mut out = Vec::with_capacity(raw.len());
        for mut v in raw {
            if v.len() != self.dim {
                return Err(CruxError::other(format!(
                    "fastembed produced dim={} but config said dim={}",
                    v.len(),
                    self.dim
                )));
            }
            let mut sum = 0f32;
            for x in &v {
                sum += x * x;
            }
            if sum > 0.0 {
                let n = sum.sqrt();
                for x in &mut v {
                    *x /= n;
                }
            }
            out.push(v);
        }
        Ok(out)
    }
}

pub fn pack_vector(vec: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for x in vec {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub fn unpack_vector(blob: &[u8], dim: usize) -> Option<Vec<f32>> {
    if blob.len() != dim * 4 {
        return None;
    }
    let mut v = Vec::with_capacity(dim);
    for chunk in blob.chunks_exact(4) {
        let arr: [u8; 4] = chunk.try_into().ok()?;
        v.push(f32::from_le_bytes(arr));
    }
    Some(v)
}

pub fn cosine_normalized(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut s = 0f32;
    for i in 0..a.len() {
        s += a[i] * b[i];
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_embedder_produces_normalized_vectors() {
        let e = HashEmbedder::new(64);
        let v = e.embed("compute delta over read cache").unwrap();
        assert_eq!(v.len(), 64);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4 || norm == 0.0);
    }

    #[test]
    fn similar_phrases_score_higher_than_unrelated() {
        let e = HashEmbedder::new(256);
        let a = e.embed("read cache delta compression").unwrap();
        let b = e.embed("read cache delta compute").unwrap();
        let c = e.embed("zoroastrian fire temple history").unwrap();
        let ab = cosine_normalized(&a, &b);
        let ac = cosine_normalized(&a, &c);
        assert!(ab > ac, "ab={ab} should beat ac={ac}");
    }

    #[test]
    fn pack_roundtrip() {
        let v = vec![0.1f32, -0.2, 0.3, 0.4];
        let blob = pack_vector(&v);
        let back = unpack_vector(&blob, 4).unwrap();
        for i in 0..4 {
            assert!((v[i] - back[i]).abs() < 1e-7);
        }
    }

    #[cfg(feature = "fastembed")]
    #[test]
    fn fastembed_embedder_round_trips_when_opted_in() {
        if std::env::var("FASTEMBED_LIVE").ok().as_deref() != Some("1") {
            return;
        }
        let model_name =
            std::env::var("FASTEMBED_MODEL").unwrap_or_else(|_| "BGE-small-en-v1.5".into());
        let dim: usize = std::env::var("FASTEMBED_DIM")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(384);
        let e = FastEmbedder::try_new(&model_name, dim).expect("init");
        let v = e.embed("hello world").expect("embed");
        assert_eq!(v.len(), dim);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3);
    }
}
