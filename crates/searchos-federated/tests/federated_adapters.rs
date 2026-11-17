//! Phase 2/3 acceptance tests: adapters are behaviour-preserving wrappers.
//!
//! The named gates here:
//!   legacy_pipeline_matches_federated_adapters   — the dependency-review
//!       equivalence gate that MUST pass before any new source is added
//!   adapter_adds_no_filtering_or_reshaping
//!   adapter_preserves_workspace_scoping
//!   provider_adapter_never_bills
//!   provider_domain_policy_matches_local_semantics
//!   registry_drives_a_full_federated_execution

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId, SourceId, WorkspaceId};
use searchos_domain::retrieval::{
    IndexedDocument, RetrievalCandidate, RetrievalQuery, RetrievalSource,
};
use searchos_domain::{SearchMode, SearchOsError};
use searchos_federated::adapters::{domain_matches, LocalIndexSource, WebSearchProviderSource};
use searchos_federated::execute::{ExecutionControls, ExecutionScope, FederatedExecutor};
use searchos_federated::planner::{FederatedPlanner, FederationStrategy, PlanningContext};
use searchos_federated::registry::InMemorySourceRegistry;
use searchos_federated::source::{
    SearchSource, SourceCategory, SourceExecutionContext, SourceExecutionStatus,
    SourceLatencyProfile, SourceScope, SourceSearchRequest,
};
use searchos_provider::error::ProviderError;
use searchos_provider::traits::{ProviderResult, SearchParams, SearchProvider, SearchResultEntry};
use searchos_retrieval::SearchIndex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// A recording fake index: the same port the runtime uses for BM25.
// ---------------------------------------------------------------------------

/// A deterministic index double.
///
/// `base` is captured once so two searches in the same test return byte
/// identical timestamps: the equivalence gate must compare mapped output,
/// and a per-call `Utc::now()` would turn a clock tick into a false failure.
struct FakeIndex {
    calls: AtomicUsize,
    last_query: std::sync::Mutex<Option<RetrievalQuery>>,
    base: DateTime<Utc>,
}

impl Default for FakeIndex {
    fn default() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            last_query: std::sync::Mutex::new(None),
            base: Utc::now(),
        }
    }
}

#[async_trait]
impl SearchIndex for FakeIndex {
    async fn index(&self, _document: IndexedDocument) -> Result<(), SearchOsError> {
        Ok(())
    }

    async fn search(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_query.lock().unwrap() = Some(query.clone());
        let now = self.base;
        Ok(vec![
            candidate("fresh-1", "https://a.dev/fresh1", "fresh one", 9.0, now),
            candidate(
                "stale-1",
                "https://a.dev/stale1",
                "stale one",
                8.0,
                now - Duration::days(40),
            ),
            candidate(
                "other-ws",
                "https://b.dev/private",
                "other workspace",
                1.0,
                now,
            ),
        ])
    }

    async fn delete(&self, _document_id: &DocumentId) -> Result<(), SearchOsError> {
        Ok(())
    }

    async fn commit(&self) -> Result<(), SearchOsError> {
        Ok(())
    }

    async fn count(&self) -> Result<u64, SearchOsError> {
        Ok(3)
    }

    async fn rebuild_from(&self, _documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        Ok(0)
    }
}

fn candidate(
    chunk: &str,
    url: &str,
    snippet: &str,
    score: f32,
    fetched_at: DateTime<Utc>,
) -> RetrievalCandidate {
    RetrievalCandidate {
        chunk_id: ChunkId::new(chunk),
        document_id: DocumentId::new("doc_1"),
        version_id: DocumentVersionId::new("docv_1"),
        title: Some(snippet.to_string()),
        snippet: snippet.into(),
        url: url.into(),
        score,
        source: RetrievalSource::Bm25,
        fetched_at,
        vector_score: None,
        bm25_score: Some(score),
        workspace_id: None,
        language: Some("en".into()),
        highlight: None,
    }
}

fn federated_request() -> SourceSearchRequest {
    SourceSearchRequest::new(
        "rust edition 2024",
        SearchMode::LocalFirst,
        WorkspaceId::new("ws_a"),
        10,
    )
}

// ---------------------------------------------------------------------------
// Equivalence gate
// ---------------------------------------------------------------------------

