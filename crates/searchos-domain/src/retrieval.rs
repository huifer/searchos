//! Retrieval domain types (RFC-007): indexed documents, candidates, cache keys.

use crate::ids::{ChunkId, DocumentId, DocumentVersionId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A document (chunk-level) submitted to a search index (RFC-002 2.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedDocument {
    pub chunk_id: ChunkId,
    pub document_id: DocumentId,
    pub version_id: DocumentVersionId,
    pub title: Option<String>,
    pub body: String,
    pub url: String,
    pub language: Option<String>,
    pub workspace_id: String,
    pub fetched_at: DateTime<Utc>,
}

/// A structured retrieval query passed to a [`crate::retrieval`] index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalQuery {
    pub text: String,
    pub workspace_id: Option<String>,
    pub language: Option<String>,
    /// Maximum number of candidates to return.
    pub top_k: usize,
    /// Optional freshness cutoff: exclude candidates fetched before this time
    /// unless `include_stale` is set.
    pub fetched_after: Option<DateTime<Utc>>,
    pub include_stale: bool,
    pub required_domains: Vec<String>,
    pub excluded_domains: Vec<String>,
}

impl RetrievalQuery {
    /// Creates a simple top-k query without filters.
    pub fn new(text: impl Into<String>, top_k: usize) -> Self {
        Self {
            text: text.into(),
            workspace_id: None,
            language: None,
            top_k,
            fetched_after: None,
            include_stale: true,
            required_domains: Vec::new(),
            excluded_domains: Vec::new(),
        }
    }
}

/// Which retrieval stage produced a candidate (RFC-007 7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalSource {
    ExactCache,
    CanonicalCache,
    Bm25,
    Vector,
    Hybrid,
    Reranked,
}

/// One retrieval candidate with provenance of the retrieval stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalCandidate {
    pub chunk_id: ChunkId,
    pub document_id: DocumentId,
    pub version_id: DocumentVersionId,
    pub title: Option<String>,
    pub snippet: String,
    pub url: String,
    pub score: f32,
    pub source: RetrievalSource,
    pub fetched_at: DateTime<Utc>,
    /// Cosine similarity when the candidate came from vector search.
    pub vector_score: Option<f32>,
    /// BM25 score when available.
    pub bm25_score: Option<f32>,
}

/// Cache key composition (RFC-007 7.3).
///
/// `fingerprint = SHA256(normalized_query + language + profile_version +
/// freshness_policy + source_policy + workspace_scope)` (RFC-003 3.4):
/// requests with different quality constraints must never merge into the
/// same cache entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchCacheKey {
    pub query_fingerprint: String,
    pub profile_version: String,
    pub freshness_policy_version: String,
    pub source_policy_version: String,
    pub language: String,
    pub workspace_scope: String,
}

impl SearchCacheKey {
    /// Composes the full cache key string used by storage backends.
    pub fn cache_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.query_fingerprint,
            self.profile_version,
            self.freshness_policy_version,
            self.source_policy_version,
            self.language,
            self.workspace_scope,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_composes_stably() {
        let k = SearchCacheKey {
            query_fingerprint: "abc".into(),
            profile_version: "1.0.0".into(),
            freshness_policy_version: "1".into(),
            source_policy_version: "1".into(),
            language: "en".into(),
            workspace_scope: "ws_1".into(),
        };
        assert_eq!(k.cache_key(), "abc|1.0.0|1|1|en|ws_1");
    }
}
