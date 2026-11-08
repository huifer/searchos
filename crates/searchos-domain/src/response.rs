//! Search response domain model (RFC-012 12.6).
//!
//! `actual: null` in cost means "no real bill data yet", never zero cost.

use crate::evidence::Evidence;
use crate::ids::TraceId;
use serde::{Deserialize, Serialize};

/// Quality gate outcome for a response (RFC-005 5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityStatus {
    Passed,
    Partial,
    Failed,
    Conflict,
    Unknown,
}

/// Freshness verification outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessStatus {
    Verified,
    Stale,
    Unknown,
}

/// Coverage verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Sufficient,
    Partial,
    Insufficient,
}

/// Quality section of a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityReport {
    pub status: QualityStatus,
    pub freshness: FreshnessStatus,
    pub coverage: CoverageStatus,
    /// Weighted score Q = w_r·R + w_f·F + w_c·C + w_s·S + w_p·P (RFC-005 5.4).
    pub score: f32,
    /// Human-readable list of failed hard constraints.
    pub failed_constraints: Vec<String>,
}

/// Decision section of a response (RFC-012 12.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionReport {
    /// Path name: local / local_with_refresh / hybrid / external / reject.
    pub path: String,
    /// Snake_case reason code.
    pub reason: String,
}

/// Cost section of a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostReport {
    pub estimated: f64,
    pub actual: Option<f64>,
    pub currency: String,
}

/// One result item in a search response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultItem {
    pub title: Option<String>,
    pub url: String,
    /// Content snippet (untrusted evidence content, RFC-014 14.2).
    pub content: String,
    pub score: f32,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub source: String,
}

/// Explicit outcome of a search task (RFC-002 2.8, RFC-004 v1.1 6.12).
///
/// A partial or insufficient result must never be reported as a completed,
/// quality-passing search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchStatus {
    Completed,
    CompletedWithWarnings,
    Partial,
    InsufficientEvidence,
    BudgetExhausted,
    Failed,
    Cancelled,
}

impl SearchStatus {
    /// Snake-case wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::CompletedWithWarnings => "completed_with_warnings",
            Self::Partial => "partial",
            Self::InsufficientEvidence => "insufficient_evidence",
            Self::BudgetExhausted => "budget_exhausted",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl Default for SearchStatus {
    /// Legacy cached payloads (pre-status) were passing responses.
    fn default() -> Self {
        Self::Completed
    }
}

/// Full search response returned by the application layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub decision: DecisionReport,
    pub results: Vec<SearchResultItem>,
    pub quality: QualityReport,
    pub cost: CostReport,
    pub provenance: Vec<Evidence>,
    pub trace_id: TraceId,
    /// Explicit task outcome (RFC-002 2.8). `#[serde(default)]` keeps old
    /// cached payloads deserializable.
    #[serde(default)]
    pub status: SearchStatus,
    /// Machine-readable warnings ("partial success" must be visible).
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Task id for cost / trace attribution.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Set when this response was replayed from the idempotency ledger.
    #[serde(default)]
    pub replayed: bool,
}

impl QualityStatus {
    /// Snake-case name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Conflict => "conflict",
            Self::Unknown => "unknown",
        }
    }
}
