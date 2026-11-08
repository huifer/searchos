//! Budget enforcement with concurrent reservations (RFC-009 9.6).
//!
//! 预算限制必须在实际执行前检查，并对并发任务预留预算.

use searchos_domain::cost::Money;
use searchos_domain::request::SearchBudget;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Outcome of a pre-execution budget check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BudgetCheck {
    Allowed,
    Exceeded { reason: String },
}

impl BudgetCheck {
    /// `true` when allowed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// Violation reason when exceeded.
    pub fn violation(&self) -> Option<&str> {
        match self {
            Self::Allowed => None,
            Self::Exceeded { reason } => Some(reason),
        }
    }
}

/// Violation detail for accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetViolation {
    ExternalQueries,
    FetchPages,
    TotalCost,
    ExecutionTime,
    Retries,
    QueryExpansions,
}

impl BudgetViolation {
    /// Stable reason string for decision traces.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ExternalQueries => "max_external_queries",
            Self::FetchPages => "max_fetch_pages",
            Self::TotalCost => "max_total_cost",
            Self::ExecutionTime => "max_execution_time_ms",
            Self::Retries => "max_retries",
            Self::QueryExpansions => "max_query_expansions",
        }
    }
}

/// Enforces budget bounds and tracks concurrent reservations so multiple
/// parallel tasks cannot jointly overrun the limit (RFC-009 9.6).
#[derive(Debug)]
pub struct BudgetEnforcer {
    /// Remaining reserved micros across all live tasks.
    reserved_micros: Arc<AtomicI64>,
    /// Global ceiling (None = only per-task budgets apply).
    global_max_micros: Option<i64>,
    currency: String,
}

impl BudgetEnforcer {
    /// New enforcer with optional workspace-level global ceiling in micros.
    pub fn new(global_max_micros: Option<i64>, currency: impl Into<String>) -> Self {
        Self {
            reserved_micros: Arc::new(AtomicI64::new(0)),
            global_max_micros,
            currency: currency.into(),
        }
    }

    /// Checks a task budget against a plan estimate.
    pub fn check(&self, budget: &SearchBudget, estimated_micros: i64) -> BudgetCheck {
        if estimated_micros > budget.max_total_cost_micros {
            return BudgetCheck::Exceeded {
                reason: format!(
                    "estimated {} micros exceeds task budget {} micros",
                    estimated_micros, budget.max_total_cost_micros
                ),
            };
        }
        if let Some(global) = self.global_max_micros {
            let reserved = self.reserved_micros.load(Ordering::SeqCst);
            if reserved + estimated_micros > global {
                return BudgetCheck::Exceeded {
                    reason: format!(
                        "reserved {reserved} + estimated {estimated_micros} micros exceeds global budget {global} micros"
                    ),
                };
            }
        }
        BudgetCheck::Allowed
    }

    /// Reserves budget for an executing task; returns a guard that releases
    /// the reservation on drop.
    pub fn reserve(&self, micros: i64) -> BudgetReservation {
        self.reserved_micros.fetch_add(micros, Ordering::SeqCst);
        BudgetReservation {
            micros,
            counter: Arc::clone(&self.reserved_micros),
        }
    }

    /// Current reserved amount.
    pub fn reserved_micros(&self) -> i64 {
        self.reserved_micros.load(Ordering::SeqCst)
    }

    /// Currency of tracked amounts.
    pub fn currency(&self) -> &str {
        &self.currency
    }

    /// Convenience: check a Money estimate (currency-checked).
    pub fn check_money(
        &self,
        budget: &SearchBudget,
        estimate: &Money,
    ) -> Result<BudgetCheck, String> {
        if estimate.currency != budget.currency {
            return Err(format!(
                "estimate currency {} != budget currency {}",
                estimate.currency, budget.currency
            ));
        }
        Ok(self.check(budget, estimate.amount_micros))
    }
}

/// RAII reservation released on drop.
#[derive(Debug)]
pub struct BudgetReservation {
    micros: i64,
    counter: Arc<AtomicI64>,
}

impl Drop for BudgetReservation {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.micros, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_over_task_budget() {
        let e = BudgetEnforcer::new(None, "USD");
        let b = SearchBudget::default(); // 0.10 USD
        let est = Money::parse("0.50", "USD").unwrap();
        match e.check_money(&b, &est).unwrap() {
            BudgetCheck::Exceeded { .. } => {}
            BudgetCheck::Allowed => panic!("should exceed"),
        }
    }

    #[test]
    fn reservation_blocks_parallel_tasks_and_releases() {
        let e = BudgetEnforcer::new(Some(100_000), "USD"); // global 0.10 USD
        let b = SearchBudget::default();
        let est = Money::parse("0.06", "USD").unwrap();
        assert!(e.check_money(&b, &est).unwrap().is_allowed());
        {
            let _g = e.reserve(est.amount_micros);
            assert_eq!(e.reserved_micros(), 60_000);
            // Second concurrent task would push past the global ceiling.
            assert!(!e.check_money(&b, &est).unwrap().is_allowed());
        }
        assert_eq!(e.reserved_micros(), 0);
    }
}
