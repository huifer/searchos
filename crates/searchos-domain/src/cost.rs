//! Cost domain model: integer money, cost events, ledger queries (RFC-008).
//!
//! 金额必须使用整数最小货币单位，避免浮点金额计算误差 (RFC-008 8.2).

use crate::ids::{AgentId, CostEventId, TaskId, TraceId, WorkspaceId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Money in integer micro units (1 unit = 1e-6 of the currency).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct Money {
    pub amount_micros: i64,
    pub currency: String,
}

impl Money {
    /// Zero money in the given currency.
    pub fn zero(currency: impl Into<String>) -> Self {
        Self {
            amount_micros: 0,
            currency: currency.into(),
        }
    }

    /// Builds money from a decimal string such as `"0.005"` (USD), rounding
    /// to micros. Parses without binary floating point error.
    pub fn parse(decimal: &str, currency: impl Into<String>) -> Result<Self, String> {
        let neg = decimal.trim().starts_with('-');
        let s = decimal.trim().trim_start_matches('-');
        let (int_part, frac_part) = match s.split_once('.') {
            Some((i, f)) => (i, f),
            None => (s, ""),
        };
        if int_part.is_empty() && frac_part.is_empty() {
            return Err(format!("invalid money: {decimal}"));
        }
        if !int_part.chars().all(|c| c.is_ascii_digit())
            || !frac_part.chars().all(|c| c.is_ascii_digit())
        {
            return Err(format!("invalid money: {decimal}"));
        }
        // Pad/truncate fraction to 6 digits.
        let mut frac6 = frac_part.to_string();
        while frac6.len() < 6 {
            frac6.push('0');
        }
        frac6.truncate(6);
        let micros: i64 = format!("{int_part}{frac6}")
            .parse()
            .map_err(|e| format!("{e}"))?;
        Ok(Self {
            amount_micros: if neg { -micros } else { micros },
            currency: currency.into(),
        })
    }

    /// Formats as decimal major units, e.g. `0.005000`.
    pub fn to_decimal_string(&self) -> String {
        let sign = if self.amount_micros < 0 { "-" } else { "" };
        let abs = self.amount_micros.unsigned_abs();
        format!("{}{}.{:06}", sign, abs / 1_000_000, abs % 1_000_000)
    }

    /// Checked addition; errors on currency mismatch or overflow.
    pub fn checked_add(&self, other: &Money) -> Result<Money, String> {
        if self.currency != other.currency {
            return Err(format!(
                "currency mismatch: {} vs {}",
                self.currency, other.currency
            ));
        }
        self.amount_micros
            .checked_add(other.amount_micros)
            .map(|v| Money {
                amount_micros: v,
                currency: self.currency.clone(),
            })
            .ok_or_else(|| "money overflow".to_string())
    }

    /// Multiplies by a non-negative integer factor.
    pub fn checked_mul(&self, factor: u64) -> Result<Money, String> {
        self.amount_micros
            .checked_mul(factor as i64)
            .map(|v| Money {
                amount_micros: v,
                currency: self.currency.clone(),
            })
            .ok_or_else(|| "money overflow".to_string())
    }

    /// Saturating sum over a list (used by summaries).
    pub fn sum<'a>(items: impl IntoIterator<Item = &'a Money>) -> Option<Money> {
        let mut iter = items.into_iter();
        let first = iter.next()?.clone();
        let mut acc = first;
        for m in iter {
            acc = acc.checked_add(m).ok()?;
        }
        Some(acc)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.to_decimal_string(), self.currency)
    }
}

/// Operation categories charged to the ledger (RFC-008 8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostOperation {
    WebSearch,
    WebFetch,
    WebCrawl,
    Embedding,
    Reranking,
    QueryInference,
    LocalBm25,
    LocalVector,
    LocalCompute,
    StorageWrite,
    StorageRead,
    IndexMaintenance,
    NetworkTransfer,
    /// Auditable provider attempt that produced no billable result
    /// (Part 30 INV-30-06).
    ProviderAttempt,
}

impl CostOperation {
    /// Snake-case name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WebSearch => "web_search",
            Self::WebFetch => "web_fetch",
            Self::WebCrawl => "web_crawl",
            Self::Embedding => "embedding",
            Self::Reranking => "reranking",
            Self::QueryInference => "query_inference",
            Self::LocalBm25 => "local_bm25",
            Self::LocalVector => "local_vector",
            Self::LocalCompute => "local_compute",
            Self::StorageWrite => "storage_write",
            Self::StorageRead => "storage_read",
            Self::IndexMaintenance => "index_maintenance",
            Self::NetworkTransfer => "network_transfer",
            Self::ProviderAttempt => "provider_attempt",
        }
    }

    /// Parses from the snake-case name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "web_search" => Self::WebSearch,
            "web_fetch" => Self::WebFetch,
            "web_crawl" => Self::WebCrawl,
            "embedding" => Self::Embedding,
            "reranking" => Self::Reranking,
            "query_inference" => Self::QueryInference,
            "local_bm25" => Self::LocalBm25,
            "local_vector" => Self::LocalVector,
            "local_compute" => Self::LocalCompute,
            "storage_write" => Self::StorageWrite,
            "storage_read" => Self::StorageRead,
            "index_maintenance" => Self::IndexMaintenance,
            "network_transfer" => Self::NetworkTransfer,
            "provider_attempt" => Self::ProviderAttempt,
            _ => return None,
        })
    }
}

