//! Agent memory as a federated search source (RFC-062 Phase 6, RFC-069).
//!
//! An agent's own distilled knowledge is retrievable without being converted
//! into a `Document`: `MemoryService` already implements the RFC-013 pipeline
//! (namespace filter → BM25 + vector → RRF) and already enforces the sharing
//! policy, so this adapter is a thin translation, not a second retrieval
//! implementation.
//!
//! It lives in the application layer on purpose: the federated kernel stays
//! free of storage/service dependencies, and memory isolation stays owned by
//! `searchos-memory::NamespaceGuard` rather than being re-derived here.

use async_trait::async_trait;
use searchos_domain::ids::{SourceId, WorkspaceId};
use searchos_domain::memory::MemoryNamespace;
use searchos_federated::source::{
    SearchSource, SearchSourceDescriptor, SourceCandidate, SourceCapabilities, SourceCategory,
    SourceCostProfile, SourceError, SourceExecutionContext, SourceExecutionStatus,
    SourceLatencyProfile, SourceScope, SourceSearchRequest, SourceSearchResponse,
};
use searchos_memory::MemoryService;
use std::sync::Arc;

/// Agent memory wrapped as a federated source.
pub struct AgentMemorySource {
    descriptor: SearchSourceDescriptor,
    memory: Arc<dyn MemoryService>,
}

impl AgentMemorySource {
    /// Source id used when the caller does not name one.
    pub const DEFAULT_ID: &'static str = "agent_memory";

    /// Registers memory for one workspace. The scope mirrors the namespace
    /// the search will read with, so a workspace can never resolve another
    /// workspace's memory source through scope precedence.
    pub fn new(memory: Arc<dyn MemoryService>, workspace: WorkspaceId, priority: i32) -> Self {
        Self::with_id(Self::DEFAULT_ID, memory, workspace, priority)
    }

    pub fn with_id(
        id: &str,
        memory: Arc<dyn MemoryService>,
        workspace: WorkspaceId,
        priority: i32,
    ) -> Self {
        Self {
            descriptor: SearchSourceDescriptor {
                source_id: SourceId::new(id),
                name: "agent memory".into(),
                category: SourceCategory::AgentMemory,
                version: "1".into(),
                enabled: true,
                priority,
                capabilities: SourceCapabilities {
                    lexical_search: true,
                    // The memory service runs BM25 + vector + RRF itself.
                    semantic_search: true,
                    // Namespace isolation is enforced by the service, which
                    // is exactly what `permission_filtering` promises.
                    permission_filtering: true,
                    provenance_metadata: true,
                    freshness_metadata: true,
                    // No domain/time predicates on this port.
                    ..Default::default()
                },
                reliability: Default::default(),
                cost: SourceCostProfile {
                    estimated_cost_micros: 0,
                    currency: "USD".into(),
                    // Local memory never produces an external bill.
                    billable: false,
                },
                latency: SourceLatencyProfile {
                    p50_ms: 5,
                    p95_ms: 50,
                    timeout_ms: 2_000,
                },
                scope: SourceScope::Workspace(workspace.to_string()),
                satisfies_requirements: Vec::new(),
            },
            memory,
        }
    }

    /// Declares which requirements this memory source is authoritative for.
    pub fn with_satisfies_requirements(
        mut self,
        requirements: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.descriptor.satisfies_requirements =
            requirements.into_iter().map(|r| r.into()).collect();
        self
    }
}

#[async_trait]
impl SearchSource for AgentMemorySource {
    fn descriptor(&self) -> SearchSourceDescriptor {
        self.descriptor.clone()
    }

    async fn search(
        &self,
        request: SourceSearchRequest,
        _context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, SourceError> {
        let started = std::time::Instant::now();
        // The reader namespace is the REQUESTER's workspace, never the
        // registration scope: memory sharing rules are decided by
        // `NamespaceGuard` against this namespace.
        let reader = MemoryNamespace {
            workspace: request.workspace_id.clone(),
            project: None,
            agent: None,
        };
        let hits = self
            .memory
            .search(&request.query, &reader, request.max_results.max(1))
            .await
            .map_err(|e| SourceError::Failed(e.to_string()))?;

        let candidates: Vec<SourceCandidate> = hits
            .into_iter()
            .map(|hit| {
                let entry = hit.entry;
                SourceCandidate {
                    source_id: self.descriptor.source_id.clone(),
                    // A memory entry has no web host. A stable local URI
                    // keeps identity/dedup honest (the same rule the local
                    // ingestion path uses for `searchos-local://`).
                    canonical_ref: entry
                        .url
                        .clone()
                        .unwrap_or_else(|| format!("searchos-memory://{}", entry.id)),
                    title: Some(entry.title.clone()),
                    snippet: entry.content.clone(),
                    raw_score: Some(hit.score),
                    observed_at: entry.updated_at,
                    published_at: Some(entry.created_at),
                    satisfies_requirements: Vec::new(),
                    // `MemoryService::search` already applied the sharing
                    // policy for this reader namespace.
                    permission_verified: true,
                }
            })
            .collect();

        let metrics = searchos_observability::MetricsRegistry::global();
        metrics.inc_counter("federation.memory.searched_total");
        for _ in &candidates {
            metrics.inc_counter("federation.memory.hits_total");
        }

        Ok(SourceSearchResponse {
            source_id: self.descriptor.source_id.clone(),
            status: SourceExecutionStatus::Success,
            candidates,
            failure: None,
            estimated_cost_micros: 0,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// Maps a memory entry onto the unified resource model (RFC-064).
///
/// Memory is user-owned knowledge: it is not age-stale, and its permission is
/// derived from the SHARING POLICY, not assumed. A `Private` entry stays
/// agent-scoped; `GlobalPublic` is the only policy that grants egress-free
/// global visibility, and even then egress stays forbidden by default
/// (RFC-087).
pub fn memory_resource(
    entry: &searchos_domain::memory::MemoryEntry,
    reader: &MemoryNamespace,
) -> searchos_federated::SearchResource {
    use searchos_domain::memory::SharingPolicy;
    use searchos_federated::{PermissionRef, ResourceType};

    let allowed_agents = match entry.sharing {
        SharingPolicy::Private => entry
            .namespace
            .agent
            .iter()
            .cloned()
            .collect::<Vec<String>>(),
        _ => Vec::new(),
    };
    let verified =
        searchos_memory::NamespaceGuard::may_read(&entry.namespace, entry.sharing, reader);
    let permission = PermissionRef {
        workspace_id: entry.namespace.workspace.to_string(),
        allowed_agents,
        egress_allowed: false,
        verified,
    };
    searchos_federated::SearchResource::new(
        ResourceType::KnowledgeFact,
        &entry.namespace.workspace,
        entry
            .url
            .clone()
            .unwrap_or_else(|| format!("searchos-memory://{}", entry.id)),
    )
    .with_title(entry.title.clone())
    .with_permission(permission)
}
