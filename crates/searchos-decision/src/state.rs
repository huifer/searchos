//! Decision state machine and structured stage trace (RFC-004 v1.1 §7, §11).
//!
//! The engine is a *control system*: illegal state jumps are rejected rather
//! than silently repaired. Every executed stage produces a structured,
//! immutable `DecisionStageTrace` record; traces never store API keys or
//! private document bodies (RFC-015 15.5).

use searchos_domain::decision::DecisionStageTrace;
use serde::{Deserialize, Serialize};
use std::fmt;

/// States of a search decision task (RFC-004 v1.1 7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionState {
    Received,
    Validating,
    PolicyResolved,
    QueryUnderstood,
    LocalRetrieving,
    LocalEvaluating,
    QualityChecking,
    GapAnalyzing,
    PlanningExternal,
    SelectingProvider,
    ExecutingExternal,
    MergingEvidence,
    FinalQualityChecking,
    PersistingKnowledge,
    Completed,
    Partial,
    InsufficientEvidence,
    BudgetExceeded,
    Failed,
    Cancelled,
}

impl DecisionState {
    /// Stable wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Validating => "validating",
            Self::PolicyResolved => "policy_resolved",
            Self::QueryUnderstood => "query_understood",
            Self::LocalRetrieving => "local_retrieving",
            Self::LocalEvaluating => "local_evaluating",
            Self::QualityChecking => "quality_checking",
            Self::GapAnalyzing => "gap_analyzing",
            Self::PlanningExternal => "planning_external",
            Self::SelectingProvider => "selecting_provider",
            Self::ExecutingExternal => "executing_external",
            Self::MergingEvidence => "merging_evidence",
            Self::FinalQualityChecking => "final_quality_checking",
            Self::PersistingKnowledge => "persisting_knowledge",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::InsufficientEvidence => "insufficient_evidence",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Pipeline stage names recorded in a trace (RFC-004 v1.1 11.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStage {
    Validation,
    PolicyResolution,
    QueryUnderstanding,
    LocalRetrieval,
    EvidenceEvaluation,
    QualityGate,
    GapAnalysis,
    ExternalPlanning,
    ProviderSelection,
    ExternalExecution,
    EvidenceMerge,
    FinalQualityGate,
    Persistence,
}

impl DecisionStage {
    /// Stable wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::PolicyResolution => "policy_resolution",
            Self::QueryUnderstanding => "query_understanding",
            Self::LocalRetrieval => "local_retrieval",
            Self::EvidenceEvaluation => "evidence_evaluation",
            Self::QualityGate => "quality_gate",
            Self::GapAnalysis => "gap_analysis",
            Self::ExternalPlanning => "external_planning",
            Self::ProviderSelection => "provider_selection",
            Self::ExternalExecution => "external_execution",
            Self::EvidenceMerge => "evidence_merge",
            Self::FinalQualityGate => "final_quality_gate",
            Self::Persistence => "persistence",
        }
    }
}

/// Stage outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Completed,
    Partial,
    Skipped,
    Failed,
}

impl StageStatus {
    /// Stable wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// Rejected state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionStateError {
    pub from: DecisionState,
    pub to: DecisionState,
}

impl fmt::Display for DecisionStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "illegal decision state transition {} -> {}",
            self.from.as_str(),
            self.to.as_str()
        )
    }
}

impl std::error::Error for DecisionStateError {}

/// Whether `from -> to` is an allowed transition (RFC-004 v1.1 7.3).
pub fn transition_allowed(from: DecisionState, to: DecisionState) -> bool {
    use DecisionState::*;
    if to == Cancelled || to == Failed {
        // Cancellation and hard failure are reachable from any active state.
        return !matches!(
            from,
            Completed | Partial | InsufficientEvidence | BudgetExceeded | Failed | Cancelled
        );
    }
    matches!(
        (from, to),
        (Received, Validating)
            | (Validating, PolicyResolved)
            | (PolicyResolved, QueryUnderstood)
            | (QueryUnderstood, LocalRetrieving)
            | (LocalRetrieving, LocalEvaluating)
            | (LocalEvaluating, QualityChecking)
            | (QualityChecking, Completed)
            | (QualityChecking, GapAnalyzing)
            | (QualityChecking, InsufficientEvidence)
            | (GapAnalyzing, PlanningExternal)
            | (PlanningExternal, SelectingProvider)
            | (PlanningExternal, BudgetExceeded)
            | (SelectingProvider, ExecutingExternal)
            // Provider fallback / retry stays inside execution.
            | (ExecutingExternal, ExecutingExternal)
            | (ExecutingExternal, MergingEvidence)
            | (MergingEvidence, FinalQualityChecking)
            | (FinalQualityChecking, Completed)
            | (FinalQualityChecking, Partial)
            | (FinalQualityChecking, InsufficientEvidence)
            | (FinalQualityChecking, PersistingKnowledge)
            | (PersistingKnowledge, Completed)
            | (PersistingKnowledge, Partial)
    )
}

/// State machine + stage recorder for one decision.
#[derive(Debug, Clone)]
pub struct DecisionStateMachine {
    state: DecisionState,
    stages: Vec<DecisionStageTrace>,
    started_ms: u64,
    /// Monotonic-ish wall clock of the current stage's own start. Stage
    /// durations are measured between records, not copied from the collector.
    last_stage_started_ms: u64,
}

impl Default for DecisionStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl DecisionStateMachine {
    /// New machine in `Received`.
    pub fn new() -> Self {
        let started_ms = now_ms();
        Self {
            state: DecisionState::Received,
            stages: Vec::new(),
            started_ms,
            last_stage_started_ms: started_ms,
        }
    }

