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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SemanticCompatibility {
    pub intents: Vec<SearchIntent>,
    pub language: String,
    pub workspace_scope: String,
    pub freshness_policy: String,
    pub source_policy: String,
}

impl SemanticCompatibility {
    /// Whether reuse is permitted: intent compatible (overlap), language and
    /// workspace compatible, and quality constraints (freshness/source)
    /// compatible. Vector similarity is NOT checked here — this gate is
    /// called *after* the caller found similar vectors.
    pub fn allows_reuse(&self, other: &Self) -> bool {
        if self.language != other.language || self.workspace_scope != other.workspace_scope {
            return false;
        }
        if self.freshness_policy != other.freshness_policy
            || self.source_policy != other.source_policy
        {
            return false;
        }
        // Intent compatibility: any overlap between the two sets (both
        // Unknown → treat as compatible by content similarity only).
        self.intents.iter().any(|i| other.intents.contains(i))
            || self.intents.iter().all(|i| *i == SearchIntent::Unknown)
            || other.intents.iter().all(|i| *i == SearchIntent::Unknown)
    }
}

/// One semantic cache entry.
type SemanticEntry = (
    Vec<RetrievalCandidate>,
    SemanticCompatibility,
    CacheEntryMeta,
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
    fn key(similarity_bucket: &str) -> String {
        format!("sem:{similarity_bucket}")
    }

    /// Stores candidates with their compatibility constraints.
    pub fn put(
        &self,
        similarity_bucket: &str,
        candidates: Vec<RetrievalCandidate>,
        compatibility: SemanticCompatibility,
        policy_version: &str,
    ) {
        let k = Self::key(similarity_bucket);
        let meta = CacheEntryMeta::new(self.ttl_seconds, policy_version);
        self.entries
            .write()
            .unwrap()
            .insert(k, (candidates, compatibility, meta));
    }

    /// Gets candidates only when the compatibility gate passes (RFC-007 7.4).
    pub fn get(
        &self,
        similarity_bucket: &str,
        requester: &SemanticCompatibility,
        policy_version: &str,
    ) -> Option<Vec<RetrievalCandidate>> {
        let k = Self::key(similarity_bucket);
        let map = self.entries.read().unwrap();
        match map.get(&k) {
            Some((v, compat, meta))
                if meta.valid(policy_version) && compat.allows_reuse(requester) =>
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
        }
    }

    fn compat(ws: &str, fresh: &str) -> SemanticCompatibility {
        SemanticCompatibility {
            intents: vec![SearchIntent::Technical],
            language: "en".into(),
            workspace_scope: ws.into(),
            freshness_policy: fresh.into(),
            source_policy: "trusted".into(),
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
        c.put("bucket-1", vec![cand("a")], compat("ws", "recent"), "1");
        // Same constraints: reuse allowed.
        assert!(c.get("bucket-1", &compat("ws", "recent"), "1").is_some());
        // Freshness constraint differs: no reuse.
        assert!(c.get("bucket-1", &compat("ws", "strict"), "1").is_none());
        // Different workspace: no reuse.
        assert!(c
            .get("bucket-1", &compat("other-ws", "recent"), "1")
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

    #[test]
    fn semantic_cache_clears_on_quality_failure() {
        let c = SemanticCache::new(600);
        c.put("b", vec![cand("a")], compat("ws", "recent"), "1");
        c.clear();
        assert!(c.get("b", &compat("ws", "recent"), "1").is_none());
    }
}
