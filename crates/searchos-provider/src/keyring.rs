//! Multi-key rotation for upstream providers (RFC-009 9.7, RFC-014 14.4).
//!
//! Upstream providers (Tavily, Brave, …) accept multiple API keys. SearchOS
//! rotates across them so a single rate-limited or exhausted key does not
//! fail the whole search:
//!
//! - **Round-robin** (default): each call picks the next healthy key.
//! - **Failover** (`rotation: failover`): keep using the first healthy key
//!   until it fails, then move on.
//!
//! Per-key state is tracked independently: 429 → rate-limited until
//! Retry-After; 401/403/quota → blocked for a longer cooldown; timeouts and
//! network errors → consecutive-failure counter. Keys are **never** logged
//! or exposed in status output — only a masked form (`tvly-****-abcd`).
//!
//! The [`RotatingProvider`] implements the same [`SearchProvider`] port as
//! a single-key adapter, so the registry, router, breaker and cost ledger
//! keep working unchanged: one registered provider id, many keys inside.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use searchos_domain::provider::ProviderCapabilities;

use crate::error::{ProviderError, ProviderErrorKind};
use crate::traits::{ProviderResult, SearchParams, SearchProvider};

/// How keys are picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationPolicy {
    /// Round-robin across healthy keys (default).
    RoundRobin,
    /// Use the first healthy key until it fails (sticky failover).
    Failover,
}

impl RotationPolicy {
    /// Parses the config value (`round_robin` | `failover`).
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "failover" | "sticky" => Self::Failover,
            _ => Self::RoundRobin,
        }
    }
}

/// Per-key health snapshot for diagnostics (masked, never the raw key).
#[derive(Debug, Clone)]
pub struct KeyStatus {
    /// Key index in the pool (0-based).
    pub index: usize,
    /// Masked key, e.g. `tvly-****-abcd`.
    pub masked_key: String,
    /// `healthy` | `rate_limited` | `blocked` | `failing`.
    pub state: &'static str,
    /// When the key becomes usable again, if limited/blocked.
    pub available_at: Option<DateTime<Utc>>,
    /// Consecutive transient failures observed.
    pub consecutive_failures: u32,
}

/// Cooldowns applied per failure class (seconds).
const RATE_LIMIT_MAX_COOLDOWN_SECS: u64 = 3600;
const AUTH_BLOCK_SECS: u64 = 1800;
const QUOTA_BLOCK_SECS: u64 = 3600;
const FAILURE_THRESHOLD: u32 = 5;

/// One credential slot with independent health state.
struct KeySlot {
    key: String,
    rate_limited_until: RwLock<Option<DateTime<Utc>>>,
    blocked_until: RwLock<Option<DateTime<Utc>>>,
    consecutive_failures: AtomicU32,
}

impl KeySlot {
    fn new(key: String) -> Self {
        Self {
            key,
            rate_limited_until: RwLock::new(None),
            blocked_until: RwLock::new(None),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    /// Whether this key may be used right now.
    fn is_usable(&self, now: DateTime<Utc>) -> bool {
        if let Some(t) = *self.rate_limited_until.read().unwrap() {
            if t > now {
                return false;
            }
        }
        if let Some(t) = *self.blocked_until.read().unwrap() {
            if t > now {
                return false;
            }
        }
        self.consecutive_failures.load(Ordering::SeqCst) < FAILURE_THRESHOLD
    }

    /// Earliest time this key becomes usable again (for Retry-After hints).
    fn available_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let rl = *self.rate_limited_until.read().unwrap();
        let bl = *self.blocked_until.read().unwrap();
        [rl, bl].into_iter().flatten().filter(|t| *t > now).min()
    }

    fn mark_rate_limited(&self, seconds: u64) {
        let until = Utc::now()
            + chrono::Duration::seconds(seconds.min(RATE_LIMIT_MAX_COOLDOWN_SECS) as i64);
        *self.rate_limited_until.write().unwrap() = Some(until);
    }

    fn mark_blocked(&self, seconds: u64) {
        let until = Utc::now() + chrono::Duration::seconds(seconds as i64);
        *self.blocked_until.write().unwrap() = Some(until);
    }

    fn mark_success(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
    }

    fn mark_failure(&self) {
        self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
    }

    fn status(&self, index: usize) -> KeyStatus {
        let now = Utc::now();
        let state = if self.available_at(now).is_some() {
            if self
                .rate_limited_until
                .read()
                .unwrap()
                .map(|t| t > now)
                .unwrap_or(false)
            {
                "rate_limited"
            } else {
                "blocked"
            }
        } else if self.consecutive_failures.load(Ordering::SeqCst) > 0 {
            "failing"
        } else {
            "healthy"
        };
        KeyStatus {
            index,
            masked_key: mask_key(&self.key),
            state,
            available_at: self.available_at(now),
            consecutive_failures: self.consecutive_failures.load(Ordering::SeqCst),
        }
    }
}

/// A pool of keys with rotation + per-key health.
pub struct KeyPool {
    slots: Vec<KeySlot>,
    policy: RotationPolicy,
    next: AtomicUsize,
    /// Recently used key index (for failover stickiness).
    last_used: AtomicUsize,
}

impl KeyPool {
    /// Pool from keys (empty entries are dropped) and a policy.
    pub fn new(keys: impl IntoIterator<Item = String>, policy: RotationPolicy) -> Self {
        let slots: Vec<KeySlot> = keys
            .into_iter()
            .filter(|k| !k.trim().is_empty())
            .map(KeySlot::new)
            .collect();
        Self {
            slots,
            policy,
            next: AtomicUsize::new(0),
            last_used: AtomicUsize::new(0),
        }
    }

