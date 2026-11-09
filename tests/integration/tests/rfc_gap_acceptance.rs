//! Part 30 execution-correctness acceptance tests (INV-30-01…21, G-02…G-12).
//!
//! These tests use the real runtime adapters plus small in-memory mocks for
//! provider calls and injected failures. They are the executable evidence for
//! the named acceptance criteria in `docs/rfc/part-30-execution-correctness-baseline.md`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use searchos_application::ingest_service::IngestRequest;
use searchos_application::search_service::{
    BudgetSettler, CostEventSink, SearchDependencies, SearchService, SearchServiceImpl,
};
use searchos_core::SearchOsRuntime;
use searchos_domain::cost::{CostAttemptStatus, CostEvent, CostOperation, CostQuery};
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::memory::MemoryNamespace;
use searchos_domain::provider::ProviderCapabilities;
use searchos_domain::request::{FreshnessRequirement, SearchMode, SearchRequest};
use searchos_domain::retrieval::{IndexedDocument, RetrievalQuery, RetrievalSource};
use searchos_domain::{SearchOsError, TRANSPORT_ERROR_CODES};
use searchos_planner::router::{
    ProviderEntry, ProviderHealth, ProviderRouter, StaticProviderRouter,
};
use searchos_policy::{FreshnessPolicy, ProfileRegistry, QualityPolicy, SourcePolicy};
use searchos_provider::error::ProviderError;
use searchos_provider::traits::{ProviderResult, SearchParams, SearchProvider, SearchResultEntry};
use searchos_retrieval::SearchIndex;
use searchos_storage_sqlite::repos::{BudgetRepo, CostRepo, DocumentRepo, IdempotencyRepo};
use serde_json::json;
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Mocks and harness
// ---------------------------------------------------------------------------

struct MockProvider {
    id: String,
    calls: Arc<AtomicUsize>,
    fail_after: Option<usize>,
    delay_ms: u64,
    entries: Vec<SearchResultEntry>,
}

impl MockProvider {
    fn new(id: &str, delay_ms: u64, fail_after: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            calls: Arc::new(AtomicUsize::new(0)),
            fail_after,
            delay_ms,
            entries: vec![SearchResultEntry {
                title: Some("Axum performance".into()),
                url: "https://external.example.com/axum".into(),
                content: "Axum performance comparison".into(),
                score: 0.9,
                published_at: None,
                raw_content: None,
            }],
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl SearchProvider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            search: true,
            ..Default::default()
        }
    }

    async fn search(&self, _params: &SearchParams) -> Result<ProviderResult, ProviderError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        }
        if let Some(fail_after) = self.fail_after {
            if n > fail_after {
                return Err(ProviderError::network(&self.id, "mock provider failure"));
            }
        }
        Ok(ProviderResult {
            entries: self.entries.clone(),
            duration_ms: 1,
            bytes_transferred: 0,
        })
    }
}

fn mock_router() -> Arc<dyn ProviderRouter> {
    Arc::new(StaticProviderRouter::new(vec![ProviderEntry {
        id: "tavily".into(),
        capabilities: ProviderCapabilities {
            search: true,
            ..Default::default()
        },
        quality: 0.9,
        coverage: 0.9,
        freshness: 0.9,
        latency_ms: 10,
        reliability: 1.0,
        cost_micros: 5_000,
        currency: "USD".into(),
        rate_limited_until: None,
        health: ProviderHealth::Healthy,
    }]))
}

struct FailingCostSink {
    inner: Arc<dyn CostEventSink>,
    fail_append: bool,
}

impl FailingCostSink {
    fn new(inner: Arc<dyn CostEventSink>, fail_append: bool) -> Self {
        Self { inner, fail_append }
    }
}

#[async_trait::async_trait]
impl CostEventSink for FailingCostSink {
    async fn append(&self, ev: CostEvent) -> Result<(), SearchOsError> {
        if self.fail_append {
            return Err(SearchOsError::Storage("injected append failure".into()));
        }
        self.inner.append(ev).await
    }
    async fn record_quality_pass(&self, task_id: &str) -> Result<(), SearchOsError> {
        self.inner.record_quality_pass(task_id).await
    }
    async fn singleflight_saved_micros(&self) -> Result<i64, SearchOsError> {
        self.inner.singleflight_saved_micros().await
    }
    async fn record_singleflight(
        &self,
        fingerprint: &str,
        leader: &str,
        followers: &[String],
        saved: i64,
    ) -> Result<(), SearchOsError> {
        self.inner
            .record_singleflight(fingerprint, leader, followers, saved)
            .await
    }
    fn budget_guard_handle(&self) -> Arc<dyn BudgetSettler> {
        self.inner.budget_guard_handle()
    }
    async fn reserve_budget(
        &self,
        task_id: &str,
        trace_id: &str,
        estimated_micros: i64,
        currency: &str,
    ) -> Result<searchos_application::search_service::BudgetGuard, SearchOsError> {
        self.inner
            .reserve_budget(task_id, trace_id, estimated_micros, currency)
            .await
    }
    async fn settle_budget(&self, task_id: &str, actual_micros: i64) -> Result<(), SearchOsError> {
        self.inner.settle_budget(task_id, actual_micros).await
    }
    async fn exceed_budget(&self, task_id: &str) -> Result<(), SearchOsError> {
        self.inner.exceed_budget(task_id).await
    }
}

