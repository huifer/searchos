//! Cost metrics (RFC-008 8.6).
//!
//! Baselines use the same quality standard; estimated vs real-bill figures
//! are never conflated.

use serde::{Deserialize, Serialize};

/// Core cost metrics (RFC-008 8.6 formulas).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostMetrics {
    /// TotalCost / QualityPassingQueries, in micros (None when no passing query).
    pub cost_per_successful_query_micros: Option<i64>,
    /// Savings = 1 − SearchOSCost / ExternalOnlyBaselineCost.
    pub savings_ratio: Option<f64>,
    /// ExternalAvoidanceRate = 1 − ExternalCalls_SearchOS / ExternalCalls_Baseline.
    pub external_avoidance_rate: Option<f64>,
    /// Baseline cost for comparison (micros).
    pub baseline_cost_micros: i64,
    /// SearchOS cost (micros).
    pub searchos_cost_micros: i64,
    /// External calls in the SearchOS run.
    pub searchos_external_calls: u64,
    /// External calls an External-Only baseline would have made.
    pub baseline_external_calls: u64,
    /// Singleflight shared savings (micros, RFC-009 9.8).
    pub shared_savings_micros: i64,
    /// All numbers in this report are *estimates*, never real bills.
    pub is_estimate: bool,
}

/// Inputs from the ledger/eval harness.
#[derive(Debug, Clone, Copy, Default)]
pub struct CostMetricInputs {
    pub searchos_cost_micros: i64,
    pub baseline_cost_micros: i64,
    pub searchos_external_calls: u64,
    pub baseline_external_calls: u64,
    pub quality_passing_queries: u64,
    pub shared_savings_micros: i64,
}

/// Computes the RFC-008 8.6 metric set.
pub fn compute_cost_metrics(inputs: CostMetricInputs) -> CostMetrics {
    let cost_per_successful = if inputs.quality_passing_queries > 0 {
        Some(inputs.searchos_cost_micros / inputs.quality_passing_queries as i64)
    } else {
        None
    };
    let savings = if inputs.baseline_cost_micros > 0 {
        Some(1.0 - inputs.searchos_cost_micros as f64 / inputs.baseline_cost_micros as f64)
    } else {
        None
    };
    let avoidance = if inputs.baseline_external_calls > 0 {
        Some(1.0 - inputs.searchos_external_calls as f64 / inputs.baseline_external_calls as f64)
    } else {
        None
    };
    CostMetrics {
        cost_per_successful_query_micros: cost_per_successful,
        savings_ratio: savings,
        external_avoidance_rate: avoidance,
        baseline_cost_micros: inputs.baseline_cost_micros,
        searchos_cost_micros: inputs.searchos_cost_micros,
        searchos_external_calls: inputs.searchos_external_calls,
        baseline_external_calls: inputs.baseline_external_calls,
        shared_savings_micros: inputs.shared_savings_micros,
        is_estimate: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn savings_and_avoidance_computed() {
        let m = compute_cost_metrics(CostMetricInputs {
            searchos_cost_micros: 5_200,
            baseline_cost_micros: 50_000,
            searchos_external_calls: 10,
            baseline_external_calls: 100,
            quality_passing_queries: 10,
            shared_savings_micros: 3_000,
        });
        assert!((m.savings_ratio.unwrap() - 0.896).abs() < 1e-9);
        assert!((m.external_avoidance_rate.unwrap() - 0.9).abs() < 1e-9);
        assert_eq!(m.cost_per_successful_query_micros, Some(520));
        assert!(m.is_estimate);
    }

    #[test]
    fn zero_baseline_yields_none_not_nan() {
        let m = compute_cost_metrics(CostMetricInputs::default());
        assert!(m.savings_ratio.is_none());
        assert!(m.external_avoidance_rate.is_none());
        assert!(m.cost_per_successful_query_micros.is_none());
    }
}
