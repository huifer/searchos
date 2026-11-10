//! Canonical Query Cache (RFC-007 7.2 row 2) and Semantic Cache (row 3).
//!
//! - CanonicalQueryCache: 归一化查询复用 — same normalized query served from
//!   a slightly broader cache entry than the exact key; invalidated by TTL or
//!   constraint (profile/policy) change.
//! - SemanticCache: 语义近似请求复用 — candidate only on
//!   [`SemanticReuse`] compatibility checks (RFC-007 7.4): intent, freshness,
//!   source, language, workspace, coverage, validity. Semantic similarity is a
//!   candidate-recall condition, never the pass condition.

use searchos_domain::request::SearchIntent;
use searchos_domain::retrieval::RetrievalCandidate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

use crate::CacheEntryMeta;

/// Canonical query cache: keyed on the normalized-query fingerprint with a
/// looser policy gate than the exact cache (profile-independent), because the
/// result set for "rust tokio" is mostly profile-agnostic at recall level.
#[derive(Debug, Default)]
pub struct CanonicalQueryCache {
    entries: RwLock<HashMap<String, (Vec<RetrievalCandidate>, CacheEntryMeta)>>,
    ttl_seconds: u64,
}

impl CanonicalQueryCache {
    /// New cache with TTL.
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl_seconds,
        }
    }

    /// Key: normalized-query fingerprint only (query + language + workspace).
    fn key(query_fingerprint: &str, language: &str, workspace: &str) -> String {
        format!("{query_fingerprint}|{language}|{workspace}")
    }

    /// Stores candidates.
    pub fn put(
        &self,
        query_fingerprint: &str,
        language: &str,
        workspace: &str,
        candidates: Vec<RetrievalCandidate>,
        policy_version: &str,
    ) {
        let k = Self::key(query_fingerprint, language, workspace);
        let meta = CacheEntryMeta::new(self.ttl_seconds, policy_version);
        self.entries.write().unwrap().insert(k, (candidates, meta));
    }

    /// Invalidates everything (policy version bump / index change).
    pub fn clear(&self) {
        self.entries.write().unwrap().clear();
    }

    /// Gets candidates when the entry is fresh and the policy version matches.
    pub fn get(
        &self,
        query_fingerprint: &str,
        language: &str,
        workspace: &str,
        policy_version: &str,
    ) -> Option<Vec<RetrievalCandidate>> {
        let k = Self::key(query_fingerprint, language, workspace);
        let map = self.entries.read().unwrap();
        match map.get(&k) {
            Some((v, meta)) if meta.valid(policy_version) => Some(v.clone()),
            _ => None,
        }
    }
}

/// Compatibility dimensions a semantic reuse must satisfy (RFC-007 7.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticCompatibility {
    pub intents: Vec<SearchIntent>,
    pub language: String,
    pub workspace_scope: String,
    pub freshness_policy: String,
    pub source_policy: String,
    /// Effective freshness requirement (RFC-007 7.4), not just policy version.
    #[serde(default)]
    pub freshness_requirement: String,
    /// Search mode: LocalOnly must never reuse external-acquired evidence.
    #[serde(default)]
    pub mode: String,
    /// Minimum coverage threshold; reuse must be at least as strong.
    #[serde(default)]
    pub coverage_requirement: f32,
    /// Validity / quality status of the cached answer.
    #[serde(default)]
    pub validity_status: String,
}

impl Default for SemanticCompatibility {
    fn default() -> Self {
        Self {
            intents: vec![SearchIntent::Unknown],
            language: "en".into(),
            workspace_scope: "default".into(),
            freshness_policy: "recent".into(),
            source_policy: "trusted".into(),
            freshness_requirement: "any".into(),
            mode: "local_first".into(),
            coverage_requirement: 0.6,
            validity_status: "passed".into(),
        }
    }
}

