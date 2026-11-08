//! Search decision domain types (RFC-004).

use crate::cost::Money;
use crate::ids::{DocumentId, TaskId, TraceId};
use crate::request::SearchIntent;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Why a decision was reached (RFC-004 4.3). Stable snake_case reason codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    LocalEvidencePassedQualityGate,
    LocalEvidenceSufficientForProfile,
    LocalEvidenceStale,
    LocalCoverageInsufficient,
    StrictFreshnessRequired,
    ExplicitSourceRequirement,
    ModeForcesExternal,
    ModeForcesLocal,
    BudgetExceeded,
    NoValidExecutionPath,
    CacheHit,
}

impl DecisionReason {
    /// Stable snake_case code stored in `search_decisions.reason_code`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::LocalEvidencePassedQualityGate => "local_evidence_passed_quality_gate",
            Self::LocalEvidenceSufficientForProfile => "local_evidence_sufficient_for_profile",
            Self::LocalEvidenceStale => "local_evidence_stale",
            Self::LocalCoverageInsufficient => "local_coverage_insufficient",
            Self::StrictFreshnessRequired => "strict_freshness_required",
            Self::ExplicitSourceRequirement => "explicit_source_requirement",
            Self::ModeForcesExternal => "mode_forces_external",
            Self::ModeForcesLocal => "mode_forces_local",
            Self::BudgetExceeded => "budget_exceeded",
            Self::NoValidExecutionPath => "no_valid_execution_path",
            Self::CacheHit => "cache_hit",
        }
    }
}

/// A missing sub-question / requirement discovered by gap analysis (RFC-005 5.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageGap {
    /// The uncovered sub-question.
    pub question: String,
    /// What kind of evidence is missing.
    pub missing_kind: String,
}

/// The decision the engine produced for one task (RFC-004 4.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchDecision {
    /// Serve entirely from local knowledge.
    LocalOnly { reason: DecisionReason },
    /// Local serve + refresh the listed stale documents.
    LocalWithRefresh {
        documents: Vec<DocumentId>,
        reason: DecisionReason,
    },
    /// Local serve + external search restricted to the missing gaps.
    Hybrid {
        missing_requirements: Vec<CoverageGap>,
        reason: DecisionReason,
    },
    /// Go straight to external search.
    ExternalOnly { reason: DecisionReason },
    /// Refuse and explain (budget / quality limits) (RFC-004 Rule 5).
    Reject { reason: DecisionReason },
}

impl SearchDecision {
    /// Short path name used in traces and API responses
    /// (`local`, `local_with_refresh`, `hybrid`, `external`, `reject`).
    pub fn path_name(&self) -> &'static str {
        match self {
            Self::LocalOnly { .. } => "local",
            Self::LocalWithRefresh { .. } => "local_with_refresh",
            Self::Hybrid { .. } => "hybrid",
            Self::ExternalOnly { .. } => "external",
            Self::Reject { .. } => "reject",
        }
    }

    /// Whether external execution is required by this decision.
    pub fn requires_external(&self) -> bool {
        matches!(self, Self::Hybrid { .. } | Self::ExternalOnly { .. })
    }

    /// The reason attached to any variant.
    pub fn reason(&self) -> &DecisionReason {
        match self {
            Self::LocalOnly { reason }
            | Self::LocalWithRefresh { reason, .. }
            | Self::Hybrid { reason, .. }
            | Self::ExternalOnly { reason }
            | Self::Reject { reason } => reason,
        }
    }
}

/// Explainable decision record (RFC-004 4.5, P-011).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionTrace {
    pub trace_id: TraceId,
    pub task_id: TaskId,
    pub intents: Vec<SearchIntent>,
    pub profile_version: String,
    pub local_candidates: usize,
    pub accepted_candidates: usize,
    pub freshness_status: String,
    pub coverage_status: String,
    pub source_status: String,
    pub selected_path: String,
    pub rejection_reasons: Vec<String>,
    pub estimated_cost: Money,
    pub actual_cost: Option<Money>,
    pub decision_at: DateTime<Utc>,
}
