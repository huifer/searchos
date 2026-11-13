//! Agent session transcripts as a federated search source (RFC-062 Phase 6,
//! RFC-069 residue: "session/transcript ingestion").
//!
//! Past research sessions are retrievable knowledge: a follow-up turn should
//! never silently re-buy a question the same workspace already asked. The
//! `SessionStore` already scopes every session to one workspace, so this
//! adapter is a thin translation, not a second store.
//!
//! It lives in the application layer on purpose (same rationale as
//! `memory_source`): the federated kernel stays free of storage/service
//! dependencies, and workspace isolation stays owned by the session store —
//! this adapter filters by the session's own `workspace_id` and never reads
//! across workspaces.

use async_trait::async_trait;
use searchos_domain::ids::{SourceId, WorkspaceId};
use searchos_federated::source::{
    SearchSource, SearchSourceDescriptor, SourceCandidate, SourceCapabilities, SourceCategory,
    SourceCostProfile, SourceError, SourceExecutionContext, SourceExecutionStatus,
    SourceLatencyProfile, SourceScope, SourceSearchRequest, SourceSearchResponse,
};
use std::sync::Arc;

use crate::session::SessionStore;

/// Agent session transcripts wrapped as a federated source.
pub struct SessionTranscriptSource {
    descriptor: SearchSourceDescriptor,
    sessions: Arc<SessionStore>,
}

impl SessionTranscriptSource {
    /// Source id used when the caller does not name one.
    pub const DEFAULT_ID: &'static str = "session_transcripts";

    /// Registers the transcript source for one workspace. The scope mirrors
    /// the filter the search applies, so a workspace can never resolve
    /// another workspace's session source through scope precedence.
    pub fn new(sessions: Arc<SessionStore>, workspace: WorkspaceId, priority: i32) -> Self {
        Self::with_id(Self::DEFAULT_ID, sessions, workspace, priority)
    }

    pub fn with_id(
        id: &str,
        sessions: Arc<SessionStore>,
        workspace: WorkspaceId,
        priority: i32,
    ) -> Self {
        Self {
            descriptor: SearchSourceDescriptor {
                source_id: SourceId::new(id),
                name: "agent session transcripts".into(),
                category: SourceCategory::Conversation,
                version: "1".into(),
                enabled: true,
                priority,
                capabilities: SourceCapabilities {
                    lexical_search: true,
                    // Turn text is matched lexically; the session store keeps
                    // no embeddings, so no semantic claim is made here.
                    semantic_search: false,
                    // Workspace isolation is enforced by this adapter's
                    // workspace filter, which is exactly what
                    // `permission_filtering` promises.
                    permission_filtering: true,
                    provenance_metadata: true,
                    freshness_metadata: true,
                    ..Default::default()
                },
                reliability: Default::default(),
                cost: SourceCostProfile {
                    estimated_cost_micros: 0,
                    currency: "USD".into(),
                    // Local transcripts never produce an external bill.
                    billable: false,
                },
                latency: SourceLatencyProfile {
                    p50_ms: 2,
                    p95_ms: 20,
                    timeout_ms: 1_000,
                },
                scope: SourceScope::Workspace(workspace.to_string()),
                satisfies_requirements: Vec::new(),
            },
            sessions,
        }
    }
}

/// One query token that participates in matching (lowercased, length > 2).
fn query_tokens(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() > 2)
        .map(|t| t.to_string())
        .collect()
}

#[async_trait]
impl SearchSource for SessionTranscriptSource {
    fn descriptor(&self) -> SearchSourceDescriptor {
        self.descriptor.clone()
    }

