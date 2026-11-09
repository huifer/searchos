//! Append-only ledger logic with aggregation (RFC-008 8.7, ADR-015).
//!
//! Corrections are made by appending adjustment events; history is never
//! rewritten. This module holds the pure aggregation over ledger views;
//! persistence lives in `searchos-storage-sqlite` (CostRepository trait).

use searchos_domain::cost::{CostEvent, CostQuery, CostSummary, Money};
use std::collections::HashMap;

/// Aggregated statistics extracted from a set of ledger events.
#[derive(Debug, Clone, Default)]
pub struct LedgerStats {
    pub total_estimated_micros: i64,
    pub total_actual_micros: i64,
    pub event_count: u64,
    pub by_operation: HashMap<String, i64>,
    pub by_provider: HashMap<String, i64>,
    pub by_workspace: HashMap<String, i64>,
    pub by_agent: HashMap<String, i64>,
    /// Distinct task ids that passed the quality gate (flagged by caller).
    pub quality_passing_tasks: u64,
    /// Sum of costs attributed as shared/singleflight benefit (RFC-009 9.8).
    pub shared_saved_micros: i64,
    pub external_calls: u64,
}

impl LedgerStats {
    /// Aggregates a filtered event slice.
    pub fn aggregate<'a>(
        events: impl IntoIterator<Item = &'a CostEvent>,
        quality_passing_task_ids: &std::collections::HashSet<String>,
    ) -> Self {
        let mut s = Self::default();
        let mut passing = std::collections::HashSet::new();
        for e in events {
            s.event_count += 1;
            s.total_estimated_micros += e.estimated_cost.amount_micros;
            s.total_actual_micros += e.actual_cost.as_ref().map(|m| m.amount_micros).unwrap_or(0);
            *s.by_operation
                .entry(e.operation.as_str().into())
                .or_insert(0) += e.estimated_cost.amount_micros;
            *s.by_provider
                .entry(e.provider_id.clone().unwrap_or_else(|| "local".into()))
                .or_insert(0) += e.estimated_cost.amount_micros;
            *s.by_workspace
                .entry(e.workspace_id.to_string())
                .or_insert(0) += e.estimated_cost.amount_micros;
            if let Some(a) = &e.agent_id {
                *s.by_agent.entry(a.to_string()).or_insert(0) += e.estimated_cost.amount_micros;
            }
            if e.shared {
                s.shared_saved_micros += e.estimated_cost.amount_micros;
            }
            if matches!(
                e.operation,
                searchos_domain::cost::CostOperation::WebSearch
                    | searchos_domain::cost::CostOperation::WebFetch
                    | searchos_domain::cost::CostOperation::WebCrawl
            ) && !e.shared
            {
                s.external_calls += 1;
            }
            if quality_passing_task_ids.contains(e.task_id.as_str()) {
                passing.insert(e.task_id.to_string());
            }
        }
        s.quality_passing_tasks = passing.len() as u64;
        s
    }

    /// Builds a domain CostSummary from these stats.
    pub fn to_summary(&self, currency: &str) -> CostSummary {
        let sort = |m: &HashMap<String, i64>| {
            let mut v: Vec<(String, Money)> = m
                .iter()
                .map(|(k, micros)| {
                    (
                        k.clone(),
                        Money {
                            amount_micros: *micros,
                            currency: currency.into(),
                        },
                    )
                })
                .collect();
            v.sort_by_key(|(_, m)| std::cmp::Reverse(m.amount_micros));
            v
        };
        CostSummary {
            total_estimated: Money {
                amount_micros: self.total_estimated_micros,
                currency: currency.into(),
            },
            total_actual: if self.total_actual_micros > 0 {
                Some(Money {
                    amount_micros: self.total_actual_micros,
                    currency: currency.into(),
                })
            } else {
                None
            },
            event_count: self.event_count,
            by_operation: sort(&self.by_operation),
            by_provider: sort(&self.by_provider),
            by_workspace: sort(&self.by_workspace),
            by_agent: sort(&self.by_agent),
            quality_passing_queries: self.quality_passing_tasks,
            cost_per_successful_query: if self.quality_passing_tasks > 0 {
                Some(Money {
                    amount_micros: self.total_estimated_micros / self.quality_passing_tasks as i64,
                    currency: currency.into(),
                })
            } else {
                None
            },
        }
    }
}

