//! Federated search kernel — RFC-062 / RFC-063 / RFC-064 of the final
//! [RFC Master Index](../../docs/rfc/rfc-master-index.md).
//!
//! SearchOS v1.x hard-wired two retrieval branches: local knowledge through
//! `LocalKnowledgePort`, external results through `ProviderRouter`. Adding a
//! fifth source (agent memory, sessions, structured data, an external API)
//! meant editing the search service. This crate replaces that shape with one
//! abstraction and one registry:
//!
//! ```text
//! Search Request
//!   → Query Understanding (existing)
//!   → FederatedPlanner::plan(mode, strategy, requirements, descriptors)   ← this crate
//!   → SearchSource::search(...) per selected source                        ← trait defined here
//!   → Result Normalization → Dedup → Trust-aware Fusion (RFC-066)
//!   → Evidence Engine + Quality Gate (existing)
//!   → Context Assembly + Token Budget (RFC-067/068)
//! ```
//!
//! # What is delivered here
//!
//! * [`source`] — the `SearchSource` trait, descriptor, capabilities, cost /
//!   latency / reliability profiles, scope, execution status and normalised
//!   candidate/ response types.
//! * [`registry`] — scoped registration, discovery, capability filtering,
//!   enable/disable and health.
//! * [`planner`] — the pure, auditable selection algorithm with the full
//!   rejection chain and stop conditions.
//! * [`resource`] — the unified resource model shared by all sources.
//!
//! # What is deliberately *not* here
//!
//! No execution and no adapters. Wrapping the existing local retrieval and
//! provider router as sources (Phase 2 of RFC-062) and rewiring
//! `search_service.rs` is a separate, higher-risk change; keeping this crate
//! free of runtime dependencies means the invariants below are testable
//! without one, and that the existing 559-test suite stays green.
//!
//! # Invariants (asserted in `tests/federated_invariants.rs`)
//!
//! * `INV-FED-01` Fail-open is availability, never a quality verdict.
//! * `INV-FED-02` `SearchMode` is the permission boundary; a
//!   [`FederationStrategy`] can never widen it.
//! * `INV-FED-03` A source never decides global quality or selects siblings.
//! * `INV-FED-04` The planner never fans out to every registered source.
//! * `INV-FED-05` Covered requirements are not re-purchased.
//! * `INV-FED-06` Every rejection is recorded; nothing is silently dropped.
//! * `INV-FED-07` Permission is fail-closed until verified.
//! * `INV-FED-08` Resource identity is stable across sources; occurrences are
//!   not independent sources.

pub mod adapters;
pub mod execute;
pub mod fusion;
pub mod planner;
pub mod registry;
pub mod resource;
pub mod source;

pub use adapters::{domain_matches, LocalIndexSource, WebSearchProviderSource};
pub use execute::{
    ExecutionControls, ExecutionReport, ExecutionScope, FederatedExecutor, SourceExecutionResult,
};
pub use fusion::{fuse_ranks, RankedSourceList};
pub use planner::{
    aggregate, billable_cost, free_cost, FederatedOutcome, FederatedPlanner, FederatedSearchPlan,
    FederationStrategy, PlanningContext, RejectedSource, RejectionReason, SearchStage, SourceTask,
    StageExecutionMode, StopCondition,
};
pub use registry::{categories_of, scope_label, InMemorySourceRegistry, RegistryError};
pub use resource::{
    canonicalize_reference, derive_resource_id, ContentRef, PermissionRef, ResourceType,
    SearchResource,
};
pub use searchos_domain::SearchMode;
pub use source::{
    ReliabilityProfile, SearchSource, SearchSourceDescriptor, SourceCandidate, SourceCapabilities,
    SourceCategory, SourceCostProfile, SourceDescriptorRef, SourceError, SourceExecutionContext,
    SourceExecutionStatus, SourceFailure, SourceFilter, SourceHealth, SourceLatencyProfile,
    SourceResult, SourceScope, SourceSearchRequest, SourceSearchResponse,
};

/// Crate version marker for diagnostics and future contract tests.
pub const KERNEL_VERSION: &str = "1";