/// The gate from `docs/rfc-062-dependency-review.md` §6: retrieval through
/// the adapter must be identical to retrieval through the legacy local path.
#[tokio::test]
async fn legacy_pipeline_matches_federated_adapters() {
    let index = Arc::new(FakeIndex::default());

    // Legacy path, exactly as `search_service.rs` builds it today:
    //   RetrievalQuery::new(normalized, top_k)
    //   + workspace_id (requester; the index adds the global `*` namespace)
    //   + language / domain policy from constraints
    //   + include_stale = true   (INV-30-01)
    //   + fetched_after from the freshness requirement
    let top_k = 10;
    let mut legacy = RetrievalQuery::new("rust edition 2024", top_k);
    legacy.workspace_id = Some("ws_a".to_string());
    legacy.language = Some("en".into());
    legacy.required_domains = vec!["a.dev".into()];
    legacy.excluded_domains = vec!["b.dev".into()];
    legacy.include_stale = true;
    let cutoff = Utc::now() - Duration::days(30);
    legacy.fetched_after = Some(cutoff);
    let legacy_hits = index.search(&legacy).await.expect("legacy search");

    // Federated path through the adapter, fed the same policy.
    let source = LocalIndexSource::new(index.clone(), SourceScope::Workspace("ws_a".into()), 10);
    let mut request = federated_request();
    request.required_domains = vec!["a.dev".into()];
    request.excluded_domains = vec!["b.dev".into()];
    request.fetched_after = Some(cutoff);
    let response = source
        .search(request.clone(), exec_context(&source))
        .await
        .expect("adapter search");

    // The query the adapter handed the index must equal the legacy query on
    // every field that carries policy. Language is the one field the
    // federated request does not model (the port has no language policy), so
    // the adapter must leave it at the index default rather than invent one.
    let issued = index
        .last_query
        .lock()
        .unwrap()
        .clone()
        .expect("query issued");
    assert_eq!(issued.text, legacy.text);
    assert_eq!(issued.top_k, legacy.top_k);
    assert_eq!(issued.workspace_id, legacy.workspace_id);
    assert_eq!(issued.required_domains, legacy.required_domains);
    assert_eq!(issued.excluded_domains, legacy.excluded_domains);
    assert_eq!(issued.fetched_after, legacy.fetched_after);
    assert!(
        issued.include_stale,
        "INV-30-01: stale rows stay in the pool; the service decides what is servable"
    );
    assert!(
        issued.language.is_none(),
        "the adapter must not invent a language policy the request did not carry"
    );

    // Same index + same query ⇒ the adapter cannot add or remove rows.
    assert_eq!(
        response.candidates.len(),
        legacy_hits.len(),
        "the adapter must not filter on its own"
    );
    for (mapped, legacy_hit) in response.candidates.iter().zip(legacy_hits.iter()) {
        assert_eq!(mapped.canonical_ref, legacy_hit.url);
        assert_eq!(mapped.snippet, legacy_hit.snippet);
        assert_eq!(mapped.raw_score, Some(legacy_hit.score));
        assert_eq!(mapped.observed_at, legacy_hit.fetched_at);
    }
    assert_eq!(response.status, SourceExecutionStatus::Success);
    assert_eq!(
        response.estimated_cost_micros, 0,
        "local retrieval is free and must never look billable"
    );
    assert!(!source.descriptor().cost.billable);
}

fn exec_context(source: &LocalIndexSource) -> SourceExecutionContext {
    SourceExecutionContext {
        request_id: searchos_domain::ids::RequestId::generate(),
        source_id: source.descriptor().source_id,
        deadline: Utc::now() + Duration::seconds(5),
        max_results: 10,
        max_estimated_cost_micros: 1_000_000,
        started_at: Utc::now(),
    }
}

