//! Exact Query Cache (RFC-007 7.2 row 1, Part 30 INV-30-21).
//!
//! Keyed by the full [`SearchCacheKey`] so different mode / freshness /
//! source constraints can never collide. Hits take a read lock; insertion is
//! bounded by an LRU eviction policy, and expired entries are removed on a
//! short write-lock path.

use crate::CacheEntryMeta;
use searchos_domain::retrieval::SearchCacheKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Default maximum resident exact-cache entries.
pub const DEFAULT_MAX_CACHE_ENTRIES: usize = 1024;

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

#[derive(Debug)]
struct CacheEntry {
    value: CachedSearchResult,
    meta: CacheEntryMeta,
    /// Logical LRU clock; atomic lets a read lock record a hit.
    last_access: AtomicU64,
}

type EntryMap = HashMap<String, Arc<CacheEntry>>;

/// In-memory exact query cache with TTL + policy-version invalidation.
#[derive(Debug)]
pub struct ExactQueryCache {
    entries: RwLock<EntryMap>,
    hits: AtomicU64,
    misses: AtomicU64,
    clock: AtomicU64,
    ttl_seconds: u64,
    max_entries: usize,
}

impl Default for ExactQueryCache {
    fn default() -> Self {
        Self::new(3600)
    }
}

impl ExactQueryCache {
    /// New cache with a TTL and the default bounded entry count.
    pub fn new(ttl_seconds: u64) -> Self {
        Self::with_max_entries(ttl_seconds, DEFAULT_MAX_CACHE_ENTRIES)
    }

    /// New cache with an explicit maximum entry count (tests / config).
    pub fn with_max_entries(ttl_seconds: u64, max_entries: usize) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            clock: AtomicU64::new(0),
            ttl_seconds,
            max_entries: max_entries.max(1),
        }
    }

    /// Stores a result under a cache key, evicting the least-recently-used
    /// entry when the cache is full.
    pub fn put(&self, key: &SearchCacheKey, value: CachedSearchResult, policy_version: &str) {
        let meta = CacheEntryMeta::new(self.ttl_seconds, policy_version);
        let tick = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let k = key.cache_key();
        let mut map = self.entries.write().unwrap();
        if !map.contains_key(&k) && map.len() >= self.max_entries {
            let victim = map
                .iter()
                .min_by_key(|(_, e)| e.last_access.load(Ordering::Relaxed))
                .map(|(k, _)| k.clone());
            if let Some(victim) = victim {
                map.remove(&victim);
                searchos_observability::MetricsRegistry::global()
                    .inc_counter("cache.evicted_total");
            }
        }
        map.insert(
            k,
            Arc::new(CacheEntry {
                value,
                meta,
                last_access: AtomicU64::new(tick),
            }),
        );
    }

    /// Looks a result up; valid hits only take a read lock.
    pub fn get(
        &self,
        key: &SearchCacheKey,
        policy_version: &str,
    ) -> CacheLookup<CachedSearchResult> {
        let k = key.cache_key();
        let entry = {
            let map = self.entries.read().unwrap();
            map.get(&k).cloned()
        };
        let Some(entry) = entry else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return CacheLookup::Miss;
        };
        if entry.meta.valid(policy_version) {
            let tick = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
            entry.last_access.store(tick, Ordering::Relaxed);
            self.hits.fetch_add(1, Ordering::Relaxed);
            return CacheLookup::Hit(CacheHit {
                value: entry.value.clone(),
                created_at: entry.meta.created_at,
            });
        }
        // Expired / policy-changed: remove only the same generation.
        {
            let mut map = self.entries.write().unwrap();
            if map.get(&k).map(|e| Arc::ptr_eq(e, &entry)).unwrap_or(false) {
                map.remove(&k);
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        CacheLookup::Miss
    }

    /// Evicts everything (policy version bump).
    pub fn clear(&self) {
        self.entries.write().unwrap().clear();
    }

    /// Hit/miss counters (RFC-015 15.2 Cache Hit Rate).
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
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
            mode: "local_only".into(),
            freshness_requirement: "recent".into(),
            key_version: "v2".into(),
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
        assert!(matches!(c.get(&k, "2.0.0"), CacheLookup::Miss));
    }

    #[test]
    fn different_constraints_never_merge() {
        let c = ExactQueryCache::new(600);
        let k1 = key("fp1");
        let mut k2 = key("fp1");
        k2.freshness_policy_version = "2".into();
        let mut k3 = key("fp1");
        k3.mode = "external_first".into();
        c.put(
            &k1,
            CachedSearchResult {
                payload: serde_json::json!(1),
                quality_status: "passed".into(),
            },
            "1",
        );
        assert!(matches!(c.get(&k2, "1"), CacheLookup::Miss));
        assert!(matches!(c.get(&k3, "1"), CacheLookup::Miss));
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
        let _ = c.get(&k, "1");
        let (hits, misses) = c.stats();
        assert_eq!((hits, misses), (2, 0));
        c.clear();
        assert!(c.is_empty());
    }

    /// Part 30 §30.11: a hot cache cannot grow without bound.
    #[test]
    fn cache_respects_max_entries() {
        let c = ExactQueryCache::with_max_entries(600, 3);
        for i in 0..10 {
            c.put(
                &key(&format!("fp{i}")),
                CachedSearchResult {
                    payload: serde_json::json!(i),
                    quality_status: "passed".into(),
                },
                "1",
            );
        }
        assert_eq!(c.len(), 3);
        assert!(
            searchos_observability::MetricsRegistry::global().counter("cache.evicted_total") > 0
        );
    }
}
