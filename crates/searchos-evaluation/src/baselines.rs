//! Baselines A / B / C (RFC-015 15.4).
//!
//! Baselines use the same quality standard; the comparison environment must
//! be reproducible (fixed dataset/time/provider config).

use serde::{Deserialize, Serialize};

use crate::runner::RunOutcome;

/// Which system a result describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineKind {
    /// A. External Only — every query hits the external provider.
    ExternalOnly,
    /// B. Traditional Exact Cache + TTL.
    ExactCacheTtl,
    /// C. SearchOS (local retrieval + quality gate + cost optimizer).
    SearchOs,
}

/// One baseline's measured/estimated result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineResult {
    pub kind: BaselineKind,
    pub quality_pass_rate: f64,
    pub keyword_coverage_avg: f64,
    pub external_calls: u64,
    pub cost_micros_total: i64,
    pub freshness_violations: u64,
    /// Whether these numbers are estimates (external-only baseline costs are
    /// analytic estimates when not executed live).
    pub is_estimate: bool,
}

/// Full comparison report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineComparison {
    pub external_only: BaselineResult,
    pub exact_cache_ttl: BaselineResult,
    pub searchos: BaselineResult,
    /// Savings ratio: 1 − SearchOS / ExternalOnly (RFC-008 8.6).
    pub savings_ratio: Option<f64>,
    /// External avoidance: 1 − SearchOS calls / ExternalOnly calls.
    pub external_avoidance_rate: Option<f64>,
    /// Cost per successful query per baseline.
    pub cost_per_successful_query: Vec<(BaselineKind, Option<f64>)>,
}

impl BaselineResult {
    /// Analytic External-Only baseline: N queries × provider price × fetches.
    /// Equal-quality standard: it also runs the fetches SearchOS would.
    pub fn external_only_estimate(
        questions: usize,
        price_per_query_micros: i64,
        fetches_per_query: f64,
        fetch_price_micros: i64,
    ) -> Self {
        let external_calls = questions as u64;
        let cost = (questions as i64 * price_per_query_micros)
            + ((questions as f64 * fetches_per_query) as i64 * fetch_price_micros);
        Self {
            kind: BaselineKind::ExternalOnly,
            quality_pass_rate: 0.0,
            keyword_coverage_avg: 0.0,
            external_calls,
            cost_micros_total: cost,
            freshness_violations: 0,
            is_estimate: true,
        }
    }

    /// Analytic Exact-Cache+TTL baseline: repeats within TTL served from
    /// cache; unique queries still hit external + fetch.
    #[allow(unused_variables)]
    pub fn exact_cache_estimate(
        unique_queries: usize,
        total_queries: usize,
        price_per_query_micros: i64,
        fetches_per_query: f64,
        fetch_price_micros: i64,
    ) -> Self {
        let external_calls = unique_queries as u64;
        let cost = (unique_queries as i64 * price_per_query_micros)
            + ((unique_queries as f64 * fetches_per_query) as i64 * fetch_price_micros);
        Self {
            kind: BaselineKind::ExactCacheTtl,
            quality_pass_rate: 0.0,
            keyword_coverage_avg: 0.0,
            external_calls,
            cost_micros_total: cost,
            freshness_violations: 0,
            is_estimate: true,
        }
    }

    /// Builds the SearchOS result from a live run.
    pub fn from_run(outcome: &RunOutcome, freshness_violations: u64) -> Self {
        Self {
            kind: BaselineKind::SearchOs,
            quality_pass_rate: outcome.quality_pass_rate,
            keyword_coverage_avg: outcome.keyword_coverage_avg,
            external_calls: outcome.external_call_total,
            cost_micros_total: outcome.cost_micros_total,
            freshness_violations,
            is_estimate: false,
        }
    }
}

/// Assembles the comparison; quality metrics only meaningful for the live run.
pub fn compare(
    external_only: BaselineResult,
    exact_cache: BaselineResult,
    searchos: BaselineResult,
) -> BaselineComparison {
    let savings = if external_only.cost_micros_total > 0 {
        Some(1.0 - searchos.cost_micros_total as f64 / external_only.cost_micros_total as f64)
    } else {
        None
    };
    let avoidance = if external_only.external_calls > 0 {
        Some(1.0 - searchos.external_calls as f64 / external_only.external_calls as f64)
    } else {
        None
    };
    let cpsq = |b: &BaselineResult| {
        if b.quality_pass_rate > 0.0 {
            Some(b.cost_micros_total as f64 / (b.quality_pass_rate * 100.0))
        } else {
            None
        }
    };
    BaselineComparison {
        savings_ratio: savings,
        external_avoidance_rate: avoidance,
        cost_per_successful_query: vec![
            (BaselineKind::ExternalOnly, cpsq(&external_only)),
            (BaselineKind::ExactCacheTtl, cpsq(&exact_cache)),
            (BaselineKind::SearchOs, cpsq(&searchos)),
        ],
        external_only,
        exact_cache_ttl: exact_cache,
        searchos,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analytic_baselines_and_comparison() {
        // 100 questions, unique 60; price 5000 micros/query, 2 fetches @ 1000.
        let a = BaselineResult::external_only_estimate(100, 5_000, 2.0, 1_000);
        assert_eq!(a.external_calls, 100);
        assert_eq!(a.cost_micros_total, 100 * 5_000 + 200 * 1_000);

        let b = BaselineResult::exact_cache_estimate(60, 100, 5_000, 2.0, 1_000);
        assert_eq!(b.external_calls, 60);
        assert_eq!(b.cost_micros_total, 60 * 5_000 + 120 * 1_000);

        let c_run = crate::runner::RunOutcome {
            quality_pass_rate: 0.8,
            keyword_coverage_avg: 0.9,
            external_call_total: 25,
            cost_micros_total: 150_000,
            ..Default::default()
        };
        let c = BaselineResult::from_run(&c_run, 0);

        let cmp = compare(a, b, c);
        assert!((cmp.savings_ratio.unwrap() - (1.0 - 150_000.0 / 700_000.0)).abs() < 1e-9);
        assert!((cmp.external_avoidance_rate.unwrap() - 0.75).abs() < 1e-9);
        assert!(cmp.external_only.is_estimate && !cmp.searchos.is_estimate);
    }
}