// ---------------------------------------------------------------------------
// Adapter adds no policy of its own
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adapter_adds_no_filtering_or_reshaping() {
    let index = Arc::new(FakeIndex::default());
    let source = LocalIndexSource::new(index.clone(), SourceScope::Global, 10);
    let response = source
        .search(federated_request(), exec_context(&source))
        .await
        .unwrap();
    assert_eq!(response.candidates.len(), 3);
    assert_eq!(response.source_id, source.descriptor().source_id);
    // Every candidate carries the source's identity, so dedup can attribute
    // an occurrence to the source that discovered it.
    assert!(response
        .candidates
        .iter()
        .all(|c| c.source_id == source.descriptor().source_id));
    // And the adapter ran the query through the shared index once.
    assert_eq!(index.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn adapter_preserves_workspace_scoping() {
    let index = Arc::new(FakeIndex::default());
    let source = LocalIndexSource::new(index.clone(), SourceScope::Workspace("ws_a".into()), 10);
    let mut request = federated_request();
    request.workspace_id = WorkspaceId::new("ws_b");
    source.search(request, exec_context(&source)).await.unwrap();
    let issued = index.last_query.lock().unwrap().clone().unwrap();
    assert_eq!(
        issued.workspace_id.as_deref(),
        Some("ws_b"),
        "the requester's workspace is what reaches the index"
    );
}

#[test]
fn adapter_declares_only_capabilities_the_index_provides() {
    let source = LocalIndexSource::new(Arc::new(FakeIndex::default()), SourceScope::Global, 10);
    let descriptor = source.descriptor();
    assert!(descriptor.capabilities.lexical_search);
    assert!(
        !descriptor.capabilities.semantic_search,
        "the BM25 port cannot do vector lookup; hybrid retrieval stays a separate stage"
    );
    assert!(descriptor.capabilities.metadata_filter);
    assert!(descriptor.capabilities.permission_filtering);
    assert!(!descriptor.capabilities.full_document_fetch);
    assert!(!descriptor.capabilities.incremental_sync);
    assert!(!descriptor.category.is_external());
    assert_eq!(descriptor.category, SourceCategory::LocalKnowledge);
    assert!(!descriptor.cost.billable);
}

// ---------------------------------------------------------------------------
// Provider adapter
// ---------------------------------------------------------------------------

fn entry(url: &str, score: f32) -> SearchResultEntry {
    SearchResultEntry {
        title: Some("t".into()),
        url: url.into(),
        content: "c".into(),
        score,
        published_at: None,
        raw_content: None,
    }
}

fn provider_source(provider: Arc<dyn SearchProvider>) -> WebSearchProviderSource {
    WebSearchProviderSource::new(
        provider,
        SourceScope::Global,
        5,
        2_000,
        SourceLatencyProfile {
            p50_ms: 200,
            p95_ms: 1_500,
            timeout_ms: 3_000,
        },
    )
}

/// Shared counters so assertions survive the `Arc<dyn>` erasure.
struct CountingProvider {
    id: &'static str,
    fail: Option<ProviderError>,
    results: Vec<SearchResultEntry>,
    calls: Arc<AtomicUsize>,
    last_params: Arc<std::sync::Mutex<Option<SearchParams>>>,
    unhealthy: bool,
}

impl CountingProvider {
    fn new(
        id: &'static str,
        fail: Option<ProviderError>,
        results: Vec<SearchResultEntry>,
        unhealthy: bool,
    ) -> (
        Arc<Self>,
        Arc<AtomicUsize>,
        Arc<std::sync::Mutex<Option<SearchParams>>>,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(std::sync::Mutex::new(None));
        (
            Arc::new(Self {
                id,
                fail,
                results,
                calls: Arc::clone(&calls),
                last_params: Arc::clone(&last),
                unhealthy,
            }),
            calls,
            last,
        )
    }
}

#[async_trait]
impl SearchProvider for CountingProvider {
    fn id(&self) -> &str {
        self.id
    }
    fn capabilities(&self) -> searchos_domain::provider::ProviderCapabilities {
        searchos_domain::provider::ProviderCapabilities {
            search: true,
            extract: true,
            supports_domain_filter: false,
            supports_date_filter: true,
            ..Default::default()
        }
    }
    async fn search(&self, params: &SearchParams) -> Result<ProviderResult, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_params.lock().unwrap() = Some(params.clone());
        match &self.fail {
            Some(error) => Err(error.clone()),
            None => Ok(ProviderResult {
                entries: self.results.clone(),
                duration_ms: 12,
                bytes_transferred: 1_024,
            }),
        }
    }
    async fn health_check(&self) -> Result<bool, ProviderError> {
        if self.unhealthy {
            Err(ProviderError {
                provider_id: self.id.to_string(),
                kind: searchos_provider::error::ProviderErrorKind::ProviderUnavailable,
                message: "offline".into(),
                retry_after_seconds: None,
                status: Some(503),
            })
        } else {
            Ok(true)
        }
    }
}

#[tokio::test]
async fn provider_adapter_never_bills_itself() {
    let (provider, _calls, _last) =
        CountingProvider::new("tavily", None, vec![entry("https://x.dev/1", 0.9)], false);
    let source = provider_source(provider);
    let response = source
        .search(federated_request(), exec_context_dummy())
        .await
        .unwrap();
    // It reports an ESTIMATE for planning; it does not settle anything.
    assert_eq!(response.estimated_cost_micros, 2_000);
    assert!(source.descriptor().cost.billable);
    // And the estimate is an estimate: no actual-cost field exists on the
    // response at all, so there is nothing that could be mistaken for a bill.
    let json = serde_json::to_value(&response).unwrap();
    assert!(json.get("actual_cost").is_none());
    assert!(json.get("settled_cost").is_none());
}

fn exec_context_dummy() -> SourceExecutionContext {
    SourceExecutionContext {
        request_id: searchos_domain::ids::RequestId::generate(),
        source_id: SourceId::new("tavily"),
        deadline: Utc::now() + Duration::seconds(5),
        max_results: 10,
        max_estimated_cost_micros: 1_000_000,
        started_at: Utc::now(),
    }
}

#[tokio::test]
async fn provider_domain_policy_matches_local_semantics() {
    let (provider, _calls, _last) = CountingProvider::new(
        "tavily",
        None,
        vec![
            entry("https://docs.rs/std", 0.9),
            entry("https://blog.example.com/post", 0.8),
            entry("https://other.dev/page", 0.7),
        ],
        false,
    );
    let source = provider_source(provider);
    let mut request = federated_request();
    request.required_domains = vec!["docs.rs".into()];
    request.excluded_domains = vec!["other.dev".into()];
    let response = source.search(request, exec_context_dummy()).await.unwrap();
    assert_eq!(
        response
            .candidates
            .iter()
            .map(|c| c.canonical_ref.as_str())
            .collect::<Vec<_>>(),
        vec!["https://docs.rs/std"],
        "subdomain match and exclusion behave like `candidate_allowed`"
    );
    // Same helper, same rule: the local and external paths cannot disagree.
    assert!(domain_matches("docs.rs", "docs.rs"));
    assert!(domain_matches("api.docs.rs", "docs.rs"));
    assert!(!domain_matches("notdocs.rs", "docs.rs"));

    // Permission is NOT this source's job: external candidates arrive
    // unverified so the assembler must not trust them implicitly.
    assert!(response.candidates.iter().all(|c| !c.permission_verified));
}

#[tokio::test]
async fn provider_freshness_and_language_map_to_params() {
    let (provider, calls, last) = CountingProvider::new("tavily", None, Vec::new(), false);
    let source = provider_source(provider);
    let mut request = federated_request();
    request.max_results = 7;
    request.fetched_after = Some(Utc::now() - Duration::days(30));
    let response = source.search(request, exec_context_dummy()).await.unwrap();
    assert_eq!(response.status, SourceExecutionStatus::Success);
    assert_eq!(
        response.candidates.len(),
        0,
        "empty is a success, not a failure"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let params = last.lock().unwrap().clone().expect("params sent");
    assert_eq!(params.max_results, 7);
    assert!(
        (15..=31).contains(&params.freshness_days.unwrap()),
        "freshness cutoff {} maps to a day count",
        params.freshness_days.unwrap()
    );
}

#[tokio::test]
async fn provider_errors_map_to_distinct_execution_statuses() {
    let cases = [
        (
            searchos_provider::error::ProviderErrorKind::Timeout,
            SourceExecutionStatus::Timeout,
        ),
        (
            searchos_provider::error::ProviderErrorKind::RateLimit,
            SourceExecutionStatus::RateLimited,
        ),
        (
            searchos_provider::error::ProviderErrorKind::QuotaExhausted,
            SourceExecutionStatus::RateLimited,
        ),
        (
            searchos_provider::error::ProviderErrorKind::ProviderUnavailable,
            SourceExecutionStatus::Unavailable,
        ),
        (
            searchos_provider::error::ProviderErrorKind::AuthenticationError,
            SourceExecutionStatus::PermissionDenied,
        ),
        (
            searchos_provider::error::ProviderErrorKind::InvalidResponse,
            SourceExecutionStatus::Failed,
        ),
    ];
    for (kind, expected) in cases {
        let label = format!("{kind:?}");
        let (provider, _calls, _last) = CountingProvider::new(
            "tavily",
            Some(ProviderError {
                provider_id: "tavily".into(),
                kind: kind.clone(),
                message: "boom".into(),
                retry_after_seconds: None,
                status: Some(500),
            }),
            Vec::new(),
            false,
        );
        let source = provider_source(provider);
        let error = source
            .search(federated_request(), exec_context_dummy())
            .await
            .expect_err("provider failed");
        assert_eq!(error.status(), expected, "{label}");
    }
}

#[tokio::test]
async fn provider_health_probe_is_reported_not_swallowed() {
    let (healthy, _, _) = CountingProvider::new("tavily", None, Vec::new(), false);
    let healthy = provider_source(healthy);
    let (down, _, _) = CountingProvider::new("brave", None, Vec::new(), true);
    let down = provider_source(down);
    assert_eq!(
        healthy.health_check().await,
        searchos_federated::source::SourceHealth::Healthy
    );
    assert_eq!(
        down.health_check().await,
        searchos_federated::source::SourceHealth::Down,
        "health is bookkeeping: a failed probe is reported, not turned into a request error"
    );
}

// ---------------------------------------------------------------------------
// End-to-end: registry → planner → executor → adapters
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_drives_a_full_federated_execution() {
    let registry = InMemorySourceRegistry::new();
    let local = Arc::new(LocalIndexSource::new(
        Arc::new(FakeIndex::default()),
        SourceScope::Workspace("ws_a".into()),
        10,
    ));
    let (provider, provider_calls, _) = CountingProvider::new(
        "tavily",
        None,
        vec![entry("https://official.dev/release", 0.95)],
        false,
    );
    // Wiring declares what each source can answer; the planner will not
    // spend money on a source that fills no open requirement.
    let tavily_source = provider_source(provider).with_satisfies_requirements(["answer"]);
    registry.register(local).unwrap();
    registry
        .register(Arc::new(tavily_source) as Arc<dyn SearchSource>)
        .unwrap();

    let descriptors = registry.list(
        &searchos_federated::source::SourceFilter {
            enabled_only: true,
            ..Default::default()
        },
        &WorkspaceId::new("ws_a"),
    );
    assert_eq!(descriptors.len(), 2);

    // local_only: the provider must never be reached — this is the same
    // guarantee at runtime that the planner test asserts at plan time.
    let plan = FederatedPlanner::plan(
        &PlanningContext {
            query: "rust edition 2024".into(),
            mode: SearchMode::LocalOnly,
            strategy: FederationStrategy::Auto,
            workspace_id: "ws_a".into(),
            requirements: vec!["answer".into()],
            covered_requirements: Vec::new(),
            required_requirements: Vec::new(),
            max_sources: 4,
            max_parallel_sources: 4,
            max_estimated_cost_micros: 100_000,
            max_latency_ms: 30_000,
            stop_when_covered: true,
            generic_sources_can_answer: false,
            selection: searchos_federated::SelectionMode::default(),
        },
        &descriptors,
    );
    assert_eq!(
        plan.selected_source_ids(),
        vec!["local_knowledge".to_string()]
    );
    let request = SourceSearchRequest::new(
        "rust edition 2024",
        SearchMode::LocalOnly,
        WorkspaceId::new("ws_a"),
        5,
    );
    let report = FederatedExecutor::new(
        &registry,
        ExecutionScope {
            request_id: searchos_domain::ids::RequestId::generate(),
            workspace_id: WorkspaceId::new("ws_a"),
            max_parallel_sources: 4,
        },
        ExecutionControls::new(),
    )
    .execute(&plan, &request)
    .await;
    assert_eq!(report.executed_count(), 1);
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    assert_eq!(report.results[0].status, SourceExecutionStatus::Success);

    // Hybrid with an open requirement and no early stop: both run, local
    // planned first (P-002 dominates authority and priority).
    let hybrid_ctx = PlanningContext {
        query: "rust edition 2024".into(),
        mode: SearchMode::Hybrid,
        strategy: FederationStrategy::Auto,
        workspace_id: "ws_a".into(),
        // One requirement is local knowledge, one is current web fact. The
        // local adapter remains generic/free, while the provider adapter
        // carries the runtime-declared requirement id so the plan cannot
        // mistake it for a duplicate of the local gap.
        requirements: vec!["answer".into(), "current".into()],
        covered_requirements: Vec::new(),
        required_requirements: Vec::new(),
        max_sources: 4,
        max_parallel_sources: 4,
        max_estimated_cost_micros: 100_000,
        max_latency_ms: 30_000,
        stop_when_covered: false,
        generic_sources_can_answer: true,
        selection: searchos_federated::SelectionMode::default(),
    };
    let plan = FederatedPlanner::plan(&hybrid_ctx, &descriptors);
    // The executed request carries the SAME mode as the plan: a plan built
    // for Hybrid must never be run with a LocalOnly request, or the
    // permission boundary the plan reasoned about would be a different one.
    let hybrid_request = SourceSearchRequest::new(
        "rust edition 2024",
        SearchMode::Hybrid,
        WorkspaceId::new("ws_a"),
        5,
    );
    let report = FederatedExecutor::new(
        &registry,
        ExecutionScope {
            request_id: searchos_domain::ids::RequestId::generate(),
            workspace_id: WorkspaceId::new("ws_a"),
            max_parallel_sources: 4,
        },
        ExecutionControls::new(),
    )
    .execute(&plan, &hybrid_request)
    .await;
    assert_eq!(report.executed_count(), 2);
    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert!(report.failed_source_ids().is_empty());

    // The aggregation boundary is the same one the quality gate reads.
    let outcome = searchos_federated::planner::aggregate(&report.responses(), &plan);
    assert!(!outcome.required_failure);
    assert_eq!(outcome.candidates.len(), 4, "3 local + 1 provider");
}

#[tokio::test]
async fn unregistered_source_in_plan_surfaces_as_unavailable() {
    let registry = InMemorySourceRegistry::new();
    registry
        .register(Arc::new(LocalIndexSource::new(
            Arc::new(FakeIndex::default()),
            SourceScope::Global,
            10,
        )))
        .unwrap();
    let descriptors = vec![searchos_federated::source::SearchSourceDescriptor {
        source_id: SourceId::new("deleted_source"),
        ..local_descriptor_shape()
    }];
    let plan = FederatedPlanner::plan(
        &PlanningContext {
            query: "q".into(),
            mode: SearchMode::LocalFirst,
            strategy: FederationStrategy::Auto,
            workspace_id: "ws_a".into(),
            requirements: Vec::new(),
            covered_requirements: Vec::new(),
            required_requirements: Vec::new(),
            max_sources: 2,
            max_parallel_sources: 2,
            max_estimated_cost_micros: 100_000,
            max_latency_ms: 30_000,
            stop_when_covered: false,
            generic_sources_can_answer: false,
            selection: searchos_federated::SelectionMode::default(),
        },
        &descriptors,
    );
    let report = FederatedExecutor::new(
        &registry,
        ExecutionScope {
            request_id: searchos_domain::ids::RequestId::generate(),
            workspace_id: WorkspaceId::new("ws_a"),
            max_parallel_sources: 2,
        },
        ExecutionControls::new(),
    )
    .execute(&plan, &federated_request())
    .await;
    // A stale plan is not a crash: the missing source is reported.
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].status, SourceExecutionStatus::Unavailable);
    assert_eq!(
        report.failed_source_ids(),
        vec!["deleted_source".to_string()]
    );
}