    /// Number of keys in the pool.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Selects the next usable key index, or `None` when every key is
    /// rate-limited/blocked. Advances the round-robin cursor.
    pub fn select(&self) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        let now = Utc::now();
        let n = self.slots.len();
        let start = match self.policy {
            RotationPolicy::RoundRobin => self.next.fetch_add(1, Ordering::SeqCst) % n,
            RotationPolicy::Failover => {
                let sticky = self.last_used.load(Ordering::SeqCst);
                if sticky < n && self.slots[sticky].is_usable(now) {
                    sticky
                } else {
                    self.next.fetch_add(1, Ordering::SeqCst) % n
                }
            }
        };
        for offset in 0..n {
            let idx = (start + offset) % n;
            if self.slots[idx].is_usable(now) {
                self.last_used.store(idx, Ordering::SeqCst);
                return Some(idx);
            }
        }
        None
    }

    /// Earliest time any key becomes usable again (for Retry-After).
    pub fn earliest_release(&self) -> Option<DateTime<Utc>> {
        let now = Utc::now();
        self.slots.iter().filter_map(|s| s.available_at(now)).min()
    }

    /// Records a successful call on the key.
    pub fn mark_success(&self, index: usize) {
        if let Some(slot) = self.slots.get(index) {
            slot.mark_success();
        }
    }

    /// Classifies a provider error and marks the key accordingly.
    pub fn mark_error(&self, index: usize, err: &ProviderError) {
        let Some(slot) = self.slots.get(index) else {
            return;
        };
        match err.kind {
            ProviderErrorKind::RateLimit => {
                let secs = err.retry_after_seconds.unwrap_or(30);
                slot.mark_rate_limited(secs);
            }
            ProviderErrorKind::AuthenticationError => slot.mark_blocked(AUTH_BLOCK_SECS),
            ProviderErrorKind::QuotaExhausted => slot.mark_blocked(QUOTA_BLOCK_SECS),
            _ => slot.mark_failure(),
        }
    }

    /// Status snapshots for every key (masked).
    pub fn key_status(&self) -> Vec<KeyStatus> {
        self.slots
            .iter()
            .enumerate()
            .map(|(i, s)| s.status(i))
            .collect()
    }

    /// Raw key by index (for building the underlying adapter).
    pub fn key(&self, index: usize) -> Option<&str> {
        self.slots.get(index).map(|s| s.key.as_str())
    }
}

/// Masks a key for logs/status: keep first 4 and last 4 characters.
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}****{tail}")
}

/// A [`SearchProvider`] that rotates across multiple API keys.
///
/// `factory` builds the single-key adapter for one credential (e.g.
/// `TavilyProvider::new(key)`); adapters are created lazily and cached so
/// the HTTP client is built once per key.
pub struct RotatingProvider<F> {
    id: &'static str,
    capabilities: ProviderCapabilities,
    pool: KeyPool,
    factory: F,
    adapters: RwLock<Vec<Option<Arc<dyn SearchProvider>>>>,
}