    async fn search(
        &self,
        request: SourceSearchRequest,
        _context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, SourceError> {
        let started = std::time::Instant::now();
        let tokens = query_tokens(&request.query);

        let mut candidates: Vec<SourceCandidate> = Vec::new();
        // The store is process-local and bounded; linear scan is the whole
        // retrieval here. Only THIS requester's sessions are ever read.
        for session in self.sessions.list() {
            if session.workspace_id != request.workspace_id.to_string() {
                continue;
            }
            for (turn_index, turn) in session.turns.iter().enumerate() {
                let turn_lower = turn.query.to_lowercase();
                let matched = tokens
                    .iter()
                    .filter(|t| turn_lower.contains(t.as_str()))
                    .count();
                if tokens.is_empty() || matched == 0 {
                    continue;
                }
                let score = (matched as f32 / tokens.len() as f32).clamp(0.05, 1.0);
                candidates.push(SourceCandidate {
                    source_id: self.descriptor.source_id.clone(),
                    canonical_ref: format!(
                        "searchos-session://{}/{}",
                        session.session_id, turn_index
                    ),
                    title: Some(turn.query.clone()),
                    snippet: format!(
                        "research turn {}: asked \"{}\" via {} (status {})",
                        turn_index + 1,
                        turn.query,
                        turn.decision_path,
                        turn.status
                    ),
                    raw_score: Some(score),
                    observed_at: chrono::DateTime::parse_from_rfc3339(&turn.recorded_at)
                        .map(|t| t.with_timezone(&chrono::Utc))
                        .unwrap_or_else(|_| chrono::Utc::now()),
                    published_at: None,
                    satisfies_requirements: Vec::new(),
                    // The workspace filter above IS the permission check.
                    permission_verified: true,
                });
            }
        }

        // Best match ratio first, then oldest turn (deterministic).
        candidates.sort_by(|a, b| {
            b.raw_score
                .partial_cmp(&a.raw_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.canonical_ref.cmp(&b.canonical_ref))
        });
        candidates.truncate(request.max_results.max(1));

        let metrics = searchos_observability::MetricsRegistry::global();
        metrics.inc_counter("federation.sessions.searched_total");
        for _ in &candidates {
            metrics.inc_counter("federation.sessions.hits_total");
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

/// Maps a session turn onto the unified resource model (RFC-064).
///
/// A transcript is workspace-owned knowledge: not age-stale, no egress, and
/// its permission is the workspace boundary itself (no agent allow-list — a
/// session is visible to every agent in the owning workspace).
pub fn transcript_resource(
    session_id: &str,
    workspace_id: &str,
    turn_index: usize,
    query: &str,
) -> searchos_federated::SearchResource {
    use searchos_federated::{PermissionRef, ResourceType};

    let mut resource = searchos_federated::SearchResource::new(
        ResourceType::Transcript,
        &searchos_domain::ids::WorkspaceId::new(workspace_id),
        format!("searchos-session://{session_id}/{turn_index}"),
    )
    .with_title(query.to_string());
    resource.permission = PermissionRef {
        workspace_id: workspace_id.to_string(),
        allowed_agents: Vec::new(),
        egress_allowed: false,
        verified: true,
    };
    resource
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionLimits, SessionTurn};

    fn turn(query: &str, at: &str) -> SessionTurn {
        SessionTurn {
            query: query.into(),
            trace_id: searchos_domain::ids::TraceId::generate(),
            status: "completed".into(),
            decision_path: "local".into(),
            open_requirements: 0,
            recorded_at: at.into(),
        }
    }

    fn store_with(ws_a: &[(&str, &str)], ws_b: &[(&str, &str)]) -> Arc<SessionStore> {
        let store = Arc::new(SessionStore::new(SessionLimits {
            max_sessions: 64,
            max_turns_per_session: 20,
        }));
        for (sid, q) in ws_a {
            store.create_or_get(sid, "ws_a");
            store.record_turn(sid, "ws_a", turn(q, "2026-11-13T00:00:00Z"), Vec::new());
        }
        for (sid, q) in ws_b {
            store.create_or_get(sid, "ws_b");
            store.record_turn(sid, "ws_b", turn(q, "2026-11-13T00:00:00Z"), Vec::new());
        }
        store
    }

    fn source_for(store: Arc<SessionStore>) -> SessionTranscriptSource {
        SessionTranscriptSource::new(store, searchos_domain::ids::WorkspaceId::new("ws_a"), 80)
    }

    fn request(q: &str) -> SourceSearchRequest {
        SourceSearchRequest::new(
            q.to_string(),
            searchos_domain::SearchMode::LocalOnly,
            searchos_domain::ids::WorkspaceId::new("ws_a"),
            5,
        )
    }

    fn context() -> SourceExecutionContext {
        SourceExecutionContext {
            request_id: searchos_domain::ids::RequestId::generate(),
            source_id: SourceId::new(SessionTranscriptSource::DEFAULT_ID),
            deadline: chrono::Utc::now() + chrono::Duration::seconds(5),
            max_results: 10,
            max_estimated_cost_micros: 0,
            started_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn only_the_requesters_sessions_are_visible() {
        let store = store_with(
            &[("s_a1", "tokio async runtime internals")],
            &[("s_b1", "tokio async runtime internals")],
        );
        let source = source_for(store);
        let resp = source
            .search(request("tokio runtime"), context())
            .await
            .unwrap();
        assert!(!resp.candidates.is_empty());
        assert!(resp
            .candidates
            .iter()
            .all(|c| c.canonical_ref.starts_with("searchos-session://s_a1/")));
    }

    #[tokio::test]
    async fn matched_turns_rank_above_partial_matches() {
        let store = store_with(&[("s1", "tokio async runtime"), ("s2", "tokio")], &[]);
        let source = source_for(store);
        let resp = source
            .search(request("tokio async runtime internals"), context())
            .await
            .unwrap();
        assert_eq!(resp.candidates[0].canonical_ref, "searchos-session://s1/0");
        // 1 of 4 tokens matched on the weaker session.
        assert!(resp.candidates[1].raw_score.unwrap() < resp.candidates[0].raw_score.unwrap());
    }

    #[tokio::test]
    async fn unmatched_queries_return_no_candidates() {
        let store = store_with(&[("s1", "postgres indexing")], &[]);
        let source = source_for(store);
        let resp = source
            .search(request("kubernetes ingress"), context())
            .await
            .unwrap();
        assert!(resp.candidates.is_empty());
        assert_eq!(resp.status, SourceExecutionStatus::Success);
    }

    #[tokio::test]
    async fn results_respect_max_results() {
        let store = store_with(
            &[("s1", "tokio runtime"), ("s2", "tokio runtime again")],
            &[],
        );
        let source = source_for(store);
        let mut req = request("tokio runtime");
        req.max_results = 1;
        let resp = source.search(req, context()).await.unwrap();
        assert_eq!(resp.candidates.len(), 1);
    }

    #[test]
    fn transcript_resource_maps_onto_the_unified_model() {
        let r = transcript_resource("s1", "ws_a", 0, "tokio runtime");
        assert_eq!(
            r.resource_type,
            searchos_federated::ResourceType::Transcript
        );
        assert_eq!(r.workspace_id, "ws_a");
        assert_eq!(r.canonical_ref.as_deref(), Some("searchos-session://s1/0"));
        // Fail-closed posture: transcript knowledge never egresses.
        assert!(!r.permission.egress_allowed);
        assert!(r.permission.verified);
    }
}