    /// Current state.
    pub fn state(&self) -> DecisionState {
        self.state
    }

    /// Attempts a transition; rejects illegal jumps.
    pub fn transition(&mut self, to: DecisionState) -> Result<(), DecisionStateError> {
        if !transition_allowed(self.state, to) {
            return Err(DecisionStateError {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }

    /// Records one structured stage with its own real start timestamp.
    /// An explicit non-zero duration remains authoritative for deterministic
    /// tests; zero means "measure it from this stage's wall-clock start".
    pub fn record(
        &mut self,
        stage: DecisionStage,
        status: StageStatus,
        duration_ms: u64,
        input_summary: impl Into<String>,
        output_summary: impl Into<String>,
        reasons: Vec<String>,
    ) {
        let now = now_ms();
        if let Some(previous) = self.stages.last_mut() {
            let measured = now.saturating_sub(self.last_stage_started_ms);
            previous.duration_ms = previous.duration_ms.max(measured);
        }
        self.last_stage_started_ms = now;
        self.stages.push(DecisionStageTrace {
            stage: stage.as_str().into(),
            status: status.as_str().into(),
            started_at_ms: now,
            duration_ms,
            input_summary: input_summary.into(),
            output_summary: output_summary.into(),
            reasons,
        });
    }

    /// Closes the final stage's measured duration. Call before exporting.
    pub fn finish_stages(&mut self) {
        let now = now_ms();
        debug_assert!(now >= self.started_ms, "wall clock moved backwards");
        if let Some(last) = self.stages.last_mut() {
            let measured = now.saturating_sub(self.last_stage_started_ms);
            last.duration_ms = last.duration_ms.max(measured);
        }
        self.last_stage_started_ms = now;
    }

    /// The immutable stage records produced so far.
    pub fn stages(&self) -> &[DecisionStageTrace] {
        &self.stages
    }

    /// Consumes the machine into its stage records.
    pub fn into_stages(mut self) -> Vec<DecisionStageTrace> {
        self.finish_stages();
        self.stages
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_transitions_are_allowed() {
        let mut m = DecisionStateMachine::new();
        for s in [
            DecisionState::Validating,
            DecisionState::PolicyResolved,
            DecisionState::QueryUnderstood,
            DecisionState::LocalRetrieving,
            DecisionState::LocalEvaluating,
            DecisionState::QualityChecking,
            DecisionState::Completed,
        ] {
            m.transition(s).unwrap();
        }
        assert_eq!(m.state(), DecisionState::Completed);
    }

    #[test]
    fn illegal_jump_is_rejected() {
        let mut m = DecisionStateMachine::new();
        let err = m.transition(DecisionState::ExecutingExternal).unwrap_err();
        assert_eq!(err.from, DecisionState::Received);
        assert_eq!(err.to, DecisionState::ExecutingExternal);
        assert_eq!(m.state(), DecisionState::Received);
    }

    #[test]
    fn terminal_states_do_not_reopen() {
        let mut m = DecisionStateMachine::new();
        m.transition(DecisionState::Validating).unwrap();
        m.transition(DecisionState::PolicyResolved).unwrap();
        m.transition(DecisionState::QueryUnderstood).unwrap();
        m.transition(DecisionState::LocalRetrieving).unwrap();
        m.transition(DecisionState::LocalEvaluating).unwrap();
        m.transition(DecisionState::QualityChecking).unwrap();
        m.transition(DecisionState::GapAnalyzing).unwrap();
        m.transition(DecisionState::PlanningExternal).unwrap();
        m.transition(DecisionState::BudgetExceeded).unwrap();
        assert!(m.transition(DecisionState::ExecutingExternal).is_err());
    }

    #[test]
    fn stage_records_are_structured_and_ordered() {
        let mut m = DecisionStateMachine::new();
        m.record(
            DecisionStage::Validation,
            StageStatus::Completed,
            1,
            "request",
            "valid",
            vec!["ok".into()],
        );
        m.record(
            DecisionStage::QualityGate,
            StageStatus::Partial,
            2,
            "12 candidates",
            "pricing stale",
            vec!["freshness_failed".into()],
        );
        let s = m.stages();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].stage, "validation");
        assert_eq!(s[1].status, "partial");
        assert_eq!(s[1].reasons, vec!["freshness_failed".to_string()]);
    }

    /// Part 30 §30.11: stage timestamps are per-stage and durations are
    /// measured, not copied from the collector's start time.
    #[test]
    fn stage_trace_records_real_timestamps_and_duration() {
        let mut m = DecisionStateMachine::new();
        let collector_start = m.started_ms;
        m.record(
            DecisionStage::Validation,
            StageStatus::Completed,
            0,
            "request",
            "valid",
            vec![],
        );
        std::thread::sleep(std::time::Duration::from_millis(3));
        m.record(
            DecisionStage::QualityGate,
            StageStatus::Completed,
            0,
            "evidence",
            "passed",
            vec![],
        );
        m.finish_stages();
        let stages = m.stages();
        assert_eq!(stages.len(), 2);
        assert!(stages[0].started_at_ms >= collector_start);
        assert!(stages[1].started_at_ms > stages[0].started_at_ms);
        assert!(stages[0].duration_ms > 0, "first duration was measured");
        assert!(stages[1].duration_ms <= stages[0].duration_ms + 3_000);
    }

    #[test]
    fn cancellation_propagates_from_active_state() {
        let mut m = DecisionStateMachine::new();
        m.transition(DecisionState::Validating).unwrap();
        m.transition(DecisionState::Cancelled).unwrap();
        assert_eq!(m.state(), DecisionState::Cancelled);
    }
}