impl SemanticCompatibility {
    /// Seven compatibility gates: intent, freshness, source, language,
    /// workspace, coverage and validity. Vector similarity is only a recall
    /// candidate; it never waives one of these gates.
    pub fn allows_reuse(&self, other: &Self) -> bool {
        let intent_ok = self.intents.iter().any(|i| other.intents.contains(i))
            || self.intents.iter().all(|i| *i == SearchIntent::Unknown)
            || other.intents.iter().all(|i| *i == SearchIntent::Unknown);
        intent_ok
            && self.language == other.language
            && self.workspace_scope == other.workspace_scope
            && self.freshness_policy == other.freshness_policy
            && self.freshness_requirement == other.freshness_requirement
            && self.source_policy == other.source_policy
            && self.mode == other.mode
            && self.coverage_requirement >= other.coverage_requirement
            && self.validity_status == other.validity_status
    }
}

/// Cosine similarity between two equally-sized vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

/// Minimum cosine similarity for a semantic-cache reuse. The bucket key is a
/// coarse recall hint only; reuse additionally requires the query embeddings
/// to actually be close, otherwise unrelated queries that happen to share a
/// rounded prefix would be served each other's candidates.
pub const SEMANTIC_REUSE_MIN_SIMILARITY: f32 = 0.92;

/// One semantic cache entry.
type SemanticEntry = (
    Vec<RetrievalCandidate>,
    SemanticCompatibility,
    CacheEntryMeta,
    Vec<f32>,
);

/// Semantic cache: 语义近似请求复用 — invalidated on quality or freshness
/// failure (RFC-007 7.2 row 3).
#[derive(Debug, Default)]
pub struct SemanticCache {
    entries: RwLock<HashMap<String, SemanticEntry>>,
    ttl_seconds: u64,
}