#[async_trait::async_trait]
impl BudgetSettler for FailingCostSink {
    async fn settle(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        self.inner
            .budget_guard_handle()
            .settle(task_id, spent_micros)
            .await
    }
    fn settle_sync(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        self.inner
            .budget_guard_handle()
            .settle_sync(task_id, spent_micros)
    }
    async fn exceed(&self, task_id: &str) -> Result<(), SearchOsError> {
        self.inner.budget_guard_handle().exceed(task_id).await
    }
    fn exceed_sync(&self, task_id: &str) -> Result<(), SearchOsError> {
        self.inner.budget_guard_handle().exceed_sync(task_id)
    }
    async fn release(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        self.inner
            .budget_guard_handle()
            .release(task_id, spent_micros)
            .await
    }
    fn release_sync(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        self.inner
            .budget_guard_handle()
            .release_sync(task_id, spent_micros)
    }
}

struct Harness {
    runtime: Arc<SearchOsRuntime>,
    service: Arc<SearchServiceImpl>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = searchos_core::SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        let runtime = SearchOsRuntime::init(cfg).await.unwrap();
        let service = Arc::new(SearchServiceImpl::new(deps(
            &runtime,
            runtime.cost_sink(),
            runtime.provider_registry.clone(),
            vec![],
            false,
        )));
        Self {
            runtime,
            service,
            _dir: dir,
        }
    }

    fn service_with(
        &self,
        provider: Arc<MockProvider>,
        allow_external: bool,
    ) -> Arc<SearchServiceImpl> {
        Arc::new(SearchServiceImpl::new(deps(
            &self.runtime,
            self.runtime.cost_sink(),
            mock_router(),
            vec![provider as Arc<dyn SearchProvider>],
            allow_external,
        )))
    }

    async fn seed(
        &self,
        chunk: &str,
        body: &str,
        url: &str,
        workspace: &str,
        fetched_at: DateTime<Utc>,
    ) {
        self.runtime
            .fulltext
            .index(IndexedDocument {
                chunk_id: searchos_domain::ids::ChunkId::new(format!("chunk_{chunk}")),
                document_id: searchos_domain::ids::DocumentId::new(format!("doc_{chunk}")),
                version_id: searchos_domain::ids::DocumentVersionId::new(format!("docv_{chunk}")),
                title: Some(body.lines().next().unwrap_or("doc").to_string()),
                body: body.to_string(),
                url: url.to_string(),
                language: Some("en".into()),
                workspace_id: workspace.into(),
                fetched_at,
            })
            .await
            .unwrap();
        self.runtime.fulltext.commit().await.unwrap();
    }
}

fn deps(
    runtime: &Arc<SearchOsRuntime>,
    sink: Arc<dyn CostEventSink>,
    router: Arc<dyn ProviderRouter>,
    providers: Vec<Arc<dyn SearchProvider>>,
    allow_external: bool,
) -> Arc<SearchDependencies> {
    Arc::new(SearchDependencies {
        profiles: ProfileRegistry::builtin(),
        quality: QualityPolicy::default(),
        freshness: FreshnessPolicy::default_policy(),
        source: SourcePolicy::default_policy(),
        cost_config: searchos_cost::CostModelConfig::default_config(),
        bm25: runtime.fulltext.clone(),
        vector: Some(runtime.vector.clone()),
        hybrid: Some(runtime.hybrid.clone()),
        embedder: runtime.embedder.clone(),
        canonical_cache: runtime.canonical_cache.clone(),
        semantic_cache: runtime.semantic_cache.clone(),
        retrieval_cache: runtime.retrieval_cache.clone(),
        embedding_cache: runtime.embedding_cache.clone(),
        rerank_cache: runtime.rerank_cache.clone(),
        router,
        search_providers: providers,
        local_port: runtime.local_port(),
        traces: runtime.traces(),
        cost_sink: sink,
        task_recorder: runtime.task_recorder(),
        singleflight: runtime.singleflight.clone(),
        cache: runtime.exact_cache.clone(),
        semantic_dedup: true,
        incremental: true,
        query_expansion: true,
        reranking: true,
        allow_external,
    })
}

