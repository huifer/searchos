//! Quality gate & evidence evaluation (RFC-005) + decision engine (RFC-004).
//!
//! Pipeline (RFC-004 4.2):
//! Request → Resolve Policy → Classify Query → Evaluate Local Availability →
//! Select Retrieval Depth → Local Retrieval → Quality Gate →
//! Pass: local result / Partial: gap analysis / Fail: external search.

pub mod engine;
pub mod gap;
pub mod quality;
pub mod state;
pub mod trace;

pub use engine::{
    DecisionEngine, DecisionInput, DefaultDecisionEngine, LocalCandidateEval, LocalKnowledgePort,
};
pub use gap::{
    build_external_plan, ExternalQueryTask, ExternalSearchPlan, GapAnalyzer, InformationNeed,
};
pub use quality::{EvidenceEvaluation, QualityGate, QualityVerdict};
pub use state::{
    transition_allowed, DecisionStage, DecisionState, DecisionStateError, DecisionStateMachine,
    StageStatus,
};
pub use trace::TraceCollector;
