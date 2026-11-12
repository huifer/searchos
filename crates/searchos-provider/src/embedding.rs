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

/// Configuration for an OpenAI-compatible remote embedding endpoint.
/// Secrets are NEVER stored here — `api_key_env` names an environment
/// variable read at request time, so keys never land in config files or logs.
#[derive(Debug, Clone)]
pub struct RemoteEmbeddingConfig {
    /// Full embeddings endpoint, e.g. `https://api.openai.com/v1/embeddings`.
    pub endpoint: String,
    pub model: String,
    /// Environment variable holding the API key (empty = no auth header).
    pub api_key_env: String,
    /// Expected vector width; responses with a different width are rejected.
    pub dimensions: usize,
    pub timeout_seconds: u64,
    /// Stable version tag for embedding-reuse cache keys (RFC-007 7.6).
    pub model_version: String,
    /// Vectors returned by the endpoint are L2-normalized.
    pub normalized: bool,
    /// Upper bound on texts per HTTP request.
    pub max_batch_size: usize,
}

impl Default for RemoteEmbeddingConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            model: "text-embedding-3-small".into(),
            api_key_env: "SEARCHOS_EMBEDDING_API_KEY".into(),
            dimensions: 1536,
            timeout_seconds: 15,
            model_version: "v1".into(),
            normalized: true,
            max_batch_size: 32,
        }
    }
}

/// OpenAI-compatible `/embeddings` provider. Kept deliberately small: the
/// only wire contract is `{model, input[], dimensions?}` →
/// `{data[{index, embedding[]}]}`. Any compatible gateway (OpenAI, vLLM,
/// Ollama with an OpenAI shim, a local bge server) works unchanged.
pub struct RemoteEmbeddingProvider {
    client: reqwest::Client,
    config: RemoteEmbeddingConfig,
}

impl std::fmt::Debug for RemoteEmbeddingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteEmbeddingProvider")
            .field("endpoint", &self.config.endpoint)
            .field("model", &self.config.model)
            .field("dimensions", &self.config.dimensions)
            .finish_non_exhaustive()
    }
}

impl RemoteEmbeddingProvider {
    pub fn new(config: RemoteEmbeddingConfig) -> Result<Self, ProviderEmbedError> {
        if config.endpoint.trim().is_empty() {
            return Err(ProviderEmbedError(
                "remote embedding endpoint must not be empty".into(),
            ));
        }
        if config.dimensions == 0 || config.max_batch_size == 0 {
            return Err(ProviderEmbedError(
                "dimensions and max_batch_size must be positive".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                config.timeout_seconds.max(1),
            ))
            .build()
            .map_err(|e| ProviderEmbedError(format!("http client: {e}")))?;
        Ok(Self { client, config })
    }

    fn auth_header(&self) -> Option<String> {
        if self.config.api_key_env.is_empty() {
            return None;
        }
        let key = std::env::var(&self.config.api_key_env).unwrap_or_default();
        if key.trim().is_empty() {
            None
        } else {
            Some(format!("Bearer {key}"))
        }
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, ProviderEmbedError> {
        let mut body = serde_json::json!({
            "model": self.config.model,
            "input": texts,
        });
        if self.config.dimensions > 0 {
            body["dimensions"] = serde_json::json!(self.config.dimensions);
        }
        let mut req = self.client.post(&self.config.endpoint).json(&body);
        if let Some(auth) = self.auth_header() {
            req = req.header(reqwest::header::AUTHORIZATION, auth);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ProviderEmbedError(format!("embedding request: {e}")))?;
        let status = resp.status();
        let payload: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderEmbedError(format!("embedding response: {e}")))?;
        if !status.is_success() {
            // Never echo the body: it may contain request/auth context.
            return Err(ProviderEmbedError(format!(
                "embedding endpoint returned HTTP {status}"
            )));
        }
        let data = payload
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ProviderEmbedError("missing data[] in embedding response".into()))?;
        let mut by_index: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        for item in data {
            let idx = item
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or(usize::MAX as u64) as usize;
            let values = item
                .get("embedding")
                .and_then(|v| v.as_array())
                .ok_or_else(|| ProviderEmbedError("missing embedding[] in response item".into()))?;
            let vector: Vec<f32> = values
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();
            if vector.len() != self.config.dimensions {
                return Err(ProviderEmbedError(format!(
                    "embedding dimension mismatch: got {}, expected {}",
                    vector.len(),
                    self.config.dimensions
                )));
            }
            if idx >= by_index.len() {
                return Err(ProviderEmbedError(format!(
                    "embedding index {idx} out of range for batch size {}",
                    texts.len()
                )));
            }
            by_index[idx] = Some(vector);
        }
        by_index
            .into_iter()
            .enumerate()
            .map(|(i, v)| {
                v.ok_or_else(|| ProviderEmbedError(format!("missing embedding for input {i}")))
            })
            .collect()
    }
}

#[async_trait]
impl EmbeddingProvider for RemoteEmbeddingProvider {
    fn model_id(&self) -> &str {
        &self.config.model
    }

    fn model_version(&self) -> &str {
        &self.config.model_version
    }

    fn dimension(&self) -> usize {
        self.config.dimensions
    }

    fn normalized(&self) -> bool {
        self.config.normalized
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, ProviderEmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(self.config.max_batch_size) {
            let part = self.embed_batch(chunk).await?;
            if part.len() != chunk.len() {
                return Err(ProviderEmbedError(
                    "embedding response count does not match input batch".into(),
                ));
            }
            out.extend(part);
        }
        Ok(out)
    }
}
