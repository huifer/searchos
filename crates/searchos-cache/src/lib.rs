//! Multi-layer cache architecture (RFC-007 7.2).
//!
//! | Cache                 | 作用       | 失效条件                        |
//! |-----------------------|------------|--------------------------------|
//! | Exact Query Cache     | 完全相同请求 | TTL / Policy Version           |
//! | Canonical Query Cache | 归一化查询复用 | TTL / Constraint Change      |
//! | Document Cache        | 网页内容复用 | Content Change / Refresh       |
//! | Retrieval Cache       | 候选检索结果复用 | Index Version Change       |
//! | Embedding Cache       | 相同文本向量复用 | Model Version Change       |
//! | Rerank Cache          | 相同候选排序复用 | Model / Candidate Change   |
//!
//! Cache 不是 Memory (RFC-013 13.1) — memory lives in `searchos-memory`.

pub mod canonical_semantic;
pub mod document;
pub mod exact;
pub mod generic;

pub use canonical_semantic::{CanonicalQueryCache, SemanticCache, SemanticCompatibility};
pub use document::{DocumentCache, FetchCacheEntry};
pub use exact::{CacheHit, CacheLookup, ExactQueryCache, DEFAULT_MAX_CACHE_ENTRIES};
pub use generic::{EmbeddingCache, RerankCache, RetrievalCache};

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};

/// Generic TTL entry metadata shared by cache layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntryMeta {
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub policy_version: String,
}

impl CacheEntryMeta {
    /// New entry with TTL seconds from now.
    pub fn new(ttl_seconds: u64, policy_version: impl Into<String>) -> Self {
        let created_at = Utc::now();
        Self {
            created_at,
            expires_at: created_at + chrono::Duration::seconds(ttl_seconds as i64),
            policy_version: policy_version.into(),
        }
    }

    /// Whether the entry is expired (or the policy version changed).
    pub fn valid(&self, current_policy_version: &str) -> bool {
        Utc::now() < self.expires_at && self.policy_version == current_policy_version
    }
}
