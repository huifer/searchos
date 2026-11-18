//! The unified `SearchSource` contract (RFC-062 §4, RFC-063).
//!
//! Everything retrievable — local knowledge, vector stores, agent memory,
//! sessions, web search, web documents, external APIs, structured data —
//! implements one trait and declares one descriptor. The pipeline must stop
//! hard-wiring "local vs external" branches: adding a source must not require
//! editing the planner or the search service.
//!
//! Two boundaries are load-bearing:
//!
//! * **A source never decides global quality.** It returns candidates and an
//!   execution record; the quality gate owns the verdict (ADR-062-003).
//! * **A source never escapes the mode permission boundary.** `SearchMode`
//!   stays the authority on whether local or external retrieval is allowed at
//!   all; federation strategy is orthogonal to it (ADR-062-008).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use searchos_domain::ids::{RequestId, SourceId, WorkspaceId};
use searchos_domain::{SearchMode, SearchOsError};
use serde::{Deserialize, Serialize};

/// Which family of information a source provides (RFC-062 4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCategory {
    LocalKnowledge,
    WebSearch,
    WebDocument,
    FileSystem,
    AgentMemory,
    Conversation,
    KnowledgeGraph,
    VectorStore,
    ExternalApi,
    StructuredDatabase,
    Custom,
}

impl SourceCategory {
    /// Snake-case wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LocalKnowledge => "local_knowledge",
            Self::WebSearch => "web_search",
            Self::WebDocument => "web_document",
            Self::FileSystem => "file_system",
            Self::AgentMemory => "agent_memory",
            Self::Conversation => "conversation",
            Self::KnowledgeGraph => "knowledge_graph",
            Self::VectorStore => "vector_store",
            Self::ExternalApi => "external_api",
            Self::StructuredDatabase => "structured_database",
            Self::Custom => "custom",
        }
    }

    /// Whether contacting this category leaves the machine. This is the one
    /// classification the mode permission boundary is allowed to key on, and
    /// it is deliberately derived from the category instead of being typed in
    /// per source, so a mislabelled source cannot smuggle an external call
    /// into a `local_only` request.
    pub fn is_external(&self) -> bool {
        matches!(
            self,
            Self::WebSearch
                | Self::WebDocument
                | Self::ExternalApi
                | Self::StructuredDatabase
                | Self::Custom
        )
    }
}

/// Visibility scope of a registration (RFC-063 5.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceScope {
    Global,
    Workspace(String),
    Project { workspace: String, project: String },
    Agent { workspace: String, agent: String },
}

impl SourceScope {
    /// Scope precedence: Agent > Project > Workspace > Global.
    ///
    /// A higher-priority scope may override defaults, but it can never relax
    /// a permission or security rule — the planner enforces that separately.
    pub fn precedence(&self) -> u8 {
        match self {
            Self::Global => 0,
            Self::Workspace(_) => 1,
            Self::Project { .. } => 2,
            Self::Agent { .. } => 3,
        }
    }

    /// Workspace this scope belongs to, if it is workspace-bound.
    pub fn workspace(&self) -> Option<&str> {
        match self {
            Self::Global => None,
            Self::Workspace(ws) => Some(ws),
            Self::Project { workspace, .. } => Some(workspace),
            Self::Agent { workspace, .. } => Some(workspace),
        }
    }

    /// Whether this scope can serve a request from `requester`.
    ///
    /// A global source serves every workspace; a workspace-bound source only
    /// serves its own. This is the RFC-013 8.7 sharing rule (default
    /// isolation, explicit sharing through a `Global` registration).
    pub fn serves(&self, requester: &WorkspaceId) -> bool {
        match self.workspace() {
            None => true,
            Some(ws) => ws == requester.as_str(),
        }
    }
}