impl SemanticCache {
    /// New cache with TTL.
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl_seconds,
        }
    }

    /// Key: similarity bucket of the query embedding (caller computes the
    /// bucket from the embedder: e.g. L2-normalized vector rounded).
    ///
    /// The bucket is a coarse recall hint only. Two unrelated queries can
    /// share a bucket (the fallback embedder hashes text into a small
    /// dimensional space and the bucket rounds to one decimal), so reuse
    /// additionally requires a real similarity gate — see [`Self::get`].
    fn key(similarity_bucket: &str) -> String {
        format!("sem:{similarity_bucket}")
    }

    /// Stores candidates with their compatibility constraints and the query
    /// embedding that produced them.
    pub fn put(
        &self,
        similarity_bucket: &str,
        candidates: Vec<RetrievalCandidate>,
        compatibility: SemanticCompatibility,
        policy_version: &str,
        query_vector: Vec<f32>,
    ) {
        let k = Self::key(similarity_bucket);
        let meta = CacheEntryMeta::new(self.ttl_seconds, policy_version);
        self.entries
            .write()
            .unwrap()
            .insert(k, (candidates, compatibility, meta, query_vector));
    }

    /// Gets candidates only when the compatibility gate passes
    /// (RFC-007 7.4) **and** the requesting query is actually close to the
    /// cached query. Sharing a bucket is never sufficient.
    pub fn get(
        &self,
        similarity_bucket: &str,
        requester: &SemanticCompatibility,
        policy_version: &str,
        query_vector: &[f32],
    ) -> Option<Vec<RetrievalCandidate>> {
        let k = Self::key(similarity_bucket);
        let map = self.entries.read().unwrap();
        match map.get(&k) {
            Some((v, compat, meta, stored_vector))
                if meta.valid(policy_version)
                    && compat.allows_reuse(requester)
                    && cosine_similarity(stored_vector, query_vector)
                        >= SEMANTIC_REUSE_MIN_SIMILARITY =>
            {
                Some(v.clone())
            }
            _ => None,
        }
    }

    /// Invalidates everything (quality/freshness failure, RFC-007 7.2).
    pub fn clear(&self) {
        self.entries.write().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
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

    fn compat(ws: &str, fresh: &str) -> SemanticCompatibility {
        SemanticCompatibility {
            intents: vec![SearchIntent::Technical],
            language: "en".into(),
            workspace_scope: ws.into(),
            freshness_policy: fresh.into(),
            freshness_requirement: fresh.into(),
            mode: "local_first".into(),
            coverage_requirement: 0.6,
            validity_status: "passed".into(),
            ..Default::default()
        }
    }

    #[test]
    fn canonical_cache_roundtrip_and_policy_invalidation() {
        let c = CanonicalQueryCache::new(600);
        c.put("fp", "en", "ws", vec![cand("a")], "1");
        assert!(c.get("fp", "en", "ws", "1").is_some());
        assert!(
            c.get("fp", "en", "ws", "2").is_none(),
            "constraint change invalidates"
        );
        assert!(
            c.get("fp", "zh", "ws", "1").is_none(),
            "language scope matters"
        );
    }

    #[test]
    fn semantic_reuse_requires_compatibility() {
        let c = SemanticCache::new(600);
        let vector = vec![1.0, 0.0, 0.0];
        c.put(
            "bucket-1",
            vec![cand("a")],
            compat("ws", "recent"),
            "1",
            vector.clone(),
        );
        // Same constraints and a close query: reuse allowed.
        assert!(c
            .get("bucket-1", &compat("ws", "recent"), "1", &vector)
            .is_some());
        // Freshness constraint differs: no reuse.
        assert!(c
            .get("bucket-1", &compat("ws", "strict"), "1", &vector)
            .is_none());
        // Different workspace: no reuse.
        assert!(c
            .get("bucket-1", &compat("other-ws", "recent"), "1", &vector)
            .is_none());
    }

    /// The bucket key is a coarse rounded prefix; two unrelated queries can
    /// share it. Reuse must still be denied by the similarity gate, otherwise
    /// unrelated queries would be served each other's candidates.
    #[test]
    fn semantic_reuse_requires_vector_similarity() {
        let c = SemanticCache::new(600);
        let stored = vec![1.0, 0.0, 0.0];
        c.put(
            "bucket-1",
            vec![cand("a")],
            compat("ws", "recent"),
            "1",
            stored,
        );

        // Orthogonal query, same bucket: no reuse.
        assert!(c
            .get("bucket-1", &compat("ws", "recent"), "1", &[0.0, 1.0, 0.0])
            .is_none());
        // Near-identical query: reuse.
        assert!(c
            .get("bucket-1", &compat("ws", "recent"), "1", &[1.0, 0.01, 0.0])
            .is_some());
        // Zero-length / mismatched vectors never match.
        assert!(c
            .get("bucket-1", &compat("ws", "recent"), "1", &[])
            .is_none());
    }

    #[test]
    fn semantic_intent_compatibility() {
        let a = SemanticCompatibility {
            intents: vec![SearchIntent::Technical, SearchIntent::Latest],
            ..compat("ws", "recent")
        };
        let b = SemanticCompatibility {
            intents: vec![SearchIntent::Latest],
            ..compat("ws", "recent")
        };
        assert!(a.allows_reuse(&b), "intent overlap permits reuse");
        let c = SemanticCompatibility {
            intents: vec![SearchIntent::News],
            ..compat("ws", "recent")
        };
        assert!(!a.allows_reuse(&c), "disjoint intents deny reuse");
    }

    /// G-02 / RFC-007 7.4: all seven compatibility dimensions are enforced.
    #[test]
    fn semantic_reuse_checks_all_seven_dimensions() {
        let c = SemanticCache::new(600);
        let stored = compat("ws", "recent");
        let vector = vec![1.0, 0.0];
        c.put(
            "bucket",
            vec![cand("a")],
            stored.clone(),
            "1",
            vector.clone(),
        );
        assert!(c.get("bucket", &stored, "1", &vector).is_some());
        let mut different = stored.clone();
        different.mode = "local_only".into();
        assert!(c.get("bucket", &different, "1", &vector).is_none());
        let mut stricter = stored.clone();
        stricter.coverage_requirement = 0.9;
        assert!(c.get("bucket", &stricter, "1", &vector).is_none());
        let mut invalid = stored.clone();
        invalid.validity_status = "failed".into();
        assert!(c.get("bucket", &invalid, "1", &vector).is_none());
    }

    #[test]
    fn semantic_cache_clears_on_quality_failure() {
        let c = SemanticCache::new(600);
        c.put("b", vec![cand("a")], compat("ws", "recent"), "1", vec![1.0]);
        c.clear();
        assert!(c.get("b", &compat("ws", "recent"), "1", &[1.0]).is_none());
    }
}
