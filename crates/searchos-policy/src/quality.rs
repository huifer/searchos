//! Quality policy: hard constraints + scoring weights (RFC-005, P-001).
//!
//! 硬性约束不能被加权分数抵消 (RFC-005 5.4).

use serde::{Deserialize, Serialize};

/// Weighted quality model Q = w_r·R + w_f·F + w_c·C + w_s·S + w_p·P.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityWeights {
    pub relevance: f32,
    pub freshness: f32,
    pub coverage: f32,
    pub source: f32,
    pub provenance: f32,
}

impl Default for QualityWeights {
    fn default() -> Self {
        Self {
            relevance: 0.35,
            freshness: 0.20,
            coverage: 0.25,
            source: 0.15,
            provenance: 0.05,
        }
    }
}

impl QualityWeights {
    /// Weighted score from dimension scores in [0, 1].
    pub fn score(
        &self,
        relevance: f32,
        freshness: f32,
        coverage: f32,
        source: f32,
        provenance: f32,
    ) -> f32 {
        self.relevance * relevance.clamp(0.0, 1.0)
            + self.freshness * freshness.clamp(0.0, 1.0)
            + self.coverage * coverage.clamp(0.0, 1.0)
            + self.source * source.clamp(0.0, 1.0)
            + self.provenance * provenance.clamp(0.0, 1.0)
    }
}

/// Policy combining hard minimums (non-overridable by cost) and weights.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityPolicy {
    pub version: String,
    pub minimum_quality: f32,
    pub minimum_coverage: f32,
    pub require_provenance: bool,
    pub minimum_distinct_sources: usize,
    pub weights: QualityWeights,
}

impl Default for QualityPolicy {
    fn default() -> Self {
        Self {
            version: crate::POLICY_SUITE_VERSION.into(),
            minimum_quality: 0.6,
            minimum_coverage: 0.6,
            require_provenance: true,
            minimum_distinct_sources: 1,
            weights: QualityWeights::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_bounded() {
        let w = QualityWeights::default();
        let s = w.score(1.0, 1.0, 1.0, 1.0, 1.0);
        assert!((s - 1.0).abs() < 1e-5);
        let s2 = w.score(0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(s2, 0.0);
    }
}
