//! Search response domain model (RFC-012 12.6).
//!
//! `actual: null` in cost means "no real bill data yet", never zero cost.

use crate::decision::CoverageGap;
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
    /// Query-matched passage with `<b>…</b>` markers, when available.
    #[serde(default)]
    pub highlight: Option<String>,
    /// RFC-058: stable entity identity when a REGISTERED alias matched this
    /// result's title / URL / content. `None` when no alias matched — the
    /// system never invents an entity from text alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
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
    /// Sub-questions the final quality gate found uncovered (RFC-005 5.6,
    /// RFC-004 v1.1 6.7). This is the agent-facing contract for "what is
    /// still missing": each entry carries the open question, its coverage
    /// state, priority, minimal acquisition strategy and a stable
    /// requirement id so a follow-up query can target it directly.
    #[serde(default)]
    pub coverage_gaps: Vec<CoverageGap>,
    /// Agent session this turn belongs to (when the caller supplied one).
    #[serde(default)]
    pub session_id: Option<String>,
    /// Open requirements a follow-up turn should target (same content as
    /// `coverage_gaps[].question`, flattened for convenience).
    #[serde(default)]
    pub focus_requirements: Vec<String>,
    /// Agent-facing context assembled under the caller's budget (RFC-067/068).
    /// Present only when the request carried a `context_budget`; the raw
    /// `results` and `provenance` remain the untruncated contract.
    #[serde(default)]
    pub context: Option<crate::context::ContextBlockSummary>,
    /// RFC-038 evidence graph: duplicate folds and syndication relations the
    /// evidence merge actually observed. `None` when the merged set has no
    /// relations (a single-source answer has nothing to graph).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_graph: Option<EvidenceGraphBlock>,
    /// RFC-071 v2: evidence this research workspace accumulated in EARLIER
    /// turns and carried into this one (validated; never part of `results`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_evidence: Option<WorkspaceEvidenceBlock>,
}

/// Evidence a research workspace carried into this turn (RFC-071 v2).
///
/// Only evidence NOT already present in `results` is listed, and local
/// documents are re-validated (deleted / re-versioned entries are dropped
/// and counted, never re-served as fresh knowledge).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceEvidenceBlock {
    pub items: Vec<WorkspaceEvidenceItem>,
    /// Carried entries whose local document no longer exists at the cached
    /// version (dropped, never served).
    pub invalidated: usize,
    /// Workspace entries considered (before filtering).
    pub considered: usize,
}

/// One carried workspace evidence entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceEvidenceItem {
    pub url: String,
    pub title: Option<String>,
    pub source: String,
    pub score: f32,
    /// Trace of the turn that originally served it.
    pub trace_id: String,
    pub recorded_at: String,
}

/// A relation between two pieces of evidence (RFC-038).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRelation {
    /// The `from` URL canonicalizes to the `to` URL (same resource, one
    /// document).
    DuplicateOf,
    /// The `from` URL carries the same normalized content as `to` (a
    /// syndicated / re-published copy — corroboration accounting, RFC-074).
    SameContent,
}

/// One directed edge of the evidence graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceGraphEdge {
    pub from: String,
    pub to: String,
    pub relation: EvidenceRelation,
}

/// The evidence graph block attached to a response (RFC-038).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EvidenceGraphBlock {
    /// Canonical URLs that appear as endpoints.
    pub nodes: Vec<String>,
    pub edges: Vec<EvidenceGraphEdge>,
}

impl CoverageStatus {
    /// Snake-case wire name; decision traces must carry this, not a count.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sufficient => "sufficient",
            Self::Partial => "partial",
            Self::Insufficient => "insufficient",
        }
    }
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