/// What a source can actually do (RFC-062 4.3).
///
/// The planner may only ask a source for operations it declared. Defaulting
/// everything to `false` is intentional: a descriptor that forgets to declare
/// a capability loses work rather than producing a wrong result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCapabilities {
    pub lexical_search: bool,
    pub semantic_search: bool,
    pub exact_lookup: bool,
    pub metadata_filter: bool,
    pub time_filter: bool,
    pub full_document_fetch: bool,
    pub incremental_sync: bool,
    pub pagination: bool,
    pub freshness_metadata: bool,
    pub provenance_metadata: bool,
    pub permission_filtering: bool,
}

/// Observed dependability of a source (RFC-062 4.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReliabilityProfile {
    /// 0.0..=1.0 historical success rate.
    pub success_rate: f32,
    /// 0.0..=1.0 confidence in the source's own metadata.
    pub metadata_trust: f32,
    /// Consecutive failures; drives the circuit-breaker decision.
    pub consecutive_failures: u32,
    /// Whether the planner may send an unverifiable query to it at all.
    pub circuit_open: bool,
    /// RFC-080: RESULT-LEVEL quality, learned from judged results.
    ///
    /// 0.0..=1.0 exponential moving average of the fraction of a source's
    /// JUDGED results that the relevance judge marked relevant. It is
    /// `None`-like while `result_samples` is below the minimum: an UNJUDGED
    /// source is unknown, not bad, and must never be punished for it.
    #[serde(default)]
    pub result_quality: f32,
    /// How many judged results fed `result_quality`.
    #[serde(default)]
    pub result_samples: u32,
}

impl Default for ReliabilityProfile {
    fn default() -> Self {
        Self {
            success_rate: 1.0,
            metadata_trust: 0.5,
            consecutive_failures: 0,
            circuit_open: false,
            result_quality: 0.0,
            result_samples: 0,
        }
    }
}

/// Expected cost shape of a source (RFC-062 4.4). Costs are micros so the
/// existing cost ledger units stay authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCostProfile {
    /// Estimated cost of one source execution, in micro units of `currency`.
    pub estimated_cost_micros: i64,
    pub currency: String,
    /// Whether executing this source can produce a real external bill.
    pub billable: bool,
}

impl Default for SourceCostProfile {
    fn default() -> Self {
        Self {
            estimated_cost_micros: 0,
            currency: "USD".into(),
            billable: false,
        }
    }
}

/// Expected latency shape of a source (RFC-062 4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLatencyProfile {
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub timeout_ms: u64,
}

impl Default for SourceLatencyProfile {
    fn default() -> Self {
        Self {
            p50_ms: 50,
            p95_ms: 400,
            timeout_ms: 5_000,
        }
    }
}

/// Everything the planner needs to know about a registered source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchSourceDescriptor {
    pub source_id: SourceId,
    pub name: String,
    pub category: SourceCategory,
    pub version: String,
    pub enabled: bool,
    /// Higher runs first inside a sequential stage.
    pub priority: i32,
    pub capabilities: SourceCapabilities,
    pub reliability: ReliabilityProfile,
    pub cost: SourceCostProfile,
    pub latency: SourceLatencyProfile,
    pub scope: SourceScope,
    /// Requirement ids this source is authoritative for (e.g. an official
    /// docs source for `required_official_source`).
    pub satisfies_requirements: Vec<String>,
}

impl SearchSourceDescriptor {
    /// A minimal descriptor; capabilities/reliability/cost/latency default to
    /// the most conservative values.
    pub fn new(
        source_id: impl Into<SourceId>,
        name: impl Into<String>,
        category: SourceCategory,
        scope: SourceScope,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            name: name.into(),
            category,
            version: "1".into(),
            enabled: true,
            priority: 0,
            capabilities: SourceCapabilities::default(),
            reliability: ReliabilityProfile::default(),
            cost: SourceCostProfile::default(),
            latency: SourceLatencyProfile::default(),
            scope,
            satisfies_requirements: Vec::new(),
        }
    }

    pub fn with_capabilities(mut self, capabilities: SourceCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_cost(mut self, estimated_cost_micros: i64, billable: bool) -> Self {
        self.cost.estimated_cost_micros = estimated_cost_micros;
        self.cost.billable = billable;
        self
    }

    pub fn with_reliability(mut self, success_rate: f32, circuit_open: bool) -> Self {
        self.reliability.success_rate = success_rate;
        self.reliability.circuit_open = circuit_open;
        self
    }

    pub fn with_requirements(mut self, requirements: Vec<String>) -> Self {
        self.satisfies_requirements = requirements;
        self
    }
}

