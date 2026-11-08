//! SearchOS core domain (RFC-002).
//!
//! This crate contains the core domain objects and domain rules:
//! strongly-typed identifiers, search request/response models, evidence,
//! documents and versions, cost primitives, execution plans, decision
//! traces, and the unified [`SearchOsError`].
//!
//! Dependency rule (RFC-002 2.5): **domain does not depend on infrastructure.**
//! Only serde/chrono/uuid/sha2 are used for serialization, time and hashing.

pub mod cost;
pub mod decision;
pub mod document;
pub mod error;
pub mod evidence;
pub mod ids;
pub mod memory;
pub mod plan;
pub mod provider;
pub mod request;
pub mod response;
pub mod retrieval;

pub use cost::{
    CostEvent, CostOperation, CostQuery, CostSummary, Money, ReuseCostViews, SavingsReport,
};
pub use decision::{
    CoverageGap, DecisionReason, DecisionStageTrace, DecisionTrace, GapPriority,
    GapResolutionStrategy, RequirementStatus, SearchDecision,
};
pub use document::{Document, DocumentChunk, DocumentStatus, DocumentVersion};
pub use error::{SearchOsError, ValidationError};
pub use evidence::{Evidence, Provenance, ProvenanceOrigin, ReliabilityLevel};
pub use ids::*;
pub use memory::{MemoryEntry, MemoryNamespace, SharingPolicy};
pub use plan::{ExecutionDependency, ExecutionNode, ExecutionNodeKind, ExecutionPlan, PlanStatus};
pub use provider::ProviderCapabilities;
pub use request::{
    FreshnessRequirement, SearchBudget, SearchConstraints, SearchIntent, SearchMode, SearchRequest,
};
pub use response::{
    CostReport, DecisionReport, QualityReport, SearchResponse, SearchResultItem, SearchStatus,
};
pub use retrieval::{IndexedDocument, RetrievalCandidate, RetrievalQuery, SearchCacheKey};

use chrono::{DateTime, Utc};

/// Returns the current UTC timestamp (helper used across crates).
pub fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

/// Generates a prefixed, time-sortable unique identifier such as
/// `req_01ab...`. Used for requests, tasks, traces, plans, cost events.
pub fn new_id(prefix: &str) -> String {
    let id = uuid::Uuid::new_v4();
    format!("{prefix}_{id}")
}