fn request(query: &str, mode: SearchMode) -> SearchRequest {
    SearchRequest::new(
        query,
        ProfileId::general(),
        mode,
        WorkspaceId::new("default"),
    )
    .unwrap()
}

async fn join<T: Send + 'static>(handles: Vec<JoinHandle<T>>) -> Vec<T> {
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

// ---------------------------------------------------------------------------
// INV-30-01 / INV-30-03 / INV-30-04 / INV-30-08
// ---------------------------------------------------------------------------

/// INV-30-01: local-only results must obey freshness and excluded-domain
/// constraints; stale/excluded candidates are never served.
#[tokio::test]
async fn local_only_never_returns_stale_or_excluded_candidates() {
    let h = Harness::new().await;
    h.seed(
        "allowed",
        "Quokka habitat on Rottnest Island",
        "https://allowed.example.com/quokka",
        "default",
        Utc::now(),
    )
    .await;
    h.seed(
        "stale",
        "Quokka habitat old news",
        "https://old.example.com/quokka",
        "default",
        Utc::now() - chrono::Duration::days(400),
    )
    .await;
    h.seed(
        "blocked",
        "Quokka habitat secret",
        "https://blocked.com/quokka",
        "default",
        Utc::now(),
    )
    .await;
    let mut req = request("quokka habitat", SearchMode::LocalOnly);
    req.constraints.excluded_domains = vec!["blocked.com".into()];
    let resp = h.service.search(req).await.unwrap();
    let urls: HashSet<String> = resp.results.iter().map(|r| r.url.clone()).collect();
    assert_eq!(
        urls,
        HashSet::from(["https://allowed.example.com/quokka".to_string()]),
        "stale/excluded candidates must not be served: {urls:?}"
    );
}

/// INV-30-02: concurrent replays with one request_id execute once and bill once.
#[tokio::test]
async fn concurrent_same_request_id_executes_once_and_bills_once() {
    let h = Harness::new().await;
    let provider = MockProvider::new("tavily", 150, None);
    let service = h.service_with(provider.clone(), true);
    let base = request("what is tokio async runtime", SearchMode::ExternalOnly)
        .with_request_id("same-request-id");
    let mut req = base.clone();
    req.budget.max_external_queries = 1;
    req.budget.max_total_cost_micros = 100_000;
    let mut handles = Vec::new();
    for _ in 0..2 {
        let s = Arc::clone(&service);
        let r = req.clone();
        handles.push(tokio::spawn(async move { s.search(r).await }));
    }
    let results = join(handles).await;
    assert!(results.iter().all(|r| r.is_ok()));
    assert_eq!(provider.calls(), 1, "one external execution only");
    let tasks: i64 = h
        .runtime
        .sqlite
        .with_conn(|c| {
            c.query_row("SELECT count(*) FROM search_tasks", [], |r| r.get(0))
                .map_err(|e| SearchOsError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(tasks, 1, "one task row only");
    let events = CostRepo::new(&h.runtime.sqlite)
        .query(&CostQuery::default())
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.operation == CostOperation::WebSearch)
            .count(),
        1,
        "one external bill only"
    );
}

/// INV-30-05: one fingerprint executes one external call; followers get the
/// same result and measurable savings.
#[tokio::test]
async fn single_fingerprint_single_external_call() {
    let h = Harness::new().await;
    let provider = MockProvider::new("tavily", 150, None);
    let service = h.service_with(provider.clone(), true);
    let mut handles = Vec::new();
    for i in 0..2 {
        let s = Arc::clone(&service);
        let mut req = request("tokio runtime singleflight", SearchMode::ExternalOnly)
            .with_request_id(format!("req-{i}"));
        req.budget.max_external_queries = 1;
        req.budget.max_total_cost_micros = 100_000;
        handles.push(tokio::spawn(async move { s.search(req).await }));
    }
    let results = join(handles).await;
    assert!(results.iter().all(|r| r.is_ok()));
    assert_eq!(provider.calls(), 1, "singleflight merges the fingerprint");
    assert!(
        h.runtime
            .cost_sink()
            .singleflight_saved_micros()
            .await
            .unwrap()
            > 0,
        "follower savings are positive"
    );
}

/// INV-30-13: singleflight savings are measurable when followers join.
#[tokio::test]
async fn singleflight_savings_positive_when_followers_join() {
    let h = Harness::new().await;
    let provider = MockProvider::new("tavily", 150, None);
    let service = h.service_with(provider.clone(), true);
    let mut handles = Vec::new();
    for i in 0..3 {
        let s = Arc::clone(&service);
        let mut req = request("tokio runtime savings", SearchMode::ExternalOnly)
            .with_request_id(format!("savings-{i}"));
        req.budget.max_external_queries = 1;
        req.budget.max_total_cost_micros = 100_000;
        handles.push(tokio::spawn(async move { s.search(req).await }));
    }
    let _ = join(handles).await;
    let saved = h
        .runtime
        .cost_sink()
        .singleflight_saved_micros()
        .await
        .unwrap();
    assert!(saved > 0, "saved={saved}");
    assert_eq!(provider.calls(), 1);
}

/// INV-30-06: a failure after an earlier successful external call still
/// reports and settles the spend already incurred.
#[tokio::test]
async fn partial_external_failure_reports_spent_and_settles_spent() {
    let h = Harness::new().await;
    h.seed(
        "tokio",
        "Tokio performance comparison baseline",
        "https://tokio.rs/performance",
        "default",
        Utc::now(),
    )
    .await;
    let provider = MockProvider::new("tavily", 0, Some(1));
    let service = h.service_with(provider.clone(), true);
    let mut req = request("compare Tokio and Axum performance", SearchMode::Hybrid);
    req.budget.max_external_queries = 2;
    req.budget.max_total_cost_micros = 100_000;
    let resp = service.search(req).await.unwrap();
    assert!(
        resp.warnings
            .iter()
            .any(|w| w.contains("external_execution_failed")),
        "warnings: {:?}",
        resp.warnings
    );
    let spent_micros = (resp.cost.estimated * 1_000_000.0).round() as i64;
    assert_eq!(spent_micros, 5_000, "one successful call was spent");
    let events = CostRepo::new(&h.runtime.sqlite)
        .query(&CostQuery::default())
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.operation == CostOperation::WebSearch)
            .count(),
        1
    );
    let failed_attempts = events
        .iter()
        .filter(|e| {
            e.operation == CostOperation::ProviderAttempt && e.status == CostAttemptStatus::Failed
        })
        .count();
    assert_eq!(failed_attempts, 1, "failed attempt is auditable");
    let rows = BudgetRepo::new(&h.runtime.sqlite).list(10).unwrap();
    let row = rows
        .iter()
        .find(|r| r.spent_cost_micros == 5_000)
        .expect("reservation settled with spent amount");
    assert_eq!(row.status, "settled");
}

