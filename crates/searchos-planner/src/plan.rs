//! Execution plan construction (RFC-009 9.2).

use searchos_cost::CostEstimator;
use searchos_domain::cost::Money;
use searchos_domain::decision::SearchDecision;
use searchos_domain::ids::{PlanId, TaskId};
use searchos_domain::plan::{
    ExecutionDependency, ExecutionNode, ExecutionNodeKind, ExecutionPlan, PlanStatus,
};
use searchos_domain::request::SearchBudget;
use searchos_domain::SearchOsError;

/// Builds execution plans from decisions with cost estimates.
pub struct PlanBuilder<'a> {
    estimator: &'a CostEstimator,
}

impl<'a> PlanBuilder<'a> {
    /// Builder bound to a cost estimator.
    pub fn new(estimator: &'a CostEstimator) -> Self {
        Self { estimator }
    }

    /// Builds the DAG for a decision (RFC-009 9.2 node example).
    pub fn build(
        &self,
        task_id: TaskId,
        decision: &SearchDecision,
        budget: &SearchBudget,
        provider_id: Option<&str>,
    ) -> Result<ExecutionPlan, SearchOsError> {
        let currency = self.estimator.rates().currency.clone();
        let mut nodes: Vec<ExecutionNode> = Vec::new();
        let mut deps: Vec<ExecutionDependency> = Vec::new();
        let node = |id: &str,
                    kind: ExecutionNodeKind,
                    provider: Option<&str>,
                    est: Money,
                    dur: u64,
                    nodes: &mut Vec<ExecutionNode>| {
            nodes.push(ExecutionNode {
                id: id.into(),
                kind,
                provider_id: provider.map(|s| s.to_string()),
                estimated_cost: est,
                estimated_duration_ms: dur,
            });
        };

        let local_cpu = self.estimator.estimate_cpu_millis(50); // heuristic local cost
        node(
            "normalize",
            ExecutionNodeKind::QueryNormalize,
            None,
            local_cpu.clone(),
            5,
            &mut nodes,
        );
        node(
            "cache_lookup",
            ExecutionNodeKind::CacheLookup,
            None,
            Money::zero(&currency),
            1,
            &mut nodes,
        );
        node(
            "local_retrieval",
            ExecutionNodeKind::LocalRetrieval,
            None,
            local_cpu.clone(),
            30,
            &mut nodes,
        );
        node(
            "quality_eval",
            ExecutionNodeKind::QualityEvaluation,
            None,
            local_cpu.clone(),
            10,
            &mut nodes,
        );
        deps.push(ExecutionDependency {
            from: "normalize".into(),
            to: "cache_lookup".into(),
        });
        deps.push(ExecutionDependency {
            from: "cache_lookup".into(),
            to: "local_retrieval".into(),
        });
        deps.push(ExecutionDependency {
            from: "local_retrieval".into(),
            to: "quality_eval".into(),
        });

        let mut estimated = Money::zero(&currency);
        for n in &nodes {
            estimated = estimated
                .checked_add(&n.estimated_cost)
                .map_err(SearchOsError::Internal)?;
        }

        match decision {
            SearchDecision::LocalOnly { .. } => {
                node(
                    "return",
                    ExecutionNodeKind::Return,
                    None,
                    Money::zero(&currency),
                    1,
                    &mut nodes,
                );
                deps.push(ExecutionDependency {
                    from: "quality_eval".into(),
                    to: "return".into(),
                });
            }
            SearchDecision::LocalWithRefresh { .. } | SearchDecision::Hybrid { .. } => {
                node(
                    "external_search",
                    ExecutionNodeKind::ExternalSearch,
                    provider_id,
                    self.estimator
                        .estimate_request(provider_id.unwrap_or("tavily")),
                    800,
                    &mut nodes,
                );
                node(
                    "fetch_documents",
                    ExecutionNodeKind::FetchDocuments,
                    provider_id,
                    Money::zero(&currency),
                    600,
                    &mut nodes,
                );
                node(
                    "extract",
                    ExecutionNodeKind::Extract,
                    None,
                    local_cpu.clone(),
                    100,
                    &mut nodes,
                );
                node(
                    "persist",
                    ExecutionNodeKind::Persist,
                    None,
                    Money::zero(&currency),
                    20,
                    &mut nodes,
                );
                node(
                    "final_gate",
                    ExecutionNodeKind::QualityEvaluation,
                    None,
                    local_cpu.clone(),
                    10,
                    &mut nodes,
                );
                node(
                    "return",
                    ExecutionNodeKind::Return,
                    None,
                    Money::zero(&currency),
                    1,
                    &mut nodes,
                );
                deps.push(ExecutionDependency {
                    from: "quality_eval".into(),
                    to: "external_search".into(),
                });
                deps.push(ExecutionDependency {
                    from: "external_search".into(),
                    to: "fetch_documents".into(),
                });
                deps.push(ExecutionDependency {
                    from: "fetch_documents".into(),
                    to: "extract".into(),
                });
                deps.push(ExecutionDependency {
                    from: "extract".into(),
                    to: "persist".into(),
                });
                deps.push(ExecutionDependency {
                    from: "persist".into(),
                    to: "final_gate".into(),
                });
                deps.push(ExecutionDependency {
                    from: "final_gate".into(),
                    to: "return".into(),
                });
                estimated = estimated
                    .checked_add(
                        &self
                            .estimator
                            .estimate_request(provider_id.unwrap_or("tavily")),
                    )
                    .map_err(SearchOsError::Internal)?;
            }
            SearchDecision::ExternalOnly { .. } => {
                node(
                    "external_search",
                    ExecutionNodeKind::ExternalSearch,
                    provider_id,
                    self.estimator
                        .estimate_request(provider_id.unwrap_or("tavily")),
                    800,
                    &mut nodes,
                );
                node(
                    "fetch_documents",
                    ExecutionNodeKind::FetchDocuments,
                    provider_id,
                    Money::zero(&currency),
                    600,
                    &mut nodes,
                );
                node(
                    "extract",
                    ExecutionNodeKind::Extract,
                    None,
                    local_cpu.clone(),
                    100,
                    &mut nodes,
                );
                node(
                    "persist",
                    ExecutionNodeKind::Persist,
                    None,
                    Money::zero(&currency),
                    20,
                    &mut nodes,
                );
                node(
                    "final_gate",
                    ExecutionNodeKind::QualityEvaluation,
                    None,
                    local_cpu.clone(),
                    10,
                    &mut nodes,
                );
                node(
                    "return",
                    ExecutionNodeKind::Return,
                    None,
                    Money::zero(&currency),
                    1,
                    &mut nodes,
                );
                deps.push(ExecutionDependency {
                    from: "normalize".into(),
                    to: "external_search".into(),
                });
                deps.push(ExecutionDependency {
                    from: "external_search".into(),
                    to: "fetch_documents".into(),
                });
                deps.push(ExecutionDependency {
                    from: "fetch_documents".into(),
                    to: "extract".into(),
                });
                deps.push(ExecutionDependency {
                    from: "extract".into(),
                    to: "persist".into(),
                });
                deps.push(ExecutionDependency {
                    from: "persist".into(),
                    to: "final_gate".into(),
                });
                deps.push(ExecutionDependency {
                    from: "final_gate".into(),
                    to: "return".into(),
                });
                estimated = estimated
                    .checked_add(
                        &self
                            .estimator
                            .estimate_request(provider_id.unwrap_or("tavily")),
                    )
                    .map_err(SearchOsError::Internal)?;
            }
            SearchDecision::Reject { .. } => {
                node(
                    "reject",
                    ExecutionNodeKind::Reject,
                    None,
                    Money::zero(&currency),
                    1,
                    &mut nodes,
                );
                deps.push(ExecutionDependency {
                    from: "quality_eval".into(),
                    to: "reject".into(),
                });
            }
        }

        let estimated_duration_ms: u64 = nodes.iter().map(|n| n.estimated_duration_ms).sum();
        let status = match decision {
            SearchDecision::Reject { .. } => PlanStatus::Rejected,
            _ => PlanStatus::Proposed,
        };

        Ok(ExecutionPlan {
            id: PlanId::generate(),
            task_id,
            nodes,
            dependencies: deps,
            estimated_cost: estimated,
            estimated_duration_ms,
            budget: budget.clone(),
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use searchos_cost::model::CostModelConfig;
    use searchos_cost::CostEstimator;

    fn builder() -> PlanBuilder<'static> {
        // Leak a static estimator for the test lifetime.
        let est: &'static CostEstimator = Box::leak(Box::new(CostEstimator::from_config(
            &CostModelConfig::default_config(),
        )));
        PlanBuilder::new(est)
    }

    #[test]
    fn local_plan_has_no_external_nodes() {
        let b = builder();
        let plan = b
            .build(
                TaskId::generate(),
                &SearchDecision::LocalOnly {
                    reason:
                        searchos_domain::decision::DecisionReason::LocalEvidencePassedQualityGate,
                },
                &SearchBudget::default(),
                None,
            )
            .unwrap();
        assert!(plan
            .nodes
            .iter()
            .all(|n| n.kind != ExecutionNodeKind::ExternalSearch));
        assert!(plan.topological_order().is_ok());
        assert!(
            plan.estimated_cost.amount_micros > 0,
            "local cost never zero (RFC-008)"
        );
    }

    #[test]
    fn hybrid_plan_includes_external_chain() {
        let b = builder();
        let plan = b
            .build(
                TaskId::generate(),
                &SearchDecision::Hybrid {
                    missing_requirements: vec![searchos_domain::decision::CoverageGap::new(
                        "q", "evidence",
                    )],
                    reason: searchos_domain::decision::DecisionReason::LocalCoverageInsufficient,
                },
                &SearchBudget::default(),
                Some("tavily"),
            )
            .unwrap();
        assert!(plan
            .nodes
            .iter()
            .any(|n| n.kind == ExecutionNodeKind::ExternalSearch
                && n.provider_id.as_deref() == Some("tavily")));
        let order = plan.topological_order().unwrap();
        let pos = |s: &str| order.iter().position(|x| x == s).unwrap();
        assert!(pos("normalize") < pos("external_search"));
        assert!(pos("persist") < pos("return"));
        // Includes external request price.
        assert!(plan.estimated_cost.amount_micros >= 5_000);
    }

    #[test]
    fn reject_plan_status() {
        let b = builder();
        let plan = b
            .build(
                TaskId::generate(),
                &SearchDecision::Reject {
                    reason: searchos_domain::decision::DecisionReason::BudgetExceeded,
                },
                &SearchBudget::default(),
                None,
            )
            .unwrap();
        assert_eq!(plan.status, PlanStatus::Rejected);
    }
}
