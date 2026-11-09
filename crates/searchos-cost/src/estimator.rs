//! Cost estimation per operation (RFC-008 8.2/8.4, Part 30 INV-30-13).
//!
//! The estimator dispatches on the declared `PricingModel`: request-based
//! prices scale with requests, per-thousand-unit prices scale with actual
//! units / 1000, and parse failures are reported instead of silently becoming
//! zero.

use searchos_domain::cost::{CostOperation, Money};
use std::collections::HashMap;

use crate::model::{CostModelConfig, PricingModel, ResourceRates};

/// Estimates costs for operations using the configured pricing models.
#[derive(Debug, Clone)]
pub struct CostEstimator {
    /// provider_id → (declared pricing model, currency).
    providers: HashMap<String, (PricingModel, String)>,
    rates: ResourceRates,
    /// Misconfigured prices are observable through `parse_errors()` and
    /// `CostModelConfig::validate_prices()`, never silent zeroes.
    parse_errors: Vec<String>,
}

impl CostEstimator {
    /// Builds an estimator from a cost model config.
    pub fn from_config(config: &CostModelConfig) -> Self {
        let mut providers = HashMap::new();
        for p in &config.providers {
            providers.insert(
                p.provider_id.clone(),
                (p.pricing.clone(), p.currency.clone()),
            );
        }
        let parse_errors = config.validate_prices();
        for e in &parse_errors {
            tracing::warn!(error = %e, "cost pricing parse failure");
        }
        Self {
            providers,
            rates: config.local.clone(),
            parse_errors,
        }
    }

    /// Pricing errors found while loading the model (never swallowed).
    pub fn parse_errors(&self) -> &[String] {
        &self.parse_errors
    }

    /// Estimates one external request for a provider (one unit).
    pub fn estimate_request(&self, provider_id: &str) -> Money {
        self.estimate_request_with_units(provider_id, 1)
    }

