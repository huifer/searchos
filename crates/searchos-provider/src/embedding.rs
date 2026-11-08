//! Pluggable embedding providers (RFC Part 22, RFC-007 7.6).
//!
//! MVP default: deterministic feature-hashing embedder (no ML model, fully
//! offline, good enough for BM25-complement retrieval and embedding reuse
//! tests). Remote model providers plug in via [`EmbeddingProvider`].

use async_trait::async_trait;

/// Embedding provider trait (pluggable model provider).
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Stable model id (billed to the cost ledger).
    fn model_id(&self) -> &str;

    /// Model version — embedding reuse requires version match (RFC-007 7.6).
    fn model_version(&self) -> &str;

    /// Vector dimension.
    fn dimension(&self) -> usize;

    /// Whether vectors are L2-normalized.
    fn normalized(&self) -> bool;

    /// Embeds a batch of texts; order preserved.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, ProviderEmbedError>;
}

/// Embedding failure.
#[derive(Debug, thiserror::Error)]
#[error("embedding error: {0}")]
pub struct ProviderEmbedError(pub String);

/// Deterministic hashing embedder (MVP local model).
pub struct HashingEmbedder {
    dim: usize,
}

impl HashingEmbedder {
    /// Embedder with target dimension.
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(64) }
    }
}

#[async_trait]
impl EmbeddingProvider for HashingEmbedder {
    fn model_id(&self) -> &str {
        "hashing-mvp"
    }

    fn model_version(&self) -> &str {
        "v1"
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    fn normalized(&self) -> bool {
        true
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, ProviderEmbedError> {
        Ok(texts.iter().map(|t| self.embed_one(t)).collect())
    }
}

impl HashingEmbedder {
    /// Single-text embedding (public for batch builders).
    pub fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        // Tokenize on non-alphanumeric boundaries; hash each token into
        // three buckets with sign flipping (standard feature hashing).
        for token in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
        {
            let lower = token.to_lowercase();
            let mut h: u64 = 1469598103934665603; // FNV offset
            for b in lower.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(1099511628211);
            }
            let idx = (h as usize) % self.dim;
            let sign = if h & 1 == 0 { 1.0 } else { -1.0 };
            v[idx] += sign;
            // Second hash for robustness
            let h2 = h.wrapping_mul(0x9E3779B97F4A7C15);
            let idx2 = (h2 as usize) % self.dim;
            v[idx2] += sign * 0.5;
        }
        // L2 normalize.
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

/// Cosine similarity of two vectors (both expected normalized; falls back
/// to full cosine otherwise).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deterministic_and_normalized() {
        let e = HashingEmbedder::new(256);
        let a = e.embed(&["rust tokio async".into()]).await.unwrap();
        let b = e.embed(&["rust tokio async".into()]).await.unwrap();
        assert_eq!(a, b);
        let n: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-4, "normalized: {n}");
        assert_eq!(a[0].len(), 256);
    }

    #[tokio::test]
    async fn similar_texts_more_similar_than_different() {
        let e = HashingEmbedder::new(256);
        let q = e
            .embed(&["tokio async runtime".into()])
            .await
            .unwrap()
            .pop()
            .unwrap();
        let near = e
            .embed(&["tokio async runtime rust".into()])
            .await
            .unwrap()
            .pop()
            .unwrap();
        let far = e
            .embed(&["chocolate cake recipe".into()])
            .await
            .unwrap()
            .pop()
            .unwrap();
        let s_near = cosine_similarity(&q, &near);
        let s_far = cosine_similarity(&q, &far);
        assert!(s_near > s_far, "near {s_near} vs far {s_far}");
    }

    #[test]
    fn metadata_recorded() {
        let e = HashingEmbedder::new(128);
        assert_eq!(e.model_id(), "hashing-mvp");
        assert_eq!(e.model_version(), "v1");
        assert_eq!(e.dimension(), 128);
        assert!(e.normalized());
    }
}
