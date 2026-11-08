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
    /// ExternalFirst keeps external evidence primary and local as supplement.
    ExternalFirstRequested,
    /// Unresolved contradictions in the merged evidence set.
    ConflictingEvidence,
    /// No healthy provider can satisfy the plan.
    ProviderUnavailable,
    /// Workspace policy forbids this request.
    WorkspacePolicyDenied,
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
            Self::ExternalFirstRequested => "external_first_requested",
            Self::ConflictingEvidence => "conflicting_evidence",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::WorkspacePolicyDenied => "workspace_policy_denied",
        }
    }
}

/// Coverage state of one information requirement (RFC-004 v1.1 5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    #[default]
    Missing,
    PartiallyCovered,
    Covered,
    Conflicting,
    NotApplicable,
}

/// How urgently a gap must be resolved (RFC-004 v1.1 6.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GapPriority {
    Critical,
    High,
    #[default]
    Normal,
    Low,
}

/// Minimal acquisition strategy for one gap (RFC-004 v1.1 10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GapResolutionStrategy {
    ReuseExistingEvidence,
    RefreshExistingSource,
    SearchOfficialSource,
    #[default]
    SearchIndependentSources,
    FetchExistingUrl,
    CrawlDomain,
    SearchAlternativeSources,
}

/// A missing sub-question / requirement discovered by gap analysis (RFC-005 5.6,
/// RFC-004 v1.1 6.7/10.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageGap {
    /// The uncovered sub-question.
    pub question: String,
    /// What kind of evidence is missing.
    pub missing_kind: String,
    /// Requirement coverage state (Missing / PartiallyCovered / Conflicting).
    #[serde(default)]
    pub status: RequirementStatus,
    /// Resolution priority used by the external planner.
    #[serde(default)]
    pub priority: GapPriority,
    /// Minimal acquisition strategy for this gap.
    #[serde(default)]
    pub strategy: GapResolutionStrategy,
    /// Stable requirement id (links an external query back to the gap).
    #[serde(default)]
    pub requirement_id: String,
}

impl CoverageGap {
    /// A newly discovered missing requirement with a stable id.
    pub fn new(question: impl Into<String>, missing_kind: impl Into<String>) -> Self {
        let question = question.into();
        Self {
            requirement_id: crate::new_id("need"),
            question,
            missing_kind: missing_kind.into(),
            status: RequirementStatus::Missing,
            priority: GapPriority::Normal,
            strategy: GapResolutionStrategy::SearchIndependentSources,
        }
    }

    /// Builder-style status upgrade used by gap analysis.
    pub fn with_status(mut self, status: RequirementStatus) -> Self {
        self.status = status;
        self
    }

    /// Builder-style priority.
    pub fn with_priority(mut self, priority: GapPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Builder-style acquisition strategy.
    pub fn with_strategy(mut self, strategy: GapResolutionStrategy) -> Self {
        self.strategy = strategy;
        self
    }
}

impl Default for CoverageGap {
    fn default() -> Self {
        Self::new("", "evidence")
    }
}

/// One structured, immutable stage of a decision trace (RFC-004 v1.1 11.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionStageTrace {
    pub stage: String,
    pub status: String,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub input_summary: String,
    pub output_summary: String,
    pub reasons: Vec<String>,
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
    /// Ordered stage trace (RFC-004 v1.1 11.1).
    #[serde(default)]
    pub stages: Vec<DecisionStageTrace>,
    /// Strategy selected by the decision engine (RFC-004 v1.1 8.2).
    #[serde(default)]
    pub final_strategy: Option<String>,
    /// Final outcome (success / partial / insufficient / budget_exceeded).
    #[serde(default)]
    pub final_outcome: Option<String>,
}
