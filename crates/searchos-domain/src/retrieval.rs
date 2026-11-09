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
    /// Requesting workspace. Implementations should ALSO include the global
    /// `*` namespace so shared knowledge stays retrievable (RFC-013 8.7:
    /// default isolation, explicit sharing).
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
    /// Owning workspace when the adapter can recover it. `None` means the
    /// adapter relies on URL-based inference / engine-side filtering.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Stored document language when the adapter can recover it.
    #[serde(default)]
    pub language: Option<String>,
}

/// Cache key composition (RFC-007 7.3, Part 30 INV-30-04).
///
/// The fingerprint hashes query, language, policy versions, workspace, mode
/// and effective freshness. Mode/freshness are repeated explicitly so an old
/// or externally acquired entry can never satisfy a LocalOnly / strict path.
/// `key_version` makes pre-fix entries structurally unreachable.
pub const CACHE_KEY_VERSION: &str = "v2";

fn default_cache_key_version() -> String {
    CACHE_KEY_VERSION.into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchCacheKey {
    pub query_fingerprint: String,
    pub profile_version: String,
    pub freshness_policy_version: String,
    pub source_policy_version: String,
    pub language: String,
    pub workspace_scope: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub freshness_requirement: String,
    #[serde(default = "default_cache_key_version")]
    pub key_version: String,
}

impl Default for SearchCacheKey {
    fn default() -> Self {
        Self {
            query_fingerprint: String::new(),
            profile_version: String::new(),
            freshness_policy_version: String::new(),
            source_policy_version: String::new(),
            language: String::new(),
            workspace_scope: String::new(),
            mode: String::new(),
            freshness_requirement: String::new(),
            key_version: CACHE_KEY_VERSION.into(),
        }
    }
}

impl SearchCacheKey {
    /// Composes the full cache key string used by storage backends.
    pub fn cache_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.key_version,
            self.query_fingerprint,
            self.profile_version,
            self.freshness_policy_version,
            self.source_policy_version,
            self.language,
            self.workspace_scope,
            self.mode,
            self.freshness_requirement,
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
            mode: "local_only".into(),
            freshness_requirement: "recent".into(),
            key_version: CACHE_KEY_VERSION.into(),
        };
        assert_eq!(k.cache_key(), "v2|abc|1.0.0|1|1|en|ws_1|local_only|recent");
    }
}
