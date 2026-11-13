//! RFC-069 acceptance: agent memory as a federated source.
//!
//! The load-bearing property is not "memory is searchable" (the memory service
//! already does that) but that federating it **cannot widen access**: the
//! adapter reads with the REQUESTER's namespace, and the sharing policy is
//! still decided by `NamespaceGuard`.

use async_trait::async_trait;
use searchos_application::memory_source::{memory_resource, AgentMemorySource};
use searchos_domain::ids::{MemoryEntryId, SourceId, WorkspaceId};
use searchos_domain::memory::{MemoryEntry, MemoryNamespace, SharingPolicy};
use searchos_domain::SearchMode;
use searchos_federated::source::{
    SearchSource, SourceCategory, SourceExecutionContext, SourceSearchRequest,
};
use searchos_federated::SourceScope;
use searchos_memory::{MemorySearchHit, MemoryService, MemoryStats};
use std::sync::{Arc, Mutex};

fn ns(workspace: &str, agent: Option<&str>) -> MemoryNamespace {
    MemoryNamespace {
        workspace: WorkspaceId::new(workspace),
        project: None,
        agent: agent.map(|a| a.to_string()),
    }
}

fn entry(id: &str, owner: MemoryNamespace, sharing: SharingPolicy, content: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        namespace: owner,
        sharing,
        title: format!("title {id}"),
        content: content.into(),
        url: None,
        tags: Vec::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding_status: "none".into(),
    }
}

