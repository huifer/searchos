//! Provider registry with health tracking (RFC-009 9.5, RFC-014 14.4).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use searchos_domain::SearchOsError;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::circuit::{CircuitBreaker, CircuitState};
use crate::error::ProviderError;
use crate::traits::{FetchProvider, SearchProvider};
use searchos_planner::router::{ProviderEntry, ProviderHealth, ProviderRouter, RouterWeights};

/// A registered provider: adapter + breaker + rate-limit state.
pub struct RegisteredProvider {
    pub id: String,
    pub search: Option<Arc<dyn SearchProvider>>,
    pub fetch: Option<Arc<dyn FetchProvider>>,
    pub breaker: CircuitBreaker,
    pub rate_limited_until: RwLock<Option<DateTime<Utc>>>,
    /// Quality/coverage/freshness/latency/reliability estimates updated
    /// from observed outcomes (seeded at registration).
    pub quality: RwLock<f32>,
    pub coverage: RwLock<f32>,
    pub freshness: RwLock<f32>,
    pub latency_ms: RwLock<u64>,
    pub reliability: RwLock<f32>,
    pub cost_micros: i64,
    pub currency: String,
}

impl RegisteredProvider {
    /// Registers a search provider with seed stats.
    pub fn from_search(
        provider: Arc<dyn SearchProvider>,
        cost_micros: i64,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            id: provider.id().to_string(),
            search: Some(provider),
            fetch: None,
            breaker: CircuitBreaker::new(5, Duration::from_secs(30)),
            rate_limited_until: RwLock::new(None),
            quality: RwLock::new(0.7),
            coverage: RwLock::new(0.7),
            freshness: RwLock::new(0.7),
            latency_ms: RwLock::new(1_000),
            reliability: RwLock::new(0.95),
            cost_micros,
            currency: currency.into(),
        }
    }

    /// Attaches fetch capability.
    pub fn with_fetch(mut self, fetch: Arc<dyn FetchProvider>) -> Self {
        self.fetch = Some(fetch);
        self
    }

    /// Records a rate-limit response (RFC-009 9.7: 429 follows Retry-After).
    pub fn mark_rate_limited(&self, seconds: u64) {
        *self.rate_limited_until.write().unwrap() =
            Some(Utc::now() + chrono::Duration::seconds(seconds.min(3600) as i64));
    }

    /// Updates observed stats after a successful call.
    pub fn record_success(&self, latency_ms: u64) {
        self.breaker.record_success();
        *self.latency_ms.write().unwrap() = (*self.latency_ms.read().unwrap() + latency_ms) / 2;
        let mut q = self.quality.write().unwrap();
        *q = (*q * 0.9 + 0.1f32).clamp(0.0, 1.0);
        let mut r = self.reliability.write().unwrap();
        *r = (*r * 0.9 + 0.1f32).clamp(0.0, 1.0);
    }

    /// Updates stats after a failure.
    pub fn record_failure(&self, err: &ProviderError) {
        self.breaker.record_failure();
        if err.kind.code() == "rate_limit" {
            let secs = err.retry_after_seconds.unwrap_or(30);
            self.mark_rate_limited(secs);
        }
        let mut r = self.reliability.write().unwrap();
        *r = (*r * 0.9).clamp(0.0, 1.0);
    }

    /// Router entry view of this provider.
    pub fn entry(&self) -> ProviderEntry {
        let rate_limited = *self.rate_limited_until.read().unwrap();
        let health = match self.breaker.state() {
            CircuitState::Closed => ProviderHealth::Healthy,
            CircuitState::HalfOpen => ProviderHealth::Degraded,
            CircuitState::Open => ProviderHealth::Down,
        };
        ProviderEntry {
            id: self.id.clone(),
            capabilities: self
                .search
                .as_ref()
                .map(|s| s.capabilities())
                .or_else(|| self.fetch.as_ref().map(|f| f.capabilities()))
                .unwrap_or_default(),
            quality: *self.quality.read().unwrap(),
            coverage: *self.coverage.read().unwrap(),
            freshness: *self.freshness.read().unwrap(),
            latency_ms: *self.latency_ms.read().unwrap(),
            reliability: *self.reliability.read().unwrap(),
            cost_micros: self.cost_micros,
            currency: self.currency.clone(),
            rate_limited_until: rate_limited,
            health,
        }
    }
}

/// Registry of providers implementing the router port.
pub struct ProviderRegistry {
    providers: RwLock<HashMap<String, Arc<RegisteredProvider>>>,
    weights: RouterWeights,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
            weights: RouterWeights::default(),
        }
    }
}

