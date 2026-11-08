//! Provider router: capability filter + weighted ranking (RFC-009 9.3/9.4).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use searchos_domain::provider::ProviderCapabilities;
use searchos_domain::SearchOsError;
use serde::{Deserialize, Serialize};

/// Runtime status of one provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    pub capabilities: ProviderCapabilities,
    /// 0..=1 historical quality of results.
    pub quality: f32,
    /// 0..=1 coverage ability.
    pub coverage: f32,
    /// 0..=1 freshness of returned content.
    pub freshness: f32,
    /// Average latency in ms.
    pub latency_ms: u64,
    /// 0..=1 reliability (1 - failure rate).
    pub reliability: f32,
    /// Price per request in micros.
    pub cost_micros: i64,
    pub currency: String,
    /// Whether currently rate limited (with release time).
    pub rate_limited_until: Option<DateTime<Utc>>,
    /// Circuit breaker state: healthy / degraded / down.
    pub health: ProviderHealth,
}

/// Health states (RFC-009 9.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealth {
    Healthy,
    Degraded,
    Down,
}

/// Phase-2 ranking weights: quality and hard constraints sort before cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterWeights {
    pub quality: f32,
    pub coverage: f32,
    pub freshness: f32,
    pub cost: f32,
    pub latency: f32,
    pub reliability: f32,
}

impl Default for RouterWeights {
    fn default() -> Self {
        Self {
            quality: 0.30,
            coverage: 0.15,
            freshness: 0.15,
            cost: 0.20,
            latency: 0.10,
            reliability: 0.10,
        }
    }
}

/// Router selection outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum RouterSelection {
    Selected(ProviderEntry),
    /// No provider satisfies phase-1 filters (RFC-009 9.4).
    None {
        reason: String,
    },
}

/// Provider routing port.
#[async_trait]
pub trait ProviderRouter: Send + Sync {
    /// Returns providers passing phase-1 filters, ranked by phase-2 score.
    async fn route(
        &self,
        need_search: bool,
        max_cost_micros: i64,
    ) -> Result<Vec<ProviderEntry>, SearchOsError>;

    /// Picks the best single provider.
    async fn select(
        &self,
        need_search: bool,
        max_cost_micros: i64,
    ) -> Result<RouterSelection, SearchOsError> {
        let ranked = self.route(need_search, max_cost_micros).await?;
        Ok(match ranked.into_iter().next() {
            Some(p) => RouterSelection::Selected(p),
            None => RouterSelection::None {
                reason: "no provider satisfies capability/quality/health/budget filters".into(),
            },
        })
    }
}

/// Static in-memory router over configured providers.
#[derive(Debug, Clone)]
pub struct StaticProviderRouter {
    pub providers: Vec<ProviderEntry>,
    pub weights: RouterWeights,
}

impl StaticProviderRouter {
    /// New router with providers and default weights.
    pub fn new(providers: Vec<ProviderEntry>) -> Self {
        Self {
            providers,
            weights: RouterWeights::default(),
        }
    }

    /// Phase-2 score: ProviderScore = f(Quality, Coverage, Freshness, Cost,
    /// Latency, Reliability) (RFC-009 9.4). Quality gates sort first: any
    /// provider below the quality floor is excluded in phase 1.
    pub fn score(&self, p: &ProviderEntry) -> f32 {
        let w = &self.weights;
        let quality_term = w.quality * p.quality.clamp(0.0, 1.0);
        let coverage_term = w.coverage * p.coverage.clamp(0.0, 1.0);
        let freshness_term = w.freshness * p.freshness.clamp(0.0, 1.0);
        let cost_term = w.cost * (1.0 - normalized_cost(p.cost_micros));
        let latency_term = w.latency * (1.0 - (p.latency_ms as f32 / 5000.0).clamp(0.0, 1.0));
        let reliability_term = w.reliability * p.reliability.clamp(0.0, 1.0);
        quality_term + coverage_term + freshness_term + cost_term + latency_term + reliability_term
    }
}