/// A normalised, capability-filtered retrieval request for one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSearchRequest {
    pub query: String,
    pub mode: SearchMode,
    pub workspace_id: WorkspaceId,
    pub max_results: usize,
    /// Requirement ids this source is being asked to satisfy.
    pub requirements: Vec<String>,
    /// Only return results at or after this instant when the source declares
    /// `freshness_metadata`; otherwise the planner must not ask for it.
    pub fetched_after: Option<DateTime<Utc>>,
    /// Source-domain policy. Adapters pass these through only when they can
    /// honour them; a source that cannot filter by domain declares
    /// `metadata_filter: false` and the planner will not rely on it.
    #[serde(default)]
    pub required_domains: Vec<String>,
    #[serde(default)]
    pub excluded_domains: Vec<String>,
}

impl SourceSearchRequest {
    /// A minimal request; transports fill the optional policy fields.
    pub fn new(
        query: impl Into<String>,
        mode: SearchMode,
        workspace_id: WorkspaceId,
        max_results: usize,
    ) -> Self {
        Self {
            query: query.into(),
            mode,
            workspace_id,
            max_results,
            requirements: Vec::new(),
            fetched_after: None,
            required_domains: Vec::new(),
            excluded_domains: Vec::new(),
        }
    }
}

/// One normalised candidate returned by a source (RFC-062 9.1).
///
/// `raw_score` is preserved and never compared across sources: rank is the
/// only comparable signal, which is why fusion is rank-based (RFC-066).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceCandidate {
    pub source_id: SourceId,
    /// Canonical reference (URL / URI). Identity for dedup.
    pub canonical_ref: String,
    pub title: Option<String>,
    pub snippet: String,
    pub raw_score: Option<f32>,
    pub observed_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    /// Requirement ids this candidate answers.
    pub satisfies_requirements: Vec<String>,
    /// Whether the source itself enforced workspace/agent permissions. When
    /// false, the caller must not place this candidate in an Agent context.
    pub permission_verified: bool,
}

/// Per-source execution record. Never folded into a global status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceExecutionStatus {
    Success,
    Partial,
    Timeout,
    RateLimited,
    Unavailable,
    PermissionDenied,
    Failed,
    Skipped,
    Cancelled,
}

impl SourceExecutionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Partial => "partial",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate_limited",
            Self::Unavailable => "unavailable",
            Self::PermissionDenied => "permission_denied",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether this outcome counts as an availability failure for the
    /// circuit breaker, as opposed to a legitimate empty result.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::RateLimited | Self::Unavailable | Self::Failed
        )
    }

    /// Whether the source was reached but could not fully answer.
    pub fn is_partial(&self) -> bool {
        matches!(self, Self::Partial)
    }
}

/// Why a source execution did not fully succeed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFailure {
    pub status: SourceExecutionStatus,
    pub message: String,
}

impl SourceFailure {
    pub fn new(status: SourceExecutionStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

/// What one source returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSearchResponse {
    pub source_id: SourceId,
    pub status: SourceExecutionStatus,
    pub candidates: Vec<SourceCandidate>,
    /// Set when the source is authoritative for a requirement and still
    /// failed. This is what turns fail-open into an explicit `partial`
    /// instead of a disguised success (ADR-062-004).
    pub failure: Option<SourceFailure>,
    pub estimated_cost_micros: i64,
    pub latency_ms: u64,
}

impl SourceSearchResponse {
    /// Successful, possibly empty result. An empty result is NOT a failure.
    pub fn success(
        source_id: impl Into<String>,
        candidates: Vec<SourceCandidate>,
        latency_ms: u64,
    ) -> Self {
        Self {
            source_id: SourceId::new(source_id),
            status: SourceExecutionStatus::Success,
            candidates,
            failure: None,
            estimated_cost_micros: 0,
            latency_ms,
        }
    }

