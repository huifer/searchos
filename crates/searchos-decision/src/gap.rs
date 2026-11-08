//! Requirement extraction, evidence-gap analysis and minimal external search
//! planning (RFC-004 v1.1 §5.3, §6.7, §6.8, §10; RFC-005 5.4).
//!
//! Gap-Based Search is the primary cost optimisation mechanism: only missing
//! or stale information requirements are searched externally. A covered
//! requirement is never re-purchased.

use searchos_domain::decision::{GapPriority, GapResolutionStrategy, RequirementStatus};
use serde::{Deserialize, Serialize};

/// One independent information requirement of a complex query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InformationNeed {
    /// Stable id (`need_<uuid>`), referenced by external query tasks.
    pub id: String,
    /// Human-readable sub-question / dimension.
    pub description: String,
    /// Coverage state; `Conflicting` can never be converted to `Covered`.
    pub status: RequirementStatus,
    pub priority: GapPriority,
    /// Minimal acquisition strategy when this need is uncovered.
    pub strategy: GapResolutionStrategy,
}

impl InformationNeed {
    /// A new, uncovered requirement.
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            id: searchos_domain::new_id("need"),
            description: description.into(),
            status: RequirementStatus::Missing,
            priority: GapPriority::Normal,
            strategy: GapResolutionStrategy::SearchIndependentSources,
        }
    }

    /// Whether external acquisition is required for this need.
    pub fn is_open(&self) -> bool {
        matches!(
            self.status,
            RequirementStatus::Missing
                | RequirementStatus::PartiallyCovered
                | RequirementStatus::Conflicting
        )
    }
}

/// One targeted external query, always linked back to its requirement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalQueryTask {
    pub task_id: String,
    pub requirement_id: String,
    pub query: String,
    pub domains: Vec<String>,
    pub excluded_domains: Vec<String>,
    pub priority: GapPriority,
    pub strategy: GapResolutionStrategy,
}

/// Minimal external acquisition plan (RFC-004 v1.1 6.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalSearchPlan {
    pub plan_id: String,
    pub request_id: String,
    pub queries: Vec<ExternalQueryTask>,
    pub estimated_cost_micros: i64,
    pub estimated_latency_ms: u64,
    pub required_capabilities: Vec<String>,
    pub execution_mode: String,
}

/// Deterministic, rule-based gap analysis. No LLM is required.
pub struct GapAnalyzer;

impl GapAnalyzer {
    /// Evaluates each required sub-question against accepted local evidence
    /// text. `covered_texts` is one string per accepted evidence item.
    pub fn analyze(
        required_subquestions: &[String],
        covered_texts: &[String],
        required_domains: &[String],
        strict_freshness: bool,
        stale_sources_exist: bool,
    ) -> Vec<InformationNeed> {
        let mut needs = Vec::new();
        for sq in required_subquestions {
            let tokens = crate::quality::content_tokens(sq);
            let mut best_ratio = 0.0f32;
            for text in covered_texts {
                let lower = text.to_lowercase();
                let hits = tokens.iter().filter(|t| lower.contains(t.as_str())).count();
                let ratio = if tokens.is_empty() {
                    1.0
                } else {
                    hits as f32 / tokens.len() as f32
                };
                if ratio > best_ratio {
                    best_ratio = ratio;
                }
            }
            let mut need = InformationNeed::new(sq.clone());
            need.status = if best_ratio >= 0.5 {
                RequirementStatus::Covered
            } else if best_ratio > 0.0 {
                RequirementStatus::PartiallyCovered
            } else {
                RequirementStatus::Missing
            };
            need.priority = if strict_freshness || !required_domains.is_empty() {
                GapPriority::Critical
            } else if need.status == RequirementStatus::PartiallyCovered {
                GapPriority::High
            } else {
                GapPriority::Normal
            };
            need.strategy = if stale_sources_exist {
                GapResolutionStrategy::RefreshExistingSource
            } else if !required_domains.is_empty() {
                GapResolutionStrategy::SearchOfficialSource
            } else if need.status == RequirementStatus::PartiallyCovered {
                GapResolutionStrategy::FetchExistingUrl
            } else {
                GapResolutionStrategy::SearchIndependentSources
            };
            needs.push(need);
        }
        needs
    }
}