impl<F> RotatingProvider<F>
where
    F: Fn(&str) -> Arc<dyn SearchProvider> + Send + Sync,
{
    /// Wraps a key pool + adapter factory.
    pub fn new(
        id: &'static str,
        capabilities: ProviderCapabilities,
        pool: KeyPool,
        factory: F,
    ) -> Self {
        let n = pool.len();
        Self {
            id,
            capabilities,
            pool,
            factory,
            adapters: RwLock::new((0..n).map(|_| None).collect()),
        }
    }

    /// Number of keys in the pool.
    pub fn key_count(&self) -> usize {
        self.pool.len()
    }

    /// Lazily builds (and caches) the adapter for one key index.
    fn adapter_for(&self, index: usize) -> Option<Arc<dyn SearchProvider>> {
        if let Some(Some(a)) = self.adapters.read().unwrap().get(index) {
            return Some(Arc::clone(a));
        }
        let key = self.pool.key(index)?.to_string();
        let adapter = (self.factory)(&key);
        let mut slots = self.adapters.write().unwrap();
        if let Some(slot) = slots.get_mut(index) {
            *slot = Some(Arc::clone(&adapter));
        }
        Some(adapter)
    }
}

#[async_trait]
impl<F> SearchProvider for RotatingProvider<F>
where
    F: Fn(&str) -> Arc<dyn SearchProvider> + Send + Sync,
{
    fn id(&self) -> &str {
        self.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    /// Rotates across keys until one succeeds or every usable key fails.
    async fn search(&self, params: &SearchParams) -> Result<ProviderResult, ProviderError> {
        if self.pool.is_empty() {
            return Err(ProviderError::auth(
                self.id,
                "no API keys configured (set TAVILY_API_KEYS / BRAVE_API_KEYS)",
            ));
        }
        let attempts = self.pool.len().max(1);
        let mut last_err: Option<ProviderError> = None;
        for _ in 0..attempts {
            let Some(index) = self.pool.select() else {
                // Every key is rate-limited/blocked: report the earliest
                // release so the caller can honor it.
                let retry_after = self
                    .pool
                    .earliest_release()
                    .map(|t| (t - Utc::now()).num_seconds().max(1) as u64);
                return Err(ProviderError {
                    provider_id: self.id.into(),
                    kind: ProviderErrorKind::RateLimit,
                    message: "all API keys are rate limited or blocked".into(),
                    retry_after_seconds: retry_after,
                    status: Some(429),
                });
            };
            let Some(adapter) = self.adapter_for(index) else {
                continue;
            };
            match adapter.search(params).await {
                Ok(result) => {
                    self.pool.mark_success(index);
                    return Ok(result);
                }
                Err(err) => {
                    self.pool.mark_error(index, &err);
                    last_err = Some(err.clone());
                    // Non-retryable failures (bad payload, parse errors) are
                    // returned immediately: another key would fail the same
                    // way. Auth/quota/rate-limit/timeout/network errors try
                    // the next key — that is the point of rotation.
                    if !err.kind.retryable()
                        && !matches!(
                            err.kind,
                            ProviderErrorKind::AuthenticationError
                                | ProviderErrorKind::QuotaExhausted
                        )
                    {
                        return Err(err);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| ProviderError {
            provider_id: self.id.into(),
            kind: ProviderErrorKind::ProviderUnavailable,
            message: "all API keys exhausted".into(),
            retry_after_seconds: None,
            status: None,
        }))
    }

    /// Reports per-key status (masked) for diagnostics.
    fn key_status(&self) -> Option<Vec<KeyStatus>> {
        Some(self.pool.key_status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderErrorKind;
    use crate::traits::{ProviderResult, SearchResultEntry};
    use std::sync::Mutex;

    fn pool(keys: &[&str]) -> KeyPool {
        KeyPool::new(
            keys.iter().map(|s| s.to_string()),
            RotationPolicy::RoundRobin,
        )
    }

    #[test]
    fn round_robin_cycles_through_keys() {
        let p = pool(&["k1", "k2", "k3"]);
        let order: Vec<usize> = (0..6).filter_map(|_| p.select()).collect();
        assert_eq!(order, vec![0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn rate_limited_key_is_skipped() {
        let p = pool(&["k1", "k2"]);
        p.mark_error(
            0,
            &ProviderError {
                provider_id: "tavily".into(),
                kind: ProviderErrorKind::RateLimit,
                message: "429".into(),
                retry_after_seconds: Some(60),
                status: Some(429),
            },
        );
        // cursor starts at 0 → k1 unusable → k2
        assert_eq!(p.select(), Some(1));
        assert_eq!(p.select(), Some(1), "k1 still limited");
    }

    #[test]
    fn all_keys_limited_returns_none() {
        let p = pool(&["k1", "k2"]);
        for i in 0..2 {
            p.mark_error(
                i,
                &ProviderError {
                    provider_id: "tavily".into(),
                    kind: ProviderErrorKind::RateLimit,
                    message: "429".into(),
                    retry_after_seconds: Some(60),
                    status: Some(429),
                },
            );
        }
        assert!(p.select().is_none());
        assert!(p.earliest_release().is_some());
    }

    #[test]
    fn auth_error_blocks_key() {
        let p = pool(&["k1", "k2"]);
        p.mark_error(0, &ProviderError::auth("tavily", "invalid key"));
        assert_eq!(p.select(), Some(1), "blocked key skipped");
        let status = p.key_status();
        assert_eq!(status[0].state, "blocked");
        assert_eq!(status[1].state, "healthy");
    }

    #[test]
    fn success_resets_failures() {
        let p = pool(&["k1"]);
        for _ in 0..3 {
            p.mark_error(0, &ProviderError::timeout("tavily", "timeout"));
        }
        assert_eq!(p.key_status()[0].consecutive_failures, 3);
        p.mark_success(0);
        assert_eq!(p.key_status()[0].consecutive_failures, 0);
        assert_eq!(p.key_status()[0].state, "healthy");
    }

    #[test]
    fn failover_policy_is_sticky() {
        let p = KeyPool::new(
            vec!["k1".to_string(), "k2".to_string()],
            RotationPolicy::Failover,
        );
        assert_eq!(p.select(), Some(0));
        assert_eq!(p.select(), Some(0), "stays on first healthy key");
        p.mark_error(
            0,
            &ProviderError {
                provider_id: "tavily".into(),
                kind: ProviderErrorKind::RateLimit,
                message: "429".into(),
                retry_after_seconds: Some(60),
                status: Some(429),
            },
        );
        assert_eq!(p.select(), Some(1), "moves on after failure");
    }

    #[test]
    fn mask_key_never_exposes_full_key() {
        assert_eq!(mask_key("tvly-abcdef-12345678"), "tvly****5678");
        assert_eq!(mask_key("short"), "*****");
        assert!(!mask_key("tvly-abcdef-12345678").contains("abcdef"));
    }

    struct StubProvider {
        id: &'static str,
        behavior: Mutex<std::collections::VecDeque<Result<ProviderResult, ProviderError>>>,
    }

    #[async_trait]
    impl SearchProvider for StubProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(&self, _params: &SearchParams) -> Result<ProviderResult, ProviderError> {
            self.behavior
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(ProviderResult {
                        entries: vec![SearchResultEntry {
                            title: Some("ok".into()),
                            url: "https://example.com".into(),
                            content: "c".into(),
                            score: 0.5,
                            published_at: None,
                            raw_content: None,
                        }],
                        duration_ms: 1,
                        bytes_transferred: 1,
                    })
                })
        }
    }

    fn ok_result() -> ProviderResult {
        ProviderResult {
            entries: vec![],
            duration_ms: 1,
            bytes_transferred: 1,
        }
    }

    fn err(kind: ProviderErrorKind) -> ProviderError {
        ProviderError {
            provider_id: "tavily".into(),
            kind,
            message: "stub".into(),
            retry_after_seconds: None,
            status: None,
        }
    }

    #[tokio::test]
    async fn rotating_provider_falls_through_on_rate_limit() {
        let behaviors: Vec<Arc<StubProvider>> = vec![
            Arc::new(StubProvider {
                id: "tavily",
                behavior: Mutex::new(std::collections::VecDeque::from(vec![Err(err(
                    ProviderErrorKind::RateLimit,
                ))])),
            }),
            Arc::new(StubProvider {
                id: "tavily",
                behavior: Mutex::new(std::collections::VecDeque::from(vec![Ok(ok_result())])),
            }),
        ];
        let built = Mutex::new(Vec::new());
        let provider = RotatingProvider::new(
            "tavily",
            ProviderCapabilities {
                search: true,
                ..Default::default()
            },
            KeyPool::new(
                vec!["k1".to_string(), "k2".to_string()],
                RotationPolicy::RoundRobin,
            ),
            |key: &str| {
                let idx = if key == "k1" { 0 } else { 1 };
                let a = Arc::clone(&behaviors[idx]);
                built.lock().unwrap().push(key.to_string());
                a
            },
        );
        let result = provider.search(&SearchParams::simple("q", 5)).await;
        assert!(result.is_ok(), "second key succeeds: {result:?}");
        // k1 was rate-limited → next call must use k2 again
        let result2 = provider.search(&SearchParams::simple("q", 5)).await;
        assert!(result2.is_ok());
        let status = provider.key_status().unwrap();
        assert_eq!(status[0].state, "rate_limited");
        assert_eq!(status[1].state, "healthy");
    }

    #[tokio::test]
    async fn rotating_provider_all_keys_fail() {
        let behavior = Arc::new(StubProvider {
            id: "tavily",
            behavior: Mutex::new(std::collections::VecDeque::from(vec![
                Err(err(ProviderErrorKind::RateLimit)),
                Err(err(ProviderErrorKind::RateLimit)),
            ])),
        });
        let provider = RotatingProvider::new(
            "tavily",
            ProviderCapabilities {
                search: true,
                ..Default::default()
            },
            KeyPool::new(
                vec!["k1".to_string(), "k2".to_string()],
                RotationPolicy::RoundRobin,
            ),
            move |_| Arc::clone(&behavior) as Arc<dyn SearchProvider>,
        );
        let result = provider.search(&SearchParams::simple("q", 5)).await;
        assert!(result.is_err());
        // both keys now limited → next call returns 429 with retry hint
        let result2 = provider.search(&SearchParams::simple("q", 5)).await;
        let e = result2.unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::RateLimit);
        assert_eq!(e.status, Some(429));
        assert!(e.retry_after_seconds.is_some());
    }

    #[tokio::test]
    async fn rotating_provider_auth_error_tries_next_key() {
        let behaviors: Vec<Arc<StubProvider>> = vec![
            Arc::new(StubProvider {
                id: "tavily",
                behavior: Mutex::new(std::collections::VecDeque::from(vec![Err(err(
                    ProviderErrorKind::AuthenticationError,
                ))])),
            }),
            Arc::new(StubProvider {
                id: "tavily",
                behavior: Mutex::new(std::collections::VecDeque::from(vec![Ok(ok_result())])),
            }),
        ];
        let provider = RotatingProvider::new(
            "tavily",
            ProviderCapabilities {
                search: true,
                ..Default::default()
            },
            KeyPool::new(
                vec!["k1".to_string(), "k2".to_string()],
                RotationPolicy::RoundRobin,
            ),
            |key: &str| {
                let idx = if key == "k1" { 0 } else { 1 };
                Arc::clone(&behaviors[idx]) as Arc<dyn SearchProvider>
            },
        );
        let result = provider.search(&SearchParams::simple("q", 5)).await;
        assert!(result.is_ok(), "valid key after blocked one: {result:?}");
        assert_eq!(provider.key_status().unwrap()[0].state, "blocked");
    }

    #[tokio::test]
    async fn empty_pool_reports_auth_error() {
        let provider = RotatingProvider::new(
            "tavily",
            ProviderCapabilities {
                search: true,
                ..Default::default()
            },
            KeyPool::new(Vec::<String>::new(), RotationPolicy::RoundRobin),
            |_k: &str| {
                Arc::new(StubProvider {
                    id: "tavily",
                    behavior: Mutex::new(std::collections::VecDeque::new()),
                })
            },
        );
        let e = provider
            .search(&SearchParams::simple("q", 5))
            .await
            .unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::AuthenticationError);
    }
}
