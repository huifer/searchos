//! Execution plan domain model (RFC-009 9.2).

use crate::cost::Money;
use crate::ids::{PlanId, TaskId};
use serde::{Deserialize, Serialize};

/// Kinds of nodes in an execution DAG (RFC-009 9.2 example).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionNodeKind {
    QueryNormalize,
    CacheLookup,
    LocalRetrieval,
    QualityEvaluation,
    ExternalSearch,
    FetchDocuments,
    Extract,
    Persist,
    Return,
    Reject,
}

/// One node of the execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionNode {
    pub id: String,
    pub kind: ExecutionNodeKind,
    /// Optional provider this node is bound to.
    pub provider_id: Option<String>,
    /// Estimated cost contribution of this node.
    pub estimated_cost: Money,
    /// Estimated duration of this node in milliseconds.
    pub estimated_duration_ms: u64,
}

/// Edge between two nodes: `from` must complete before `to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionDependency {
    pub from: String,
    pub to: String,
}

/// Status of a plan over its lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Proposed,
    Approved,
    Executing,
    Completed,
    Failed,
    Rejected,
}

/// A costed, dependency-ordered execution plan for one task (RFC-009 9.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub id: PlanId,
    pub task_id: TaskId,
    pub nodes: Vec<ExecutionNode>,
    pub dependencies: Vec<ExecutionDependency>,
    pub estimated_cost: Money,
    pub estimated_duration_ms: u64,
    pub budget: crate::request::SearchBudget,
    pub status: PlanStatus,
}

impl ExecutionPlan {
    /// Topologically ordered node ids (Kahn); errors on cycles.
    pub fn topological_order(&self) -> Result<Vec<String>, String> {
        let mut indegree: std::collections::HashMap<&str, usize> = Default::default();
        let mut adj: std::collections::HashMap<&str, Vec<&str>> = Default::default();
        for n in &self.nodes {
            indegree.entry(n.id.as_str()).or_insert(0);
        }
        for d in &self.dependencies {
            adj.entry(d.from.as_str()).or_default().push(d.to.as_str());
            *indegree.entry(d.to.as_str()).or_insert(0) += 1;
        }
        let mut queue: std::collections::VecDeque<&str> = indegree
            .iter()
            .filter(|(_, v)| **v == 0)
            .map(|(k, _)| *k)
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(n) = queue.pop_front() {
            order.push(n.to_string());
            for next in adj.get(n).into_iter().flatten() {
                let e = indegree.get_mut(next).unwrap();
                *e -= 1;
                if *e == 0 {
                    queue.push_back(next);
                }
            }
        }
        if order.len() != self.nodes.len() {
            return Err("execution plan contains a cycle".into());
        }
        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> ExecutionPlan {
        ExecutionPlan {
            id: PlanId::generate(),
            task_id: TaskId::generate(),
            nodes: vec![
                ExecutionNode {
                    id: "a".into(),
                    kind: ExecutionNodeKind::QueryNormalize,
                    provider_id: None,
                    estimated_cost: Money::zero("USD"),
                    estimated_duration_ms: 1,
                },
                ExecutionNode {
                    id: "b".into(),
                    kind: ExecutionNodeKind::ExternalSearch,
                    provider_id: Some("tavily".into()),
                    estimated_cost: Money::parse("0.005", "USD").unwrap(),
                    estimated_duration_ms: 800,
                },
                ExecutionNode {
                    id: "c".into(),
                    kind: ExecutionNodeKind::Return,
                    provider_id: None,
                    estimated_cost: Money::zero("USD"),
                    estimated_duration_ms: 1,
                },
            ],
            dependencies: vec![
                ExecutionDependency {
                    from: "a".into(),
                    to: "b".into(),
                },
                ExecutionDependency {
                    from: "b".into(),
                    to: "c".into(),
                },
            ],
            estimated_cost: Money::parse("0.005", "USD").unwrap(),
            estimated_duration_ms: 802,
            budget: Default::default(),
            status: PlanStatus::Proposed,
        }
    }

    #[test]
    fn topological_order_respects_dependencies() {
        let p = plan();
        let order = p.topological_order().unwrap();
        let pos = |s: &str| order.iter().position(|x| x == s).unwrap();
        assert!(pos("a") < pos("b") && pos("b") < pos("c"));
    }

    #[test]
    fn cycle_detected() {
        let mut p = plan();
        p.dependencies.push(ExecutionDependency {
            from: "c".into(),
            to: "a".into(),
        });
        assert!(p.topological_order().is_err());
    }
}