/// Builds the smallest plan that can close the open requirements.
///
/// Each query is linked to one `requirement_id`; covered requirements are
/// never re-searched. `max_queries` and `max_cost_micros` are hard caps.
pub fn build_external_plan(
    needs: &[InformationNeed],
    request_id: &str,
    max_queries: u32,
    max_cost_micros: i64,
    cost_per_query_micros: i64,
    domains: &[String],
    excluded_domains: &[String],
) -> ExternalSearchPlan {
    let mut open: Vec<&InformationNeed> = needs.iter().filter(|n| n.is_open()).collect();
    // Critical gaps first; stable (original) order otherwise.
    open.sort_by_key(|n| match n.priority {
        GapPriority::Critical => 0,
        GapPriority::High => 1,
        GapPriority::Normal => 2,
        GapPriority::Low => 3,
    });
    let mut queries = Vec::new();
    let mut cost = 0i64;
    for need in open {
        if queries.len() >= max_queries as usize {
            break;
        }
        if cost + cost_per_query_micros > max_cost_micros {
            break;
        }
        let query = match need.strategy {
            GapResolutionStrategy::RefreshExistingSource => {
                format!("{} current latest", need.description)
            }
            GapResolutionStrategy::SearchOfficialSource => {
                format!("{} official documentation", need.description)
            }
            GapResolutionStrategy::FetchExistingUrl => need.description.clone(),
            _ => need.description.clone(),
        };
        queries.push(ExternalQueryTask {
            task_id: searchos_domain::new_id("qt"),
            requirement_id: need.id.clone(),
            query,
            domains: domains.to_vec(),
            excluded_domains: excluded_domains.to_vec(),
            priority: need.priority,
            strategy: need.strategy,
        });
        cost += cost_per_query_micros;
    }
    ExternalSearchPlan {
        plan_id: searchos_domain::new_id("plan"),
        request_id: request_id.into(),
        estimated_cost_micros: cost,
        estimated_latency_ms: 400 * queries.len() as u64,
        required_capabilities: vec!["web_search".into()],
        execution_mode: if queries.len() > 1 {
            "parallel".into()
        } else {
            "sequential".into()
        },
        queries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covered_requirements_are_not_re_searched() {
        let needs = GapAnalyzer::analyze(
            &["tokio async runtime".into(), "axum web framework".into()],
            &["Tokio is an asynchronous runtime for Rust".into()],
            &[],
            false,
            false,
        );
        assert_eq!(needs[0].status, RequirementStatus::Covered);
        assert_eq!(needs[1].status, RequirementStatus::Missing);
        let plan = build_external_plan(&needs, "req_1", 5, 100_000, 5_000, &[], &[]);
        assert_eq!(plan.queries.len(), 1);
        assert!(plan.queries[0].query.contains("axum"));
        assert_eq!(plan.queries[0].requirement_id, needs[1].id);
        assert_eq!(plan.estimated_cost_micros, 5_000);
    }

    #[test]
    fn stale_sources_use_refresh_strategy() {
        let needs = GapAnalyzer::analyze(&["tavily pricing".into()], &[], &[], true, true);
        assert_eq!(needs[0].priority, GapPriority::Critical);
        assert_eq!(
            needs[0].strategy,
            GapResolutionStrategy::RefreshExistingSource
        );
        let plan = build_external_plan(&needs, "req_2", 5, 100_000, 5_000, &[], &[]);
        assert!(plan.queries[0].query.contains("current latest"));
    }

    #[test]
    fn query_and_cost_caps_are_hard() {
        let needs = GapAnalyzer::analyze(
            &["a".into(), "b".into(), "c".into(), "d".into()],
            &[],
            &[],
            false,
            false,
        );
        let plan = build_external_plan(&needs, "req_3", 2, 6_000, 5_000, &[], &[]);
        assert_eq!(plan.queries.len(), 1, "cost cap binds before query cap");
        assert_eq!(plan.estimated_cost_micros, 5_000);
    }

    #[test]
    fn official_source_requirement_is_propagated() {
        let needs = GapAnalyzer::analyze(
            &["rust release notes".into()],
            &[],
            &["rust-lang.org".into()],
            false,
            false,
        );
        let plan = build_external_plan(
            &needs,
            "req_4",
            3,
            100_000,
            5_000,
            &["rust-lang.org".into()],
            &[],
        );
        assert_eq!(plan.queries[0].domains, vec!["rust-lang.org".to_string()]);
        assert_eq!(plan.queries[0].priority, GapPriority::Critical);
    }
}