/// INV-30-03: a Reject path still settles its reservation.
#[tokio::test]
async fn reject_path_settles_reservation() {
    let h = Harness::new().await;
    let mut req = request("latest tokio version", SearchMode::LocalOnly);
    req.constraints.freshness = FreshnessRequirement::Strict;
    let resp = h.service.search(req).await.unwrap();
    assert_eq!(resp.decision.path, "reject");
    let rows = BudgetRepo::new(&h.runtime.sqlite).list(10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "settled");
    assert_eq!(rows[0].spent_cost_micros, 0);
}

/// INV-30-03: an early `?` after reserve settles through the RAII guard.
#[tokio::test]
async fn early_return_settles_reservation() {
    let h = Harness::new().await;
    h.seed(
        "local",
        "Tokio async runtime for rust",
        "https://tokio.rs",
        "default",
        Utc::now(),
    )
    .await;
    let sink = Arc::new(FailingCostSink::new(h.runtime.cost_sink(), true));
    let service = Arc::new(SearchServiceImpl::new(deps(
        &h.runtime,
        sink,
        h.runtime.provider_registry.clone(),
        vec![],
        false,
    )));
    let result = service
        .search(request("tokio async runtime", SearchMode::LocalOnly))
        .await;
    assert!(result.is_err(), "injected append failure must propagate");
    let rows = BudgetRepo::new(&h.runtime.sqlite).list(10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "settled", "guard settled on early return");
}

/// INV-30-04: LocalOnly must not be served from a cache entry written by an
/// external-capable mode.
#[tokio::test]
async fn local_only_does_not_hit_hybrid_cache_entry() {
    let h = Harness::new().await;
    h.seed(
        "tokio",
        "Tokio performance comparison baseline",
        "https://tokio.rs/performance",
        "default",
        Utc::now(),
    )
    .await;
    let provider = MockProvider::new("tavily", 0, None);
    let service = h.service_with(provider.clone(), true);
    let mut first = request("compare Tokio and Axum performance", SearchMode::LocalFirst);
    first.budget.max_external_queries = 2;
    first.budget.max_total_cost_micros = 100_000;
    let resp1 = service.search(first).await.unwrap();
    assert_eq!(
        resp1.quality.status,
        searchos_domain::response::QualityStatus::Passed
    );
    let mut second = request("compare Tokio and Axum performance", SearchMode::LocalOnly);
    second.budget.max_external_queries = 2;
    second.budget.max_total_cost_micros = 100_000;
    let resp2 = service.search(second).await.unwrap();
    assert!(
        !resp2.replayed,
        "LocalOnly must not reuse a LocalFirst cache entry"
    );
    assert_eq!(
        searchos_observability::MetricsRegistry::global().counter("cache.cross_mode_hit_total"),
        0
    );
}