impl ProviderRegistry {
    /// Empty registry with default weights.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers (or replaces) a provider.
    pub fn register(&self, p: RegisteredProvider) {
        self.providers
            .write()
            .unwrap()
            .insert(p.id.clone(), Arc::new(p));
    }

    /// Looks a provider up by id.
    pub fn get(&self, id: &str) -> Option<Arc<RegisteredProvider>> {
        self.providers.read().unwrap().get(id).cloned()
    }

    /// All provider ids.
    pub fn ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.providers.read().unwrap().keys().cloned().collect();
        v.sort();
        v
    }

    /// Number of registered providers.
    pub fn len(&self) -> usize {
        self.providers.read().unwrap().len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Health status of one provider (for `provider health`).
    pub fn health(&self, id: &str) -> Option<(CircuitState, bool)> {
        self.get(id).map(|p| {
            (
                p.breaker.state(),
                p.rate_limited_until
                    .read()
                    .unwrap()
                    .map(|t| t > Utc::now())
                    .unwrap_or(false),
            )
        })
    }

    /// Custom weights.
    pub fn with_weights(mut self, weights: RouterWeights) -> Self {
        self.weights = weights;
        self
    }
}

#[async_trait]
impl ProviderRouter for ProviderRegistry {
    async fn route(
        &self,
        need_search: bool,
        max_cost_micros: i64,
    ) -> Result<Vec<ProviderEntry>, SearchOsError> {
        let static_router = searchos_planner::router::StaticProviderRouter {
            providers: self
                .providers
                .read()
                .unwrap()
                .values()
                .filter(|p| need_search && p.search.is_some())
                .map(|p| p.entry())
                .collect(),
            weights: self.weights.clone(),
        };
        static_router.route(need_search, max_cost_micros).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderErrorKind;
    use crate::tavily::TavilyProvider;
    use crate::traits::SearchProvider;
    use searchos_domain::provider::ProviderCapabilities;

    #[tokio::test]
    async fn registry_routes_registered_providers() {
        let reg = ProviderRegistry::new();
        reg.register(RegisteredProvider::from_search(
            Arc::new(TavilyProvider::new("k")),
            5_000,
            "USD",
        ));
        reg.register(RegisteredProvider::from_search(
            Arc::new(BraveStub),
            3_000,
            "USD",
        ));
        assert_eq!(reg.ids(), vec!["brave".to_string(), "tavily".to_string()]);
        let ranked = reg.route(true, 50_000).await.unwrap();
        assert_eq!(ranked.len(), 2);
        // cheaper brave ranks first at equal quality
        assert_eq!(ranked[0].id, "brave");
    }

    #[tokio::test]
    async fn breaker_failure_degrades_routing() {
        let reg = ProviderRegistry::new();
        reg.register(RegisteredProvider::from_search(
            Arc::new(TavilyProvider::new("k")),
            5_000,
            "USD",
        ));
        let p = reg.get("tavily").unwrap();
        for _ in 0..5 {
            p.record_failure(&ProviderError {
                provider_id: "tavily".into(),
                kind: ProviderErrorKind::ProviderUnavailable,
                message: "down".into(),
                retry_after_seconds: None,
                status: Some(503),
            });
        }
        let ranked = reg.route(true, 50_000).await.unwrap();
        assert!(ranked.is_empty(), "open breaker must be filtered");
    }

    #[test]
    fn breaker_recovers_after_success() {
        // Recovery path with a realistic cooldown so scheduler jitter cannot
        // race the 1ms boundary (deterministic test, not a timing stressor).
        let b = CircuitBreaker::new(1, Duration::from_millis(50));
        b.record_failure();
        assert_eq!(b.state(), CircuitState::Open);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(b.check(), CircuitState::HalfOpen);
        b.record_success();
        assert_eq!(b.state(), CircuitState::Closed);
    }

    #[test]
    fn health_report() {
        let reg = ProviderRegistry::new();
        reg.register(RegisteredProvider::from_search(
            Arc::new(TavilyProvider::new("k")),
            5_000,
            "USD",
        ));
        let (state, rate_limited) = reg.health("tavily").unwrap();
        assert_eq!(state, CircuitState::Closed);
        assert!(!rate_limited);
        assert!(reg.health("nope").is_none());
    }

    struct BraveStub;
    #[async_trait]
    impl SearchProvider for BraveStub {
        fn id(&self) -> &str {
            "brave"
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(
            &self,
            _params: &crate::traits::SearchParams,
        ) -> Result<crate::traits::ProviderResult, ProviderError> {
            Ok(crate::traits::ProviderResult {
                entries: vec![],
                duration_ms: 1,
                bytes_transferred: 1,
            })
        }
    }
}