/// Outcome of a cost-bearing attempt (Part 30 §30.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CostAttemptStatus {
    #[default]
    Succeeded,
    Failed,
}

impl CostAttemptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "failed" => Self::Failed,
            _ => Self::Succeeded,
        }
    }
}

/// One append-only ledger entry (RFC-008 8.2, ADR-015).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEvent {
    pub id: CostEventId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub agent_id: Option<AgentId>,
    pub operation: CostOperation,
    pub provider_id: Option<String>,
    pub input_units: u64,
    pub output_units: u64,
    pub estimated_cost: Money,
    /// `None` until a real bill confirms the amount (RFC-012 12.6).
    pub actual_cost: Option<Money>,
    pub duration_ms: u64,
    pub occurred_at: DateTime<Utc>,
    pub trace_id: Option<TraceId>,
    /// Whether the event was shared via singleflight (RFC-009 9.8):
    /// shared benefit events do not double-bill external calls.
    pub shared: bool,
    /// Pricing model version used for the estimate (RFC-008 8.4).
    pub pricing_model_version: String,
    /// Whether this attempt succeeded; failed attempts have zero amount.
    #[serde(default)]
    pub status: CostAttemptStatus,
    /// Stable error code for a failed attempt, never a secret or raw body.
    #[serde(default)]
    pub error_code: Option<String>,
}

/// Filter for ledger summarization (RFC-008 8.7).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostQuery {
    pub workspace_id: Option<WorkspaceId>,
    pub agent_id: Option<AgentId>,
    pub task_id: Option<TaskId>,
    pub provider_id: Option<String>,
    pub operation: Option<CostOperation>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

/// Four separated reuse cost views (RFC-006 6.9).
///
/// A reused result is not a new external purchase; acquisition, reuse,
/// amortised and marginal costs must stay distinguishable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReuseCostViews {
    /// Cost of originally acquiring the knowledge.
    pub original_acquisition_micros: i64,
    /// Cost of executing the reuse (local compute, cache lookup).
    pub reuse_execution_micros: i64,
    /// Allocated amortised cost of the reused knowledge.
    pub amortized_micros: i64,
    /// Cost actually added by the reuse operation.
    pub marginal_micros: i64,
}

/// Savings / CPQPA report (RFC-006 6.10, Appendix C).
///
/// The baseline is an *equivalent-quality* external-only strategy; an
/// estimate is never presented as a confirmed invoice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavingsReport {
    pub currency: String,
    pub task_count: u64,
    pub baseline_micros: i64,
    pub searchos_micros: i64,
    pub savings_micros: i64,
    pub savings_rate: f64,
    pub quality_passing_tasks: u64,
    pub cost_per_quality_passing_answer_micros: Option<i64>,
    pub external_avoidance_rate: f64,
    pub valid_local_resolution_rate: f64,
    pub external_calls: u64,
    pub local_resolved_tasks: u64,
    pub reuse_views: ReuseCostViews,
    /// True when no provider billing data has been reconciled yet.
    pub is_estimate: bool,
}

/// Aggregated ledger summary (RFC-008 8.6).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostSummary {
    pub total_estimated: Money,
    pub total_actual: Option<Money>,
    pub event_count: u64,
    pub by_operation: Vec<(String, Money)>,
    pub by_provider: Vec<(String, Money)>,
    pub by_workspace: Vec<(String, Money)>,
    pub by_agent: Vec<(String, Money)>,
    pub quality_passing_queries: u64,
    /// TotalCost / QualityPassingQueries (RFC-008 8.6).
    pub cost_per_successful_query: Option<Money>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_parses_exactly() {
        let m = Money::parse("0.005", "USD").unwrap();
        assert_eq!(m.amount_micros, 5_000);
        assert_eq!(m.to_decimal_string(), "0.005000");

        let m2 = Money::parse("0.10", "USD").unwrap();
        assert_eq!(m2.amount_micros, 100_000);
    }

    #[test]
    fn money_checked_add_and_mismatch() {
        let a = Money::parse("0.005", "USD").unwrap();
        let b = Money::parse("0.01", "USD").unwrap();
        assert_eq!(a.checked_add(&b).unwrap().amount_micros, 15_000);
        let eur = Money::parse("0.01", "EUR").unwrap();
        assert!(a.checked_add(&eur).is_err());
    }

    #[test]
    fn operation_roundtrip() {
        for op in [
            CostOperation::WebSearch,
            CostOperation::LocalCompute,
            CostOperation::NetworkTransfer,
            CostOperation::ProviderAttempt,
        ] {
            assert_eq!(CostOperation::parse(op.as_str()), Some(op));
        }
    }
}
