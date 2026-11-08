//! Cost estimation per operation (RFC-008 8.2/8.4).

use searchos_domain::cost::{CostOperation, Money};
use std::collections::HashMap;

use crate::model::{PricingModel, ResourceRates};

/// Estimates costs for operations using the configured pricing models.
#[derive(Debug, Clone)]
pub struct CostEstimator {
    /// provider_id → estimated per-request cost in micros.
    provider_request_micros: HashMap<String, (i64, String)>,
    rates: ResourceRates,
}

impl CostEstimator {
    /// Builds an estimator from a cost model config.
    pub fn from_config(config: &crate::model::CostModelConfig) -> Self {
        let mut map = HashMap::new();
        for p in &config.providers {
            let micros = match &p.pricing {
                PricingModel::RequestBased { unit_price } => Money::parse(unit_price, &p.currency)
                    .map(|m| m.amount_micros)
                    .unwrap_or(0),
                PricingModel::PerThousandUnits { unit_price_per_k } => {
                    Money::parse(unit_price_per_k, &p.currency)
                        .map(|m| m.amount_micros)
                        .unwrap_or(0)
                }
                PricingModel::ResourceBased { .. } => 0,
            };
            map.insert(p.provider_id.clone(), (micros, p.currency.clone()));
        }
        Self {
            provider_request_micros: map,
            rates: config.local.clone(),
        }
    }

    /// Estimates one external request for a provider.
    pub fn estimate_request(&self, provider_id: &str) -> Money {
        match self.provider_request_micros.get(provider_id) {
            Some((micros, currency)) => Money {
                amount_micros: *micros,
                currency: currency.clone(),
            },
            None => Money::zero(&self.rates.currency),
        }
    }

    /// Estimates a whole operation with unit counts.
    pub fn estimate(
        &self,
        operation: CostOperation,
        provider_id: Option<&str>,
        input_units: u64,
        output_units: u64,
    ) -> Money {
        let currency = self
            .provider_request_micros
            .get(provider_id.unwrap_or(""))
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| self.rates.currency.clone());
        match operation {
            CostOperation::WebSearch
            | CostOperation::WebFetch
            | CostOperation::WebCrawl
            | CostOperation::Reranking => {
                let per = provider_id
                    .and_then(|p| self.provider_request_micros.get(p))
                    .map(|(m, _)| *m)
                    .unwrap_or(0);
                let units = input_units.saturating_add(output_units).max(1);
                Money {
                    amount_micros: per.saturating_mul(units as i64),
                    currency,
                }
            }
            CostOperation::Embedding => {
                // per-1k-token style pricing if configured, else zero.
                let per_k = provider_id
                    .and_then(|p| self.provider_request_micros.get(p))
                    .map(|(m, _)| *m)
                    .unwrap_or(0);
                let k = (input_units / 1000).max(if input_units > 0 { 1 } else { 0 });
                Money {
                    amount_micros: per_k.saturating_mul(k as i64),
                    currency,
                }
            }
            CostOperation::LocalBm25 | CostOperation::LocalVector | CostOperation::LocalCompute => {
                // input_units interpreted as cpu-milliseconds.
                self.estimate_cpu_millis(input_units)
            }
            CostOperation::StorageWrite | CostOperation::StorageRead => {
                // input_units interpreted as bytes written/read.
                self.estimate_storage_bytes(input_units)
            }
            CostOperation::IndexMaintenance => self.estimate_cpu_millis(input_units),
            CostOperation::NetworkTransfer => {
                // input_units interpreted as bytes transferred (both ways).
                self.estimate_transfer_bytes(input_units.saturating_add(output_units))
            }
            CostOperation::QueryInference => {
                let per = provider_id
                    .and_then(|p| self.provider_request_micros.get(p))
                    .map(|(m, _)| *m)
                    .unwrap_or(0);
                Money {
                    amount_micros: per.saturating_mul(input_units.max(1) as i64),
                    currency,
                }
            }
        }
    }

    /// CPU-milliseconds → money via cost_per_cpu_second.
    pub fn estimate_cpu_millis(&self, cpu_millis: u64) -> Money {
        let per_second = Money::parse(&self.rates.cost_per_cpu_second, &self.rates.currency)
            .map(|m| m.amount_micros)
            .unwrap_or(0);
        let micros = ((per_second as u128 * cpu_millis as u128 + 500) / 1000) as i64; // round to nearest micro
        Money {
            amount_micros: micros,
            currency: self.rates.currency.clone(),
        }
    }

    /// Bytes stored → money via cost_per_gb_month (1 month amortization).
    pub fn estimate_storage_bytes(&self, bytes: u64) -> Money {
        let per_gb = Money::parse(&self.rates.cost_per_gb_month, &self.rates.currency)
            .map(|m| m.amount_micros)
            .unwrap_or(0);
        let micros = (per_gb as u128 * bytes as u128 / (1024 * 1024 * 1024)) as i64;
        Money {
            amount_micros: micros,
            currency: self.rates.currency.clone(),
        }
    }

    /// Bytes transferred → money via cost_per_gb_transfer.
    pub fn estimate_transfer_bytes(&self, bytes: u64) -> Money {
        let per_gb = Money::parse(&self.rates.cost_per_gb_transfer, &self.rates.currency)
            .map(|m| m.amount_micros)
            .unwrap_or(0);
        let micros = (per_gb as u128 * bytes as u128 / (1024 * 1024 * 1024)) as i64;
        Money {
            amount_micros: micros,
            currency: self.rates.currency.clone(),
        }
    }

    /// Rates in use (for trace/debug output).
    pub fn rates(&self) -> &ResourceRates {
        &self.rates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CostModelConfig;

    #[test]
    fn tavily_estimate_is_5_000_micros() {
        let e = CostEstimator::from_config(&CostModelConfig::default_config());
        let m = e.estimate_request("tavily");
        assert_eq!(m.amount_micros, 5_000);
        assert_eq!(m.currency, "USD");
        let b = e.estimate_request("brave");
        assert_eq!(b.amount_micros, 3_000);
        let unknown = e.estimate_request("nope");
        assert_eq!(unknown.amount_micros, 0);
    }

    #[test]
    fn cpu_estimate_scales() {
        let e = CostEstimator::from_config(&CostModelConfig::default_config());
        let one_second = e.estimate_cpu_millis(1000);
        let ten_seconds = e.estimate_cpu_millis(10_000);
        assert_eq!(ten_seconds.amount_micros, one_second.amount_micros * 10);
    }

    #[test]
    fn local_never_free() {
        // RFC-008 8.6: 不能把本地成本记为零 — configured rate > 0 yields > 0.
        let e = CostEstimator::from_config(&CostModelConfig::default_config());
        assert!(e.estimate_cpu_millis(5000).amount_micros > 0);
    }
}
