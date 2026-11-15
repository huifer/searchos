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
    /// RFC-076: the external plan was downsized to fit the remaining
    /// budget (cost-adaptive path selection).
    ExternalPlanDownsized,
}

impl DecisionReason {
    /// Stable snake_case code stored in `search_decisions.reason_code`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::LocalEvidencePassedQualityGate => "local_evidence_passed_quality_gate",
            Self::LocalEvidenceSufficientForProfile => "local_evidence_sufficient_for_profile",
            Self::LocalEvidenceStale => "local_evidence_stale",
            Self::LocalCoverageInsufficient => "local_coverage_insufficient",
            Self::ExternalPlanDownsized => "external_plan_downsized",
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

/// RFC-076: cost-adaptive reasoning recorded on the trace. The engine
/// compares the per-query price against the remaining budget and, when the
/// budget only affords FEWER external queries than requested, reports the
/// downsize — the plan must fit what the caller actually paid for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CostAdaptiveTrace {
    /// External queries the caller's budget config requested.
    pub requested_external_queries: u32,
    /// External queries the remaining budget can actually afford.
    pub affordable_external_queries: u32,
    pub estimated_cost_per_query_micros: i64,
    /// True when the budget afforded fewer queries than requested.
    pub downsized: bool,
    /// True when even ONE external query does not fit the budget (the
    /// external path is unaffordable; the decision must reject or degrade).
    pub external_unaffordable: bool,
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
    /// Per-source federation execution summary (RFC-062 §16). Present only
    /// when the federated executor ran sources for this task; `None` on the
    /// legacy path so old traces stay byte-compatible.
    #[serde(default)]
    pub federation_execution: Option<FederationExecutionTrace>,
    /// RFC-076: cost-adaptive affordability reasoning. `None` when the mode
    /// never allows external (LocalOnly).
    #[serde(default)]
    pub cost_adaptive: Option<CostAdaptiveTrace>,
}

/// Which federated sources a task planned, executed, and lost (RFC-062 §16).
/// Audit data on the trace: the plan/selection chain explains WHY a source
/// ran; this records what actually happened to it, so a timeout or failure
/// is visible in `trace show` without cross-referencing cost events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FederationExecutionTrace {
    /// Federation strategy in force (`legacy` / `plan_only` / `execute`).
    pub strategy: String,
    /// Source ids the planner selected, in plan order.
    pub planned_sources: Vec<String>,
    /// Source ids that produced a result (success or partial).
    pub executed_sources: Vec<String>,
    /// Source ids that failed.
    pub failed_sources: Vec<String>,
    /// Source ids cut off by their deadline.
    pub timed_out_sources: Vec<String>,
    /// Sources planned but never started (cancellation / deadline / fan-out).
    pub not_executed_sources: Vec<String>,
    /// Total candidates returned across executed sources.
    pub candidates: usize,
}
