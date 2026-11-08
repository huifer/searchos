//! Exact Query Cache (RFC-007 7.2 row 1).
//!
//! Keyed by the full [`SearchCacheKey`] so different quality constraints can
//! never collide. Invalidation: TTL or policy version change.

use crate::CacheEntryMeta;
use searchos_domain::retrieval::SearchCacheKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// Outcome of a cache lookup.
#[derive(Debug, Clone, PartialEq)]
pub enum CacheLookup<T> {
    Hit(CacheHit<T>),
    Miss,
}

/// A hit plus metadata (hit rate accounting).
#[derive(Debug, Clone, PartialEq)]
pub struct CacheHit<T> {
    pub value: T,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Value stored in the exact cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSearchResult {
    /// Serialized SearchResponse payload.
    pub payload: serde_json::Value,
    /// Quality status at store time — stale/failed responses are never
    /// reused as passing (RFC-007 7.4 semantic reuse rules).
    pub quality_status: String,
}

/// In-memory exact query cache with TTL + policy-version invalidation.
#[derive(Debug, Default)]
pub struct ExactQueryCache {
    entries: RwLock<HashMap<String, (CachedSearchResult, CacheEntryMeta)>>,
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
    /// TTL seconds; policy version checked on every hit.
    ttl_seconds: u64,
}

impl ExactQueryCache {
    /// New cache with a TTL.
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            ttl_seconds,
            ..Default::default()
        }
    }

    /// Stores a result under a cache key.
    pub fn put(&self, key: &SearchCacheKey, value: CachedSearchResult, policy_version: &str) {
        let meta = CacheEntryMeta::new(self.ttl_seconds, policy_version);
        self.entries
            .write()
            .unwrap()
            .insert(key.cache_key(), (value, meta));
    }

    /// Looks a result up; expired or policy-changed entries are evicted.
    pub fn get(
        &self,
        key: &SearchCacheKey,
        policy_version: &str,
    ) -> CacheLookup<CachedSearchResult> {
        let mut map = self.entries.write().unwrap();
        let k = key.cache_key();
        match map.get(&k) {
            Some((v, meta)) if meta.valid(policy_version) => {
                self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                CacheLookup::Hit(CacheHit {
                    value: v.clone(),
                    created_at: meta.created_at,
                })
            }
            Some(_) => {
                map.remove(&k);
                self.misses
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                CacheLookup::Miss
            }
            None => {
                self.misses
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                CacheLookup::Miss
            }
        }
    }

    /// Evicts everything (policy version bump).
    pub fn clear(&self) {
        self.entries.write().unwrap().clear();
    }

    /// Hit/miss counters (RFC-015 15.2 Cache Hit Rate).
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(std::sync::atomic::Ordering::Relaxed),
            self.misses.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Current entry count.
    pub fn len(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(fp: &str) -> SearchCacheKey {
        SearchCacheKey {
            query_fingerprint: fp.into(),
            profile_version: "1.0.0".into(),
            freshness_policy_version: "1".into(),
            source_policy_version: "1".into(),
            language: "en".into(),
            workspace_scope: "ws".into(),
        }
    }

    #[test]
    fn hit_then_policy_bump_invalidates() {
        let c = ExactQueryCache::new(600);
        let k = key("fp1");
        c.put(
            &k,
            CachedSearchResult {
                payload: serde_json::json!({"q": "x"}),
                quality_status: "passed".into(),
            },
            "1.0.0",
        );
        assert!(matches!(c.get(&k, "1.0.0"), CacheLookup::Hit(_)));
        // Policy version changed → invalidated.
        assert!(matches!(c.get(&k, "2.0.0"), CacheLookup::Miss));
    }

    #[test]
    fn different_constraints_never_merge() {
        let c = ExactQueryCache::new(600);
        let k1 = key("fp1");
        let mut k2 = key("fp1");
        k2.freshness_policy_version = "2".into();
        c.put(
            &k1,
            CachedSearchResult {
                payload: serde_json::json!(1),
                quality_status: "passed".into(),
            },
            "1",
        );
        assert!(matches!(c.get(&k2, "1"), CacheLookup::Miss));
    }

    #[test]
    fn stats_counted() {
        let c = ExactQueryCache::new(600);
        let k = key("s");
        c.put(
            &k,
            CachedSearchResult {
                payload: serde_json::json!(1),
                quality_status: "passed".into(),
            },
            "1",
        );
        let _ = c.get(&k, "1");
        let _ = c.get(&k, "1"); // still valid → hit
        let (hits, misses) = c.stats();
        assert_eq!((hits, misses), (2, 0));
        c.clear();
        assert!(c.is_empty());
    }
}