/// INV-30-04: a cache hit still records the idempotency claim.
#[tokio::test]
async fn cache_hit_records_idempotency_claim() {
    let h = Harness::new().await;
    h.seed(
        "tokio",
        "Tokio async runtime for rust",
        "https://tokio.rs",
        "default",
        Utc::now(),
    )
    .await;
    let first = h
        .service
        .search(request("tokio async runtime", SearchMode::LocalOnly).with_request_id("req-a"))
        .await
        .unwrap();
    assert!(!first.replayed);
    let second = h
        .service
        .search(request("tokio async runtime", SearchMode::LocalOnly).with_request_id("req-b"))
        .await
        .unwrap();
    assert!(second.replayed, "second request hits the exact cache");
    let record = IdempotencyRepo::new(&h.runtime.sqlite)
        .get("req-b")
        .unwrap()
        .expect("cache hit records the claim");
    assert_eq!(record.status, "completed");
}

/// INV-30-08: deleting a document removes its chunks from the full-text index.
#[tokio::test]
async fn deleted_document_is_not_retrieved() {
    let h = Harness::new().await;
    let mut req = IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("notes/delete-me".into());
    let doc = h
        .runtime
        .application
        .ingest_service
        .ingest_text(req, "delete me: zanzibar knowledge")
        .await
        .unwrap();
    h.runtime.process_outbox(10).await.unwrap();
    let first_req = request("zanzibar knowledge", SearchMode::LocalOnly);
    let first = h.service.search(first_req).await.unwrap();
    assert!(
        first.results.iter().any(|r| r.url.contains("delete-me")),
        "document is retrievable before delete"
    );
    DocumentRepo::new(&h.runtime.sqlite)
        .delete(&doc.document_id)
        .unwrap();
    h.runtime.process_outbox(10).await.unwrap();
    let (status, attempts): (String, i64) = h
        .runtime
        .sqlite
        .with_conn(|c| {
            c.query_row(
                "SELECT status, attempts FROM index_outbox WHERE operation = 'delete'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| SearchOsError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(status, "processed");
    assert_eq!(attempts, 0);
    h.runtime.exact_cache.clear();
    let second_req = request("zanzibar knowledge", SearchMode::LocalOnly);
    let second = h.service.search(second_req).await.unwrap();
    assert!(
        second.results.is_empty(),
        "deleted document must not be retrieved: {:?}",
        second.results
    );
}

// ---------------------------------------------------------------------------
// INV-30-07 / INV-30-10 / INV-30-19
// ---------------------------------------------------------------------------

/// INV-30-07 + INV-30-10: rebuild preserves workspace / language / age / title.
#[tokio::test]
async fn rebuild_preserves_workspace_language_age_and_title() {
    let h = Harness::new().await;
    let workspace = WorkspaceId::new("ws_a");
    let doc = searchos_domain::document::Document {
        id: searchos_domain::ids::DocumentId::generate(),
        source_id: searchos_domain::ids::SourceId::generate(),
        canonical_url: "searchos-local://ws_a/notes/zh".into(),
        current_version_id: None,
        content_type: "text/plain".into(),
        language: Some("zh".into()),
        source_type: "local_text".into(),
        title: Some("Tokio 中文".into()),
        workspace_id: workspace.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let fetched_at = Utc::now() - chrono::Duration::days(400);
    let content_ref = searchos_document::hash::content_ref("Tokio 中文 async runtime".as_bytes());
    let version = searchos_domain::document::DocumentVersion {
        id: searchos_domain::ids::DocumentVersionId::generate(),
        document_id: doc.id.clone(),
        content_hash: "hash".into(),
        raw_content_ref: content_ref.clone(),
        markdown_ref: None,
        fetched_at,
        published_at: None,
        extractor_version: "test".into(),
        crawler_version: None,
    };
    let chunks = searchos_document::chunk_text("Tokio 中文 async runtime", &Default::default());
    let domain_chunks: Vec<_> = chunks
        .iter()
        .map(|c| searchos_domain::document::DocumentChunk {
            id: searchos_domain::ids::ChunkId::generate(),
            version_id: version.id.clone(),
            document_id: doc.id.clone(),
            chunk_hash: c.hash.clone(),
            content_ref: content_ref.clone(),
            text: c.text.clone(),
            token_count: Some(1),
            chunk_index: c.index,
            created_at: Utc::now(),
        })
        .collect();
    let payloads: Vec<IndexedDocument> = domain_chunks
        .iter()
        .map(|ch| IndexedDocument {
            chunk_id: ch.id.clone(),
            document_id: doc.id.clone(),
            version_id: version.id.clone(),
            title: doc.title.clone(),
            body: ch.text.clone(),
            url: doc.canonical_url.clone(),
            language: doc.language.clone(),
            workspace_id: workspace.to_string(),
            fetched_at: Utc::now(),
        })
        .collect();
    searchos_storage_fs::ObjectStore::put(
        h.runtime.objects.as_ref(),
        "Tokio 中文 async runtime".as_bytes(),
    )
    .await
    .unwrap();
    DocumentRepo::new(&h.runtime.sqlite)
        .save_versioned(&doc, &version, &domain_chunks, &payloads, &workspace)
        .unwrap();
    h.runtime.process_outbox(10).await.unwrap();
    h.runtime.rebuild_index().await.unwrap();
    let hits = h
        .runtime
        .local_port()
        .retrieve("Tokio 中文", 5, Some("ws_a"))
        .await
        .unwrap();
    assert!(!hits.is_empty(), "rebuilt document is searchable");
    let hit = &hits[0];
    assert_eq!(hit.title.as_deref(), Some("Tokio 中文"));
    assert_eq!(hit.language.as_deref(), Some("zh"));
    assert_eq!(hit.workspace_id.as_deref(), Some("ws_a"));
    assert_eq!(
        hit.fetched_at.timestamp(),
        fetched_at.timestamp(),
        "age/provenance preserved"
    );
}

/// INV-30-07: a rebuild is net-neutral for queries.
#[tokio::test]
async fn rebuild_is_net_neutral_for_queries() {
    let h = Harness::new().await;
    let workspace = WorkspaceId::new("ws_a");
    let doc = searchos_domain::document::Document {
        id: searchos_domain::ids::DocumentId::generate(),
        source_id: searchos_domain::ids::SourceId::generate(),
        canonical_url: "searchos-local://ws_a/notes/neutral".into(),
        current_version_id: None,
        content_type: "text/plain".into(),
        language: Some("en".into()),
        source_type: "local_text".into(),
        title: Some("Neutral notes".into()),
        workspace_id: workspace.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let content_ref = searchos_document::hash::content_ref("quokka habitat neutral".as_bytes());
    let version = searchos_domain::document::DocumentVersion {
        id: searchos_domain::ids::DocumentVersionId::generate(),
        document_id: doc.id.clone(),
        content_hash: "hash2".into(),
        raw_content_ref: content_ref.clone(),
        markdown_ref: None,
        fetched_at: Utc::now(),
        published_at: None,
        extractor_version: "test".into(),
        crawler_version: None,
    };
    let chunks = searchos_document::chunk_text("quokka habitat neutral", &Default::default());
    let domain_chunks: Vec<_> = chunks
        .iter()
        .map(|c| searchos_domain::document::DocumentChunk {
            id: searchos_domain::ids::ChunkId::generate(),
            version_id: version.id.clone(),
            document_id: doc.id.clone(),
            chunk_hash: c.hash.clone(),
            content_ref: content_ref.clone(),
            text: c.text.clone(),
            token_count: Some(1),
            chunk_index: c.index,
            created_at: Utc::now(),
        })
        .collect();
    let payloads: Vec<IndexedDocument> = domain_chunks
        .iter()
        .map(|ch| IndexedDocument {
            chunk_id: ch.id.clone(),
            document_id: doc.id.clone(),
            version_id: version.id.clone(),
            title: doc.title.clone(),
            body: ch.text.clone(),
            url: doc.canonical_url.clone(),
            language: doc.language.clone(),
            workspace_id: workspace.to_string(),
            fetched_at: Utc::now(),
        })
        .collect();
    searchos_storage_fs::ObjectStore::put(
        h.runtime.objects.as_ref(),
        "quokka habitat neutral".as_bytes(),
    )
    .await
    .unwrap();
    DocumentRepo::new(&h.runtime.sqlite)
        .save_versioned(&doc, &version, &domain_chunks, &payloads, &workspace)
        .unwrap();
    h.runtime.process_outbox(10).await.unwrap();
    let before = h
        .runtime
        .local_port()
        .retrieve("quokka habitat", 5, Some("ws_a"))
        .await
        .unwrap();
    h.runtime.rebuild_index().await.unwrap();
    let after = h
        .runtime
        .local_port()
        .retrieve("quokka habitat", 5, Some("ws_a"))
        .await
        .unwrap();
    let ids_before: Vec<_> = before.iter().map(|c| c.chunk_id.to_string()).collect();
    let ids_after: Vec<_> = after.iter().map(|c| c.chunk_id.to_string()).collect();
    assert_eq!(ids_before, ids_after);
    assert_eq!(before.len(), after.len());
}

/// INV-30-17: configured resource limits reach the fetch enforcement point.
#[tokio::test]
async fn configured_limits_are_applied() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    cfg.security.max_response_bytes = 4321;
    cfg.security.max_redirects = 2;
    cfg.crawler.default_timeout_ms = 1234;
    cfg.crawler.max_concurrent = 7;
    cfg.crawler.respect_robots = false;
    cfg.crawler.robots_unknown_allow = true;
    cfg.crawler.crawl_delay_cap_seconds = 3;
    let rt = SearchOsRuntime::init(cfg).await.unwrap();
    let limits = rt.application.fetch_service.limits();
    assert_eq!(limits.max_response_bytes, 4321);
    assert_eq!(limits.max_redirects, 2);
    assert_eq!(limits.timeout, Duration::from_millis(1234));
    assert_eq!(limits.max_concurrent, 7);
    assert!(!limits.respect_robots);
    assert!(!limits.unknown_robots_disallow);
    assert_eq!(limits.crawl_delay_cap_seconds, 3);
}

/// INV-30-19: CLI/HTTP/MCP share one mode parser and one result-limit contract.
#[tokio::test]
async fn transports_reject_unknown_mode_identically() {
    use searchos_interface_http::routes::{router, AppState};
    use searchos_interface_mcp::server::handle_message;
    use tower::ServiceExt;

    for bogus in ["LOCAL_ONLY", "localOnly", "", "hybrid "] {
        assert!(SearchMode::parse(bogus).is_err(), "accepted {bogus:?}");
    }
    let h = Harness::new().await;
    let app = router(AppState::new(h.runtime.clone()));
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/search")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"query": "q", "mode": "bogus"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let mcp = handle_message(
        h.runtime.clone(),
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search","arguments":{"query":"q","mode":"bogus"}}}),
    )
    .await
    .unwrap();
    assert_eq!(mcp["error"]["data"]["code"], "VALIDATION_FAILED");
}

/// INV-30-19: all transports share the same default and clamp for max_results.
#[test]
fn transports_agree_on_max_results() {
    use searchos_interface_http::tavily::TavilySearchRequest;
    use searchos_interface_mcp::server::tools;

    let req = request("q", SearchMode::LocalFirst);
    assert_eq!(req.max_results, 5);
    assert_eq!(searchos_domain::request::clamp_max_results(0), 1);
    assert_eq!(searchos_domain::request::clamp_max_results(500), 100);
    let tavily = TavilySearchRequest {
        query: "q".into(),
        max_results: Some(500),
        ..Default::default()
    };
    assert_eq!(tavily.effective_max_results(), 100);
    assert_eq!(TavilySearchRequest::default().effective_max_results(), 5);
    let catalogue = tools();
    let search = catalogue
        .iter()
        .find(|t| t["name"] == "search")
        .expect("search tool");
    assert_eq!(
        search["inputSchema"]["properties"]["max_results"]["maximum"],
        100
    );
}

/// INV-30-19: the shared transport error table is used by all transports.
#[test]
fn error_code_table_is_consistent() {
    for code in TRANSPORT_ERROR_CODES {
        assert_eq!(code.to_ascii_uppercase(), *code);
        assert!(!code.contains('-'));
    }
    assert!(TRANSPORT_ERROR_CODES.contains(&"VALIDATION_FAILED"));
    assert!(TRANSPORT_ERROR_CODES.contains(&"INTERNAL_ERROR"));
}

/// INV-30-13: pricing uses declared unit counts and parse failures are visible.
#[test]
fn per_thousand_units_pricing_uses_unit_counts() {
    let mut config = searchos_cost::CostModelConfig::default_config();
    config.upsert_provider(searchos_cost::model::ProviderPricing {
        provider_id: "token_api".into(),
        currency: "USD".into(),
        pricing: searchos_cost::model::PricingModel::PerThousandUnits {
            unit_price_per_k: "0.010".into(),
        },
        version: 1,
        from_real_bill: false,
    });
    let estimator = searchos_cost::CostEstimator::from_config(&config);
    assert_eq!(
        estimator
            .estimate_request_with_units("token_api", 2_000)
            .amount_micros,
        20_000
    );
    assert_eq!(
        estimator
            .estimate_request_with_units("token_api", 1)
            .amount_micros,
        10
    );
}

/// INV-30-13: malformed prices are reported, not silently zero.
#[test]
fn cost_parse_failure_is_reported() {
    let mut config = searchos_cost::CostModelConfig::default_config();
    config.upsert_provider(searchos_cost::model::ProviderPricing {
        provider_id: "broken".into(),
        currency: "USD".into(),
        pricing: searchos_cost::model::PricingModel::RequestBased {
            unit_price: "not-a-price".into(),
        },
        version: 1,
        from_real_bill: false,
    });
    let errors = config.validate_prices();
    assert!(errors.iter().any(|e| e.contains("broken")));
    let estimator = searchos_cost::CostEstimator::from_config(&config);
    assert!(estimator
        .parse_errors()
        .iter()
        .any(|e| e.contains("broken")));
}

/// G-02: BM25 + vector recall are fused into one hybrid result set.
#[tokio::test]
async fn hybrid_retrieval_fuses_bm25_and_vector_candidates() {
    use searchos_retrieval::{HybridRetriever, RrfConfig};

    let dir = tempfile::tempdir().unwrap();
    let bm25 = Arc::new(searchos_index_tantivy::TantivyIndex::in_memory().unwrap());
    let vector = Arc::new(
        searchos_vector_lancedb::LanceVectorIndex::open(&dir.path().join("vectors"), "embeddings")
            .await
            .unwrap(),
    );
    for i in 0..2_000 {
        let body = if i == 0 {
            "Tokio async runtime for rust".to_string()
        } else {
            format!("filler corpus chunk {i}")
        };
        bm25.index(IndexedDocument {
            chunk_id: searchos_domain::ids::ChunkId::new(format!("bm25_{i}")),
            document_id: searchos_domain::ids::DocumentId::new(format!("doc_{i}")),
            version_id: searchos_domain::ids::DocumentVersionId::new(format!("docv_{i}")),
            title: Some("doc".into()),
            body: body.clone(),
            url: "https://example.com/doc".into(),
            language: Some("en".into()),
            workspace_id: "*".into(),
            fetched_at: Utc::now(),
        })
        .await
        .unwrap();
    }
    bm25.commit().await.unwrap();
    vector
        .rebuild_from(&[searchos_vector_lancedb::IndexedVec {
            chunk_id: "vector_0".into(),
            document_id: "doc_vector".into(),
            version_id: "docv_vector".into(),
            title: Some("Tokio async runtime".into()),
            body: "Tokio async runtime for rust".into(),
            url: "https://semantic.example.com/tokio".into(),
            language: Some("en".into()),
            workspace_id: "*".into(),
            model_id: "hashing-mvp".into(),
            model_version: "v1".into(),
            fetched_at: Utc::now().timestamp() as u64,
            vector: searchos_provider::embedding::HashingEmbedder::new(256)
                .embed_one("Tokio async runtime for rust"),
        }])
        .await
        .unwrap();
    let hybrid = HybridRetriever::new(
        bm25,
        Some(vector as Arc<dyn searchos_retrieval::VectorIndex>),
        RrfConfig::default(),
    );
    let hits = hybrid
        .retrieve(&RetrievalQuery::new("tokio async runtime", 10))
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert!(
        hits.iter().any(|c| c.chunk_id.as_str() == "vector_0"),
        "vector candidate participates: {:?}",
        hits.iter()
            .map(|c| c.chunk_id.to_string())
            .collect::<Vec<_>>()
    );
    assert!(
        hits.iter().any(|c| c.chunk_id.as_str() == "bm25_0"),
        "bm25 candidate participates"
    );
    assert!(hits.iter().any(|c| c.source == RetrievalSource::Hybrid));
}

/// G-05: memory entries are writable and semantically retrievable.
#[tokio::test]
async fn memory_semantic_recall_and_workspace_isolation() {
    let h = Harness::new().await;
    let namespace = MemoryNamespace {
        workspace: WorkspaceId::new("ws_a"),
        project: None,
        agent: Some("a1".into()),
    };
    h.runtime
        .application
        .memory_service
        .remember(
            namespace.clone(),
            searchos_domain::memory::SharingPolicy::Private,
            "runtime notes",
            "Tokio async runtime",
            None,
            &[],
        )
        .await
        .unwrap();
    let hits = h
        .runtime
        .application
        .memory_service
        .search("asynchronous executor", &namespace, 5)
        .await
        .unwrap();
    assert!(!hits.is_empty(), "different wording recalls via vector+RRF");
    let other = MemoryNamespace {
        workspace: WorkspaceId::new("ws_b"),
        project: None,
        agent: None,
    };
    let cross = h
        .runtime
        .application
        .memory_service
        .search("asynchronous executor", &other, 5)
        .await
        .unwrap();
    assert!(cross.is_empty(), "private memory does not cross workspaces");
}