fn local_descriptor_shape() -> searchos_federated::source::SearchSourceDescriptor {
    let source = LocalIndexSource::new(Arc::new(FakeIndex::default()), SourceScope::Global, 10);
    source.descriptor()
}

/// Locks the claim in `adapters`: the local and external paths apply ONE
/// domain policy, not two that happen to agree today. Both host extractors
/// and both match rules are compared on the same corpus.
#[test]
fn local_and_external_domain_policy_agree() {
    let corpus = [
        "https://docs.rs/std",
        "https://api.docs.rs/v1",
        "https://sub.a-dev.io:8443/x",
        "https://notdocs.rs/x",
        "http://a-dev.io",
        "https://a-dev.io/x#frag",
        "https://a-dev.io/",
        "not-a-url-host/x",
        "https://a-dev.io/x?q=1",
        "",
    ];
    for url in corpus {
        assert_eq!(
            searchos_federated::adapters::host_of(url),
            searchos_retrieval::host_of(url),
            "host extraction diverged for {url:?}"
        );
    }
    // And the match rule is the same predicate the local filter uses
    // (`h == d || h.ends_with(".d")`), applied to the same host set.
    let domains = ["docs.rs", "a-dev.io", "example.com"];
    let local_rule = |host: &str, d: &str| host == d || host.ends_with(&format!(".{d}"));
    for url in corpus {
        let Some(host) = searchos_federated::adapters::host_of(url) else {
            continue;
        };
        for domain in domains {
            assert_eq!(
                domain_matches(&host, domain),
                local_rule(&host, domain),
                "domain policy diverged for {url:?} / {domain:?}"
            );
        }
    }
}
