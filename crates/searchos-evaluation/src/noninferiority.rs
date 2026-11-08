//! Non-inferiority gate (RFC-015 15.6).
//!
//! 不能简单认为 SearchOS 的结果和 External Only 一样 — cost reduction is only
//! valid when quality reaches the non-inferiority bound on the fixed test
//! set: fixed dataset, fixed time window, fixed provider config, fixed
//! quality standard; compare Evidence Recall (keyword coverage proxy),
//! Freshness, human-labeled quality (later), and full-chain cost.

use serde::{Deserialize, Serialize};

/// Non-inferiority configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NonInferiorityConfig {
    /// Minimum acceptable evidence-recall ratio vs baseline
    /// (searchos_coverage / baseline_coverage must be ≥ this).
    pub min_recall_ratio: f64,
    /// Maximum freshness violations allowed in the SearchOS run.
    pub max_freshness_violations: u64,
    /// Minimum quality pass rate for the SearchOS run.
    pub min_quality_pass_rate: f64,
}

impl Default for NonInferiorityConfig {
    fn default() -> Self {
        Self {
            min_recall_ratio: 0.95,      // 关键证据覆盖率 ≥ 95% (RFC-015 15.5)
            max_freshness_violations: 0, // 强时效数据错误复用 0 容忍
            min_quality_pass_rate: 0.6,
        }
    }
}

/// The verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NonInferiorityVerdict {
    pub quality_non_inferior: bool,
    pub cost_reduction_valid: bool,
    pub recall_ratio: Option<f64>,
    pub reasons: Vec<String>,
}

/// Evaluates the non-inferiority of a SearchOS run against the baseline.
pub fn check_noninferiority(
    config: &NonInferiorityConfig,
    searchos_coverage: f64,
    baseline_coverage: f64,
    searchos_quality_pass_rate: f64,
    searchos_freshness_violations: u64,
    searchos_cost: i64,
    baseline_cost: i64,
) -> NonInferiorityVerdict {
    let mut reasons = Vec::new();
    let mut ok = true;

    let recall_ratio = if baseline_coverage > 0.0 {
        Some(searchos_coverage / baseline_coverage)
    } else {
        None
    };
    if let Some(r) = recall_ratio {
        if r < config.min_recall_ratio {
            ok = false;
            reasons.push(format!(
                "evidence recall ratio {r:.3} < required {}",
                config.min_recall_ratio
            ));
        }
    } else if searchos_coverage < config.min_recall_ratio {
        ok = false;
        reasons.push(format!(
            "absolute coverage {searchos_coverage:.3} < required {} (no baseline coverage)",
            config.min_recall_ratio
        ));
    }

    if searchos_quality_pass_rate < config.min_quality_pass_rate {
        ok = false;
        reasons.push(format!(
            "quality pass rate {searchos_quality_pass_rate:.3} < minimum {}",
            config.min_quality_pass_rate
        ));
    }

    if searchos_freshness_violations > config.max_freshness_violations {
        ok = false;
        reasons.push(format!(
            "freshness violations {} exceed 0-tolerance bound",
            searchos_freshness_violations
        ));
    }

    let cost_down = baseline_cost > 0 && searchos_cost < baseline_cost;
    NonInferiorityVerdict {
        quality_non_inferior: ok,
        // 成本下降只有在质量非劣效时才算有效 (RFC-015 15.6).
        cost_reduction_valid: ok && cost_down,
        recall_ratio,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_when_quality_kept_and_cost_down() {
        let v = check_noninferiority(
            &NonInferiorityConfig::default(),
            0.96,
            1.0, // recall ratio 0.96 ≥ 0.95
            0.8, // pass rate ≥ 0.6
            0,   // no freshness violations
            150_000,
            700_000,
        );
        assert!(v.quality_non_inferior);
        assert!(v.cost_reduction_valid);
        assert_eq!(v.recall_ratio.unwrap(), 0.96);
        assert!(v.reasons.is_empty());
    }

    #[test]
    fn invalid_when_recall_below_bound() {
        let v = check_noninferiority(
            &NonInferiorityConfig::default(),
            0.90,
            1.0,
            0.8,
            0,
            100,
            700_000,
        );
        assert!(!v.quality_non_inferior);
        assert!(
            !v.cost_reduction_valid,
            "cost saving invalid without quality parity"
        );
        assert!(!v.reasons.is_empty());
    }

    #[test]
    fn freshness_zero_tolerance() {
        let v = check_noninferiority(
            &NonInferiorityConfig::default(),
            0.99,
            1.0,
            0.9,
            1,
            100,
            700_000,
        );
        assert!(!v.quality_non_inferior);
        assert!(v.reasons.iter().any(|r| r.contains("freshness")));
    }

    #[test]
    fn no_cost_reduction_means_not_valid_even_with_quality() {
        let v = check_noninferiority(
            &NonInferiorityConfig::default(),
            0.99,
            1.0,
            0.9,
            0,
            700_000,
            700_000,
        );
        assert!(v.quality_non_inferior);
        assert!(!v.cost_reduction_valid);
    }
}