    pub fn failed(
        source_id: impl Into<String>,
        status: SourceExecutionStatus,
        message: impl Into<String>,
    ) -> Self {
        let source_id = SourceId::new(source_id);
        let failure = SourceFailure::new(status, message);
        Self {
            source_id: source_id.clone(),
            status,
            candidates: Vec::new(),
            failure: Some(failure),
            estimated_cost_micros: 0,
            latency_ms: 0,
        }
    }

    pub fn with_cost(mut self, estimated_cost_micros: i64) -> Self {
        self.estimated_cost_micros = estimated_cost_micros;
        self
    }
}

/// Liveness of a source, tracked by the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceHealth {
    Healthy,
    Degraded,
    Down,
    Unknown,
}

/// Error type for source implementations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SourceError {
    #[error("source unavailable: {0}")]
    Unavailable(String),
    #[error("source timeout: {0}")]
    Timeout(String),
    #[error("source rate limited: {0}")]
    RateLimited(String),
    #[error("source permission denied: {0}")]
    PermissionDenied(String),
    #[error("source failed: {0}")]
    Failed(String),
}

impl SourceError {
    pub fn status(&self) -> SourceExecutionStatus {
        match self {
            Self::Unavailable(_) => SourceExecutionStatus::Unavailable,
            Self::Timeout(_) => SourceExecutionStatus::Timeout,
            Self::RateLimited(_) => SourceExecutionStatus::RateLimited,
            Self::PermissionDenied(_) => SourceExecutionStatus::PermissionDenied,
            Self::Failed(_) => SourceExecutionStatus::Failed,
        }
    }

    /// Maps into the domain error taxonomy so existing transport error codes
    /// keep working when sources are wired into the pipeline.
    pub fn to_domain(&self) -> SearchOsError {
        match self {
            Self::Unavailable(m) | Self::Failed(m) => SearchOsError::Provider(m.clone()),
            Self::Timeout(m) => SearchOsError::Timeout(m.clone()),
            Self::RateLimited(m) => SearchOsError::Provider(m.clone()),
            Self::PermissionDenied(m) => SearchOsError::Security(m.clone()),
        }
    }
}

/// Execution budget/deadline handed to one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceExecutionContext {
    pub request_id: RequestId,
    pub source_id: SourceId,
    pub deadline: DateTime<Utc>,
    pub max_results: usize,
    pub max_estimated_cost_micros: i64,
    pub started_at: DateTime<Utc>,
}

/// The one contract every retrievable information source implements.
///
/// A source declares capabilities, executes a normalised request, reports its
/// own health, and nothing else. It never decides the global verdict.
#[async_trait]
pub trait SearchSource: Send + Sync {
    fn descriptor(&self) -> SearchSourceDescriptor;

    async fn search(
        &self,
        request: SourceSearchRequest,
        context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, SourceError>;

    /// Liveness probe. The default is `Unknown`: an adapter that cannot
    /// self-probe says so instead of claiming health. Health is bookkeeping,
    /// not admission control — the planner rejects an unhealthy source via
    /// its reliability stage (RFC-063 §5).
    async fn health_check(&self) -> SourceHealth {
        SourceHealth::Unknown
    }
}

/// Selection filter used by the registry and the planner.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFilter {
    pub enabled_only: bool,
    pub categories: Vec<SourceCategory>,
    pub workspace_id: Option<WorkspaceId>,
    pub require_lexical: bool,
    pub require_semantic: bool,
    pub require_provenance: bool,
}

/// Result alias for source implementations.
pub type SourceResult<T> = Result<T, SourceError>;

/// A descriptor plus its last observed health, for registry diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceDescriptorRef {
    pub descriptor: SearchSourceDescriptor,
    pub health: SourceHealth,
}
