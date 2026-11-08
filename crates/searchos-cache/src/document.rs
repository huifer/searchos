//! Fetch/Document cache (RFC-007 7.2 row 4, RFC-010 10.9).
//!
//! Reuse requires: same canonical URL, compatible request method/headers,
//! content unchanged, and the document still satisfying the freshness policy.

use crate::CacheEntryMeta;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// Conditional-GET metadata kept per canonical URL (RFC-010 10.9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchCacheEntry {
    pub canonical_url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: String,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub next_refresh_at: chrono::DateTime<chrono::Utc>,
}

impl FetchCacheEntry {
    /// Whether this entry may be reused at `now` under a freshness policy.
    pub fn reusable(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        now < self.next_refresh_at
    }
}

/// Document (fetch) cache.
#[derive(Debug, Default)]
pub struct DocumentCache {
    entries: RwLock<HashMap<String, (FetchCacheEntry, CacheEntryMeta)>>,
}

impl DocumentCache {
    /// Puts a fetch entry.
    pub fn put(&self, entry: FetchCacheEntry, policy_version: &str) {
        let ttl = (entry.next_refresh_at - chrono::Utc::now())
            .num_seconds()
            .max(0) as u64;
        let meta = CacheEntryMeta::new(ttl.max(1), policy_version);
        self.entries
            .write()
            .unwrap()
            .insert(entry.canonical_url.clone(), (entry, meta));
    }

    /// Returns the cached entry when reusable (content unchanged + fresh).
    pub fn get(&self, canonical_url: &str, policy_version: &str) -> Option<FetchCacheEntry> {
        let map = self.entries.read().unwrap();
        match map.get(canonical_url) {
            Some((e, meta)) if meta.valid(policy_version) && e.reusable(chrono::Utc::now()) => {
                Some(e.clone())
            }
            _ => None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use searchos_document::content_hash;

    #[test]
    fn reusable_within_window() {
        let now = chrono::Utc::now();
        let e = FetchCacheEntry {
            canonical_url: "https://example.com".into(),
            etag: Some("\"v1\"".into()),
            last_modified: None,
            content_hash: content_hash(b"body"),
            fetched_at: now,
            next_refresh_at: now + chrono::Duration::hours(1),
        };
        let c = DocumentCache::default();
        c.put(e.clone(), "1");
        assert!(c.get("https://example.com", "1").is_some());
        assert!(c.get("https://other.com", "1").is_none());
    }
}
