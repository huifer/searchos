//! Query expansion (RFC-003 3.6).
//!
//! Query Expansion 不是默认开启 — it only triggers on a coverage gap.

use serde::{Deserialize, Serialize};

/// Expansion policy limits (RFC-003 3.6 initial config).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpansionConfig {
    pub enabled: bool,
    pub max_expansions: u32,
    pub max_external_queries: u32,
    /// Only expand when a coverage gap is proven.
    pub require_coverage_gap: bool,
}

impl Default for ExpansionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_expansions: 3,
            max_external_queries: 5,
            require_coverage_gap: true,
        }
    }
}

/// Rule-based query expander.
#[derive(Debug, Clone, Default)]
pub struct QueryExpander {
    pub config: ExpansionConfig,
}

impl QueryExpander {
    /// New expander with custom config.
    pub fn new(config: ExpansionConfig) -> Self {
        Self { config }
    }

    /// Whether expansion is permitted for this request.
    pub fn should_expand(&self, coverage_gap: bool, external_queries_so_far: u32) -> bool {
        if !self.config.enabled {
            return false;
        }
        if self.config.require_coverage_gap && !coverage_gap {
            return false;
        }
        external_queries_so_far < self.config.max_external_queries
    }

    /// Generates up to `max_expansions` variants of a base query for an
    /// uncovered sub-question (e.g. entity-focused reformulations).
    pub fn expand(&self, base: &str, gap_question: &str) -> Vec<String> {
        let mut out = Vec::new();
        if !self.should_expand(true, 0) {
            return out;
        }
        let gap = gap_question.trim();
        if !gap.is_empty() && !base.contains(gap) {
            out.push(format!("{base} {gap}"));
        }
        out.push(gap.to_string());
        // Entity-comparison reformulation for "A vs B" style gaps.
        for kw in ["documentation", "official", "release notes"] {
            let v = format!("{gap} {kw}");
            if !out.contains(&v) {
                out.push(v);
            }
        }
        out.truncate(self.config.max_expansions as usize);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_never_expands() {
        let e = QueryExpander::new(ExpansionConfig {
            enabled: false,
            ..Default::default()
        });
        assert!(e.expand("rust frameworks", "pricing").is_empty());
        assert!(!e.should_expand(true, 0));
    }

    #[test]
    fn no_gap_no_expansion_by_default() {
        let e = QueryExpander::default();
        assert!(!e.should_expand(false, 0));
        assert!(e.should_expand(true, 0));
    }

    #[test]
    fn respects_max_expansions() {
        let e = QueryExpander::default();
        let v = e.expand("compare tokio axum", "tokio pricing");
        assert_eq!(v.len(), 3);
    }
}