/// Applies a filter (CostQuery) to an event slice — pure function reused by
/// the SQLite repository and the evaluation harness.
pub fn filter_events<'a>(events: &[&'a CostEvent], q: &CostQuery) -> Vec<&'a CostEvent> {
    events
        .iter()
        .copied()
        .filter(|e| {
            if let Some(ws) = &q.workspace_id {
                if &e.workspace_id != ws {
                    return false;
                }
            }
            if let Some(a) = &q.agent_id {
                if e.agent_id.as_ref() != Some(a) {
                    return false;
                }
            }
            if let Some(t) = &q.task_id {
                if &e.task_id != t {
                    return false;
                }
            }
            if let Some(p) = &q.provider_id {
                if e.provider_id.as_ref() != Some(p) {
                    return false;
                }
            }
            if let Some(op) = &q.operation {
                if &e.operation != op {
                    return false;
                }
            }
            if let Some(since) = q.since {
                if e.occurred_at < since {
                    return false;
                }
            }
            if let Some(until) = q.until {
                if e.occurred_at > until {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Appends an adjustment event (纠错通过补充调整事件完成) — constructs a
/// negative-amendment event for a prior event, without mutating history.
pub fn adjustment_event_for(original: &CostEvent, reason: &str) -> CostEvent {
    let mut adj = original.clone();
    adj.id = searchos_domain::ids::CostEventId::new(searchos_domain::new_id("cost"));
    adj.estimated_cost = Money {
        amount_micros: -original.estimated_cost.amount_micros,
        currency: original.estimated_cost.currency.clone(),
    };
    adj.actual_cost = original.actual_cost.as_ref().map(|m| Money {
        amount_micros: -m.amount_micros,
        currency: m.currency.clone(),
    });
    adj.provider_id = original.provider_id.clone();
    adj.occurred_at = chrono::Utc::now();
    // The reason rides on the pricing-model-version field as annotation.
    adj.pricing_model_version = format!("adjustment:{}", reason);
    adj
}

#[cfg(test)]
mod tests {
    use super::*;
    use searchos_domain::cost::{CostOperation, Money};
    use searchos_domain::ids::{CostEventId, TaskId, WorkspaceId};

    fn event(op: CostOperation, provider: Option<&str>, micros: i64, shared: bool) -> CostEvent {
        CostEvent {
            id: CostEventId::generate(),
            task_id: TaskId::new("task_1"),
            workspace_id: WorkspaceId::new("ws_1"),
            agent_id: Some(searchos_domain::ids::AgentId::new("agent_a")),
            operation: op,
            provider_id: provider.map(|s| s.into()),
            input_units: 1,
            output_units: 0,
            estimated_cost: Money {
                amount_micros: micros,
                currency: "USD".into(),
            },
            actual_cost: None,
            duration_ms: 100,
            occurred_at: chrono::Utc::now(),
            trace_id: None,
            shared,
            pricing_model_version: "1".into(),
            status: Default::default(),
            error_code: None,
        }
    }

    #[test]
    fn aggregates_by_dimension() {
        let events = [
            event(CostOperation::WebSearch, Some("tavily"), 5_000, false),
            event(CostOperation::LocalBm25, None, 10, false),
            event(CostOperation::WebSearch, Some("tavily"), 5_000, true), // shared
        ];
        let refs: Vec<&CostEvent> = events.iter().collect();
        let stats = LedgerStats::aggregate(refs, &Default::default());
        assert_eq!(stats.total_estimated_micros, 10_010);
        assert_eq!(stats.event_count, 3);
        assert_eq!(stats.by_operation["web_search"], 10_000);
        assert_eq!(stats.by_provider["tavily"], 10_000);
        assert_eq!(stats.shared_saved_micros, 5_000);
        // shared external call doesn't count as an executed external call
        assert_eq!(stats.external_calls, 1);
    }

    #[test]
    fn cost_per_successful_query() {
        let events = [event(
            CostOperation::WebSearch,
            Some("tavily"),
            5_000,
            false,
        )];
        let refs: Vec<&CostEvent> = events.iter().collect();
        let mut passing = std::collections::HashSet::new();
        passing.insert("task_1".to_string());
        let stats = LedgerStats::aggregate(refs, &passing);
        let summary = stats.to_summary("USD");
        assert_eq!(summary.quality_passing_queries, 1);
        assert_eq!(
            summary.cost_per_successful_query.unwrap().amount_micros,
            5_000
        );
    }

    #[test]
    fn adjustment_negates_without_rewriting() {
        let original = event(CostOperation::WebSearch, Some("tavily"), 5_000, false);
        let adj = adjustment_event_for(&original, "billed_twice");
        assert_eq!(adj.estimated_cost.amount_micros, -5_000);
        let total = original
            .estimated_cost
            .checked_add(&adj.estimated_cost)
            .unwrap();
        assert_eq!(total.amount_micros, 0);
        assert!(adj.pricing_model_version.contains("billed_twice"));
    }

    #[test]
    fn filter_by_workspace_and_operation() {
        let events = [
            event(CostOperation::WebSearch, Some("tavily"), 5_000, false),
            event(CostOperation::LocalBm25, None, 10, false),
        ];
        let refs: Vec<&CostEvent> = events.iter().collect();
        let q = CostQuery {
            operation: Some(CostOperation::WebSearch),
            ..Default::default()
        };
        let filtered = filter_events(&refs, &q);
        assert_eq!(filtered.len(), 1);
    }
}