fn normalized_cost(micros: i64) -> f32 {
    // Normalize against 0.05 USD reference (50000 micros).
    (micros as f32 / 50_000.0).clamp(0.0, 1.0)
}

#[async_trait]
impl ProviderRouter for StaticProviderRouter {
    async fn route(
        &self,
        need_search: bool,
        max_cost_micros: i64,
    ) -> Result<Vec<ProviderEntry>, SearchOsError> {
        let now = Utc::now();
        // Phase 1 filters (RFC-009 9.4):
        let mut passing: Vec<ProviderEntry> = self
            .providers
            .iter()
            .filter(|p| {
                // capability
                if need_search && !p.capabilities.search {
                    return false;
                }
                // budget
                if p.cost_micros > max_cost_micros {
                    return false;
                }
                // rate limit
                if let Some(until) = p.rate_limited_until {
                    if now < until {
                        return false;
                    }
                }
                // health
                if p.health == ProviderHealth::Down {
                    return false;
                }
                // quality floor (hard constraint sorts before cost, P-001)
                if p.quality < 0.5 {
                    return false;
                }
                true
            })
            .cloned()
            .collect();
        // Phase 2 ranking.
        passing.sort_by(|a, b| {
            let sa = self.score(a);
            let sb = self.score(b);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(passing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, quality: f32, cost: i64, health: ProviderHealth) -> ProviderEntry {
        ProviderEntry {
            id: id.into(),
            capabilities: ProviderCapabilities {
                search: true,
                ..Default::default()
            },
            quality,
            coverage: 0.8,
            freshness: 0.8,
            latency_ms: 500,
            reliability: 0.95,
            cost_micros: cost,
            currency: "USD".into(),
            rate_limited_until: None,
            health,
        }
    }

    #[tokio::test]
    async fn filters_unhealthy_and_over_budget() {
        let mut ratelimited = provider("ratelimited", 0.9, 5_000, ProviderHealth::Healthy);
        ratelimited.rate_limited_until = Some(Utc::now() + chrono::Duration::hours(1));
        let r = StaticProviderRouter::new(vec![
            ratelimited,
            provider("down", 0.9, 5_000, ProviderHealth::Down),
            provider("expensive", 0.9, 500_000, ProviderHealth::Healthy),
            provider("good", 0.9, 5_000, ProviderHealth::Healthy),
        ]);
        let ranked = r.route(true, 10_000).await.unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].id, "good");
    }

    #[tokio::test]
    async fn quality_floor_excluded_even_if_cheapest() {
        // 质量硬约束先于排序: low-quality cheap provider must not win.
        let r = StaticProviderRouter::new(vec![
            provider("cheap_bad", 0.3, 1_000, ProviderHealth::Healthy),
            provider("good", 0.9, 5_000, ProviderHealth::Healthy),
        ]);
        let sel = r.select(true, 50_000).await.unwrap();
        match sel {
            RouterSelection::Selected(p) => assert_eq!(p.id, "good"),
            RouterSelection::None { .. } => panic!("good provider should be selected"),
        }
    }

    #[tokio::test]
    async fn no_provider_when_all_filtered() {
        let r = StaticProviderRouter::new(vec![provider("down", 0.9, 5_000, ProviderHealth::Down)]);
        let sel = r.select(true, 50_000).await.unwrap();
        assert!(matches!(sel, RouterSelection::None { .. }));
    }

    #[test]
    fn cheaper_provider_wins_when_quality_equal() {
        let r = StaticProviderRouter::new(vec![
            provider("a", 0.9, 5_000, ProviderHealth::Healthy),
            provider("b", 0.9, 3_000, ProviderHealth::Healthy),
        ]);
        let ranked = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(r.route(true, 50_000))
            .unwrap();
        assert_eq!(ranked[0].id, "b");
    }
}
