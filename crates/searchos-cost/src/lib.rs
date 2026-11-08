//! Cost accounting (RFC-008).
//!
//! - [`model`] — pricing model configuration and versioning (RFC-008 8.4).
//! - [`estimator`] — cost estimation per operation.
//! - [`ledger`] — append-only ledger logic with aggregation (ADR-015).
//! - [`compute`] — local compute metering: raw resource metrics + converted
//!   cost (RFC-008 8.2: 不能把本地成本记为零).
//! - [`metrics`] — CostPerSuccessfulQuery / Savings / ExternalAvoidanceRate.

pub mod compute;
pub mod estimator;
pub mod ledger;
pub mod metrics;
pub mod model;

pub use estimator::CostEstimator;
pub use ledger::{adjustment_event_for, filter_events, LedgerStats};
pub use metrics::{compute_cost_metrics, CostMetrics};
pub use model::{CostModelConfig, PricingModel, ProviderPricing, ResourceRates};
