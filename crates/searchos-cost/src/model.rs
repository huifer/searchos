//! Pricing model configuration (RFC-008 8.4).
//!
//! The YAML example in the RFC shows the config *format*; the numbers are not
//! real vendor quotes. Every computation records pricing model version,
//! currency and billing time.

use searchos_domain::cost::CostOperation;
use serde::{Deserialize, Serialize};

/// How a provider or resource is priced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "pricing_model", rename_all = "snake_case")]
pub enum PricingModel {
    /// Flat price per request (e.g. search APIs).
    RequestBased {
        /// Decimal price per request, e.g. "0.005".
        unit_price: String,
    },
    /// Price per 1k units (tokens / rows / MB).
    PerThousandUnits { unit_price_per_k: String },
    /// Resource-based: electricity + amortization (RFC-008 8.4 local section).
    ResourceBased {
        electricity_cost_per_kwh: String,
        amortization_enabled: bool,
    },
}

/// Pricing entry for one provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderPricing {
    pub provider_id: String,
    pub currency: String,
    pub pricing: PricingModel,
    /// Version of this pricing entry (RFC-008 8.4).
    pub version: u32,
    /// Whether the amounts come from a real bill (`false` = estimate).
    pub from_real_bill: bool,
}

/// Local resource conversion rates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRates {
    pub currency: String,
    /// Decimal cost per CPU-second estimate.
    pub cost_per_cpu_second: String,
    /// Decimal cost per GB stored per month (converted per GB-write).
    pub cost_per_gb_month: String,
    /// Decimal cost per GB transferred.
    pub cost_per_gb_transfer: String,
    /// Pricing config version.
    pub version: u32,
}

impl Default for ResourceRates {
    fn default() -> Self {
        // RFC-008 example rates (illustrative, not real quotes).
        Self {
            currency: "USD".into(),
            cost_per_cpu_second: "0.000100".into(), // illustrative: ~$0.36 per CPU-hour class machine (micros-precision)
            cost_per_gb_month: "0.10".into(),
            cost_per_gb_transfer: "0.09".into(),
            version: 1,
        }
    }
}

/// Full cost model configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CostModelConfig {
    pub providers: Vec<ProviderPricing>,
    pub local: ResourceRates,
}

impl CostModelConfig {
    /// Pricing lookup for a provider.
    pub fn provider(&self, provider_id: &str) -> Option<&ProviderPricing> {
        self.providers.iter().find(|p| p.provider_id == provider_id)
    }

    /// Registers or replaces provider pricing.
    pub fn upsert_provider(&mut self, pricing: ProviderPricing) {
        if let Some(p) = self
            .providers
            .iter_mut()
            .find(|p| p.provider_id == pricing.provider_id)
        {
            *p = pricing;
        } else {
            self.providers.push(pricing);
        }
    }

    /// Default config: Tavily + Brave request pricing (RFC-009: MVP providers)
    /// plus local resource rates. Illustrative estimates.
    pub fn default_config() -> Self {
        Self {
            providers: vec![
                ProviderPricing {
                    provider_id: "tavily".into(),
                    currency: "USD".into(),
                    pricing: PricingModel::RequestBased {
                        unit_price: "0.005".into(),
                    },
                    version: 1,
                    from_real_bill: false,
                },
                ProviderPricing {
                    provider_id: "brave".into(),
                    currency: "USD".into(),
                    pricing: PricingModel::RequestBased {
                        unit_price: "0.003".into(),
                    },
                    version: 1,
                    from_real_bill: false,
                },
            ],
            local: ResourceRates::default(),
        }
    }

    /// Currency used by a provider (or local default).
    pub fn currency_for(&self, provider_id: Option<&str>) -> String {
        match provider_id.and_then(|p| self.provider(p)) {
            Some(p) => p.currency.clone(),
            None => self.local.currency.clone(),
        }
    }
}

/// Operation → provider pricing resolution used by the estimator.
pub fn pricing_for_operation<'a>(
    config: &'a CostModelConfig,
    operation: CostOperation,
    provider_id: Option<&str>,
) -> (Option<&'a PricingModel>, String) {
    let currency = config.currency_for(provider_id);
    match operation {
        CostOperation::WebSearch
        | CostOperation::WebFetch
        | CostOperation::WebCrawl
        | CostOperation::Embedding
        | CostOperation::Reranking
        | CostOperation::QueryInference => {
            let pm = provider_id
                .and_then(|p| config.provider(p))
                .map(|p| &p.pricing);
            (pm, currency)
        }
        _ => (None, currency),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_mvp_providers() {
        let c = CostModelConfig::default_config();
        assert!(c.provider("tavily").is_some());
        assert!(c.provider("brave").is_some());
        assert_eq!(c.local.currency, "USD");
    }

    #[test]
    fn pricing_roundtrip_serializable() {
        let pm = PricingModel::RequestBased {
            unit_price: "0.005".into(),
        };
        let json = serde_json::to_string(&pm).unwrap();
        let back: PricingModel = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pm);
    }

    #[test]
    fn money_for_operation_zero_without_provider() {
        use searchos_domain::cost::Money;
        let config = CostModelConfig::default_config();
        let (pm, cur) = pricing_for_operation(&config, CostOperation::LocalBm25, None);
        assert!(pm.is_none());
        assert_eq!(Money::zero(cur), Money::zero("USD"));
    }
}