    /// Estimates `units` requests / tokens / pages / chunks for a provider.
    pub fn estimate_request_with_units(&self, provider_id: &str, units: u64) -> Money {
        let Some((model, currency)) = self.providers.get(provider_id) else {
            return Money::zero(&self.rates.currency);
        };
        let micros = match model {
            PricingModel::RequestBased { unit_price } => {
                let per = parse_micros(unit_price, provider_id, &mut Vec::new());
                per.saturating_mul(units.max(1) as i64)
            }
            PricingModel::PerThousandUnits { unit_price_per_k } => {
                let per_k = parse_micros(unit_price_per_k, provider_id, &mut Vec::new());
                // Ceiling division: any partial 1k-unit block is billable.
                let amount = (per_k as i128 * units.max(1) as i128 + 999) / 1000;
                amount.clamp(0, i64::MAX as i128) as i64
            }
            PricingModel::ResourceBased { .. } => 0,
        };
        Money {
            amount_micros: micros,
            currency: currency.clone(),
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
            .providers
            .get(provider_id.unwrap_or(""))
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| self.rates.currency.clone());
        let units = input_units.saturating_add(output_units);
        match operation {
            CostOperation::WebSearch
            | CostOperation::WebFetch
            | CostOperation::WebCrawl
            | CostOperation::Reranking
            | CostOperation::Embedding
            | CostOperation::QueryInference => {
                let per = provider_id
                    .and_then(|p| self.providers.get(p))
                    .map(|(model, _)| model.clone())
                    .unwrap_or(PricingModel::ResourceBased {
                        electricity_cost_per_kwh: "0".into(),
                        amortization_enabled: false,
                    });
                let micros = match per {
                    PricingModel::RequestBased { unit_price } => {
                        parse_micros(&unit_price, provider_id.unwrap_or(""), &mut Vec::new())
                            .saturating_mul(units.max(1) as i64)
                    }
                    PricingModel::PerThousandUnits { unit_price_per_k } => {
                        let per_k = parse_micros(
                            &unit_price_per_k,
                            provider_id.unwrap_or(""),
                            &mut Vec::new(),
                        );
                        ((per_k as i128 * units.max(1) as i128 + 999) / 1000)
                            .clamp(0, i64::MAX as i128) as i64
                    }
                    PricingModel::ResourceBased { .. } => 0,
                };
                Money {
                    amount_micros: micros,
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
                self.estimate_transfer_bytes(units)
            }
            CostOperation::ProviderAttempt => Money::zero(&currency),
        }
    }

    /// CPU-milliseconds → money via cost_per_cpu_second.
    pub fn estimate_cpu_millis(&self, cpu_millis: u64) -> Money {
        let per_second = parse_micros(
            &self.rates.cost_per_cpu_second,
            "local.cost_per_cpu_second",
            &mut Vec::new(),
        );
        let micros = ((per_second as u128 * cpu_millis as u128 + 500) / 1000) as i64;
        Money {
            amount_micros: micros,
            currency: self.rates.currency.clone(),
        }
    }

    /// Bytes stored → money via cost_per_gb_month (1 month amortization).
    pub fn estimate_storage_bytes(&self, bytes: u64) -> Money {
        let per_gb = parse_micros(
            &self.rates.cost_per_gb_month,
            "local.cost_per_gb_month",
            &mut Vec::new(),
        );
        let micros = (per_gb as u128 * bytes as u128 / (1024 * 1024 * 1024)) as i64;
        Money {
            amount_micros: micros,
            currency: self.rates.currency.clone(),
        }
    }

    /// Bytes transferred → money via cost_per_gb_transfer.
    pub fn estimate_transfer_bytes(&self, bytes: u64) -> Money {
        let per_gb = parse_micros(
            &self.rates.cost_per_gb_transfer,
            "local.cost_per_gb_transfer",
            &mut Vec::new(),
        );
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

fn parse_micros(raw: &str, field: &str, errors: &mut Vec<String>) -> i64 {
    match Money::parse(raw, "USD") {
        Ok(m) => m.amount_micros,
        Err(e) => {
            errors.push(format!("{field}: {e}"));
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderPricing;

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
        let e = CostEstimator::from_config(&CostModelConfig::default_config());
        assert!(e.estimate_cpu_millis(5000).amount_micros > 0);
    }

    /// Part 30 §30.11: PerThousandUnits must use unit counts, not per-request.
    #[test]
    fn per_thousand_units_pricing_uses_unit_counts() {
        let mut config = CostModelConfig::default_config();
        config.upsert_provider(ProviderPricing {
            provider_id: "token_api".into(),
            currency: "USD".into(),
            pricing: PricingModel::PerThousandUnits {
                unit_price_per_k: "0.010".into(), // 10_000 micros / 1k units
            },
            version: 1,
            from_real_bill: false,
        });
        let e = CostEstimator::from_config(&config);
        assert_eq!(
            e.estimate_request_with_units("token_api", 1).amount_micros,
            10
        );
        assert_eq!(
            e.estimate_request_with_units("token_api", 2_000)
                .amount_micros,
            20_000
        );
        assert_eq!(
            e.estimate(CostOperation::WebSearch, Some("token_api"), 3_000, 0)
                .amount_micros,
            30_000
        );
    }

    /// Part 30 §30.11: a malformed price must be visible, never silently zero.
    #[test]
    fn cost_parse_failure_is_reported() {
        let mut config = CostModelConfig::default_config();
        config.upsert_provider(ProviderPricing {
            provider_id: "broken".into(),
            currency: "USD".into(),
            pricing: PricingModel::RequestBased {
                unit_price: "not-a-price".into(),
            },
            version: 1,
            from_real_bill: false,
        });
        let errors = config.validate_prices();
        assert!(errors.iter().any(|e| e.contains("broken")));
        let e = CostEstimator::from_config(&config);
        assert!(e.parse_errors().iter().any(|x| x.contains("broken")));
    }
}
