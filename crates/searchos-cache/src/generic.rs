//! Retrieval, embedding and rerank caches (RFC-007 7.2).
//!
//! Each keys on the inputs that must match for safe reuse:
//! - RetrievalCache: query fingerprint + index version.
//! - EmbeddingCache: (model, model_version, chunk_hash).
//! - RerankCache: (rerank model, candidate hash).

use crate::CacheEntryMeta;
use searchos_domain::retrieval::RetrievalCandidate;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

fn hkey(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for p in parts {
        hasher.update(p.len().to_string().as_bytes());
        hasher.update(p.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Retrieval candidate cache invalidated by index version changes.
#[derive(Debug)]
pub struct RetrievalCache {
    entries: RwLock<HashMap<String, (Vec<RetrievalCandidate>, CacheEntryMeta)>>,
    index_version: RwLock<u64>,
    ttl_seconds: u64,
}

impl RetrievalCache {
    /// New retrieval cache with TTL.
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            index_version: RwLock::new(1),
            ttl_seconds,
        }
    }

    /// Bumping the index version invalidates all entries (RFC-007 7.2).
    pub fn bump_index_version(&self) {
        let mut v = self.index_version.write().unwrap();
        *v += 1;
        self.entries.write().unwrap().clear();
    }

    fn key(&self, fingerprint: &str, top_k: usize) -> String {
        let iv = *self.index_version.read().unwrap();
        hkey(&[fingerprint, &iv.to_string(), &top_k.to_string()])
    }

    /// Gets cached candidates.
    pub fn get(
        &self,
        fingerprint: &str,
        top_k: usize,
        policy_version: &str,
    ) -> Option<Vec<RetrievalCandidate>> {
        let k = self.key(fingerprint, top_k);
        let map = self.entries.read().unwrap();
        match map.get(&k) {
            Some((v, meta)) if meta.valid(policy_version) => Some(v.clone()),
            _ => None,
        }
    }

    /// Stores candidates.
    pub fn put(
        &self,
        fingerprint: &str,
        top_k: usize,
        policy_version: &str,
        candidates: Vec<RetrievalCandidate>,
    ) {
        let k = self.key(fingerprint, top_k);
        let meta = CacheEntryMeta::new(self.ttl_seconds, policy_version);
        self.entries.write().unwrap().insert(k, (candidates, meta));
    }
}

/// Embedding cache: same text + same model version ⇒ reuse (RFC-007 7.6).
#[derive(Debug, Default)]
pub struct EmbeddingCache {
    entries: RwLock<HashMap<String, Arc<Vec<f32>>>>,
}

impl EmbeddingCache {
    /// Cache key: model id + version + chunk hash (RFC-007 7.6).
    pub fn embedding_key(model_id: &str, model_version: &str, chunk_hash: &str) -> String {
        hkey(&[model_id, model_version, chunk_hash])
    }

    /// Gets a cached vector.
    pub fn get(
        &self,
        model_id: &str,
        model_version: &str,
        chunk_hash: &str,
    ) -> Option<Arc<Vec<f32>>> {
        self.entries
            .read()
            .unwrap()
            .get(&Self::embedding_key(model_id, model_version, chunk_hash))
            .cloned()
    }

    /// Stores a vector.
    pub fn put(&self, model_id: &str, model_version: &str, chunk_hash: &str, vector: Vec<f32>) {
        self.entries.write().unwrap().insert(
            Self::embedding_key(model_id, model_version, chunk_hash),
            Arc::new(vector),
        );
    }

    /// Entry count.
    pub fn len(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Rerank cache keyed on rerank model + candidate-set hash.
#[derive(Debug, Default)]
pub struct RerankCache {
    entries: RwLock<HashMap<String, (Vec<RetrievalCandidate>, CacheEntryMeta)>>,
    ttl_seconds: u64,
}

impl RerankCache {
    /// New rerank cache.
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl_seconds,
        }
    }

    fn key(&self, model: &str, candidate_ids: &[&str]) -> String {
        let mut joined = candidate_ids.join(",");
        joined.insert_str(0, model);
        hkey(&[&joined])
    }

    /// Evicts everything (index / candidate-set change).
    pub fn clear(&self) {
        self.entries.write().unwrap().clear();
    }

    /// Gets a cached rerank ordering.
    pub fn get(
        &self,
        model: &str,
        candidate_ids: &[&str],
        policy_version: &str,
    ) -> Option<Vec<RetrievalCandidate>> {
        let k = self.key(model, candidate_ids);
        let map = self.entries.read().unwrap();
        match map.get(&k) {
            Some((v, meta)) if meta.valid(policy_version) => Some(v.clone()),
            _ => None,
        }
    }

    /// Stores a rerank ordering.
    pub fn put(
        &self,
        model: &str,
        candidate_ids: &[&str],
        policy_version: &str,
        value: Vec<RetrievalCandidate>,
    ) {
        let k = self.key(model, candidate_ids);
        let meta = CacheEntryMeta::new(self.ttl_seconds, policy_version);
        self.entries.write().unwrap().insert(k, (value, meta));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_document::content_hash;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};

    fn cand(id: &str) -> RetrievalCandidate {
        RetrievalCandidate {
            chunk_id: ChunkId::new(id),
            document_id: DocumentId::new("doc"),
            version_id: DocumentVersionId::new("docv"),
            title: None,
            snippet: String::new(),
            url: String::new(),
            score: 1.0,
            source: searchos_domain::retrieval::RetrievalSource::Hybrid,
            fetched_at: Utc::now(),
            vector_score: None,
            bm25_score: None,
            workspace_id: None,
            language: None,
        }
    }

    #[test]
    fn embedding_reused_per_model_version() {
        let c = EmbeddingCache::default();
        let hash = content_hash(b"chunk text");
        c.put("bge-m3", "v1", &hash, vec![0.1, 0.2]);
        assert!(c.get("bge-m3", "v1", &hash).is_some());
        assert!(c.get("bge-m3", "v2", &hash).is_none()); // model change → invalid
        assert!(c.get("other", "v1", &hash).is_none());
    }

    #[test]
    fn retrieval_cache_invalidated_by_index_bump() {
        let c = RetrievalCache::new(600);
        c.put("fp", 10, "1", vec![cand("a")]);
        assert!(c.get("fp", 10, "1").is_some());
        c.bump_index_version();
        assert!(c.get("fp", 10, "1").is_none());
    }

    #[test]
    fn rerank_cache_roundtrip() {
        let c = RerankCache::new(600);
        c.put("rr", &["a", "b"], "1", vec![cand("b"), cand("a")]);
        let got = c.get("rr", &["a", "b"], "1").unwrap();
        assert_eq!(got[0].chunk_id.as_str(), "b");
        assert!(c.get("rr", &["a"], "1").is_none());
    }
}