/// A memory double that applies the REAL NamespaceGuard, so the test exercises
/// the actual isolation rule rather than a re-implementation of it.
struct GuardedMemory {
    entries: Vec<MemoryEntry>,
    readers: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl MemoryService for GuardedMemory {
    async fn remember(
        &self,
        _namespace: MemoryNamespace,
        _sharing: SharingPolicy,
        _title: &str,
        _content: &str,
        _url: Option<&str>,
        _tags: &[String],
    ) -> Result<MemoryEntry, searchos_domain::SearchOsError> {
        unimplemented!("not needed for these tests")
    }

    async fn search(
        &self,
        _query: &str,
        reader: &MemoryNamespace,
        _top_k: usize,
    ) -> Result<Vec<MemorySearchHit>, searchos_domain::SearchOsError> {
        self.readers.lock().unwrap().push(reader.scope_string());
        Ok(self
            .entries
            .iter()
            .filter(|e| searchos_memory::NamespaceGuard::may_read(&e.namespace, e.sharing, reader))
            .map(|e| MemorySearchHit {
                entry: e.clone(),
                score: 0.8,
            })
            .collect())
    }

    async fn delete(&self, _id: &str) -> Result<(), searchos_domain::SearchOsError> {
        Ok(())
    }

    async fn export(
        &self,
        reader: &MemoryNamespace,
    ) -> Result<Vec<MemoryEntry>, searchos_domain::SearchOsError> {
        Ok(searchos_memory::NamespaceGuard::visible_entries(
            self.entries.iter(),
            reader,
        ))
    }

    async fn stats(&self) -> Result<MemoryStats, searchos_domain::SearchOsError> {
        Ok(MemoryStats::default())
    }
}

fn context() -> SourceExecutionContext {
    SourceExecutionContext {
        request_id: searchos_domain::ids::RequestId::generate(),
        source_id: SourceId::new(AgentMemorySource::DEFAULT_ID),
        deadline: chrono::Utc::now() + chrono::Duration::seconds(5),
        max_results: 10,
        max_estimated_cost_micros: 0,
        started_at: chrono::Utc::now(),
    }
}

fn request(workspace: &str) -> SourceSearchRequest {
    SourceSearchRequest::new(
        "what did we decide about the index",
        SearchMode::LocalFirst,
        WorkspaceId::new(workspace),
        10,
    )
}

/// Memory registered for ws_a must not leak ws_a's private entries to ws_b,
/// even though the registry resolves the source for the requester.
#[tokio::test]
async fn memory_source_never_leaks_private_entries_across_workspaces() {
    let readers = Arc::new(Mutex::new(Vec::new()));
    let memory: Arc<dyn MemoryService> = Arc::new(GuardedMemory {
        entries: vec![
            entry(
                "private_a",
                ns("ws_a", Some("agent-1")),
                SharingPolicy::Private,
                "ws_a private decision",
            ),
            entry(
                "shared_a",
                ns("ws_a", None),
                SharingPolicy::WorkspaceShared,
                "ws_a shared decision",
            ),
            entry(
                "public",
                ns("ws_b", None),
                SharingPolicy::GlobalPublic,
                "public decision",
            ),
        ],
        readers: readers.clone(),
    });
    let source = AgentMemorySource::new(memory, WorkspaceId::new("ws_a"), 90);

    // ws_a sees its WORKSPACE-shared entry plus the public one. The
    // agent-private entry is deliberately NOT visible: `Private` requires the
    // FULL namespace to match, and this adapter reads with a workspace-level
    // reader (no agent). Federating memory must not silently widen a private
    // agent's notes to the whole workspace.
    let own = source.search(request("ws_a"), context()).await.unwrap();
    let own_refs: Vec<&str> = own
        .candidates
        .iter()
        .map(|c| c.canonical_ref.as_str())
        .collect();
    assert_eq!(own_refs.len(), 2);
    assert!(
        !own_refs.contains(&"searchos-memory://private_a"),
        "agent-private memory must not surface through a workspace-level read"
    );

    // ws_b sees ONLY the public entry — the registration scope is ws_a but
    // the read namespace is the requester's.
    let other = source.search(request("ws_b"), context()).await.unwrap();
    assert_eq!(other.candidates.len(), 1);
    assert_eq!(
        other.candidates[0].canonical_ref,
        "searchos-memory://public"
    );
    assert!(
        !other.candidates.iter().any(|c| c.snippet.contains("ws_a")),
        "private ws_a memory leaked to ws_b"
    );

    // And the reader namespace actually forwarded was the requester's.
    assert_eq!(
        readers.lock().unwrap().clone(),
        vec!["ws_a".to_string(), "ws_b".to_string()]
    );
}

#[tokio::test]
async fn memory_source_is_local_free_and_permission_verified() {
    let memory: Arc<dyn MemoryService> = Arc::new(GuardedMemory {
        entries: vec![entry(
            "e1",
            ns("ws_a", None),
            SharingPolicy::WorkspaceShared,
            "decision",
        )],
        readers: Arc::new(Mutex::new(Vec::new())),
    });
    let source = AgentMemorySource::new(memory, WorkspaceId::new("ws_a"), 90);
    let descriptor = source.descriptor();
    assert_eq!(descriptor.category, SourceCategory::AgentMemory);
    assert!(
        !descriptor.category.is_external(),
        "memory must never be treated as an external source by the mode gate"
    );
    assert!(!descriptor.cost.billable);
    assert!(descriptor.capabilities.permission_filtering);
    assert!(descriptor.capabilities.semantic_search);
    assert_eq!(descriptor.scope, SourceScope::Workspace("ws_a".into()));

    let response = source.search(request("ws_a"), context()).await.unwrap();
    assert_eq!(response.estimated_cost_micros, 0);
    // The service already applied the sharing policy for this reader.
    assert!(response.candidates.iter().all(|c| c.permission_verified));
    // A memory entry has no web host; identity is a stable local URI.
    assert!(response.candidates[0]
        .canonical_ref
        .starts_with("searchos-memory://"));
}

#[tokio::test]
async fn memory_source_is_selected_under_local_only() {
    // The mode permission boundary must allow memory in local_only: it is
    // local, free knowledge, exactly what local-first exists to reuse.
    let registry = searchos_federated::InMemorySourceRegistry::new();
    let memory: Arc<dyn MemoryService> = Arc::new(GuardedMemory {
        entries: Vec::new(),
        readers: Arc::new(Mutex::new(Vec::new())),
    });
    registry
        .register(
            Arc::new(AgentMemorySource::new(memory, WorkspaceId::new("ws_a"), 90))
                as Arc<dyn SearchSource>,
        )
        .unwrap();
    let descriptors = registry.list(
        &searchos_federated::SourceFilter {
            enabled_only: true,
            ..Default::default()
        },
        &WorkspaceId::new("ws_a"),
    );
    let plan = searchos_federated::FederatedPlanner::plan(
        &searchos_federated::PlanningContext {
            query: "q".into(),
            mode: SearchMode::LocalOnly,
            workspace_id: "ws_a".into(),
            requirements: vec!["decision".into()],
            generic_sources_can_answer: true,
            ..Default::default()
        },
        &descriptors,
    );
    assert_eq!(
        plan.selected_source_ids(),
        vec![AgentMemorySource::DEFAULT_ID.to_string()]
    );
}

#[test]
fn memory_resource_permission_follows_the_sharing_policy() {
    let reader = ns("ws_a", Some("agent-1"));

    // Private + same namespace ⇒ verified, agent-scoped.
    let private = entry(
        "p",
        ns("ws_a", Some("agent-1")),
        SharingPolicy::Private,
        "private",
    );
    let resource = memory_resource(&private, &reader);
    assert!(resource.context_eligible(Some("agent-1")));
    assert!(!resource.context_eligible(Some("agent-2")));
    assert!(
        !resource.permission.egress_allowed,
        "memory egress stays forbidden by default (RFC-087)"
    );
    assert!(resource.resource_type.is_user_owned());

    // Private + different agent ⇒ NOT verified: fail-closed.
    let other_reader = ns("ws_a", Some("agent-2"));
    let denied = memory_resource(&private, &other_reader);
    assert!(!denied.context_eligible(Some("agent-2")));

    // WorkspaceShared ⇒ any agent in the workspace.
    let shared = entry(
        "s",
        ns("ws_a", None),
        SharingPolicy::WorkspaceShared,
        "shared",
    );
    let shared_resource = memory_resource(&shared, &reader);
    assert!(shared_resource.context_eligible(Some("agent-2")));

    // Cross-workspace reader ⇒ not verified even for WorkspaceShared.
    let foreign = memory_resource(&shared, &ns("ws_b", None));
    assert!(!foreign.context_eligible(None));
}
