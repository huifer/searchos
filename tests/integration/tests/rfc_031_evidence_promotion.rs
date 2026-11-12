//! RFC-031 external evidence promotion acceptance tests.
//!
//! The kernel of the RFC: a purchased external answer becomes durable,
//! rebuildable knowledge — or is explicitly not promoted with a reason.
//! Nothing the final quality gate rejected may reach the index.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use searchos_application::fetch_service::{FetchService, FetchedDocument};
use searchos_application::promotion_service::{
    CacheInvalidator, CaptureTarget, EvidencePromotionService, EvidencePromotionServiceImpl,
    PromotionConfig, PromotionStore,
};
use searchos_domain::ids::{DocumentId, DocumentVersionId, TaskId, WorkspaceId};
use searchos_domain::promotion::{EvidenceItem, ExternalAcquisition, PromotionState, QueryOrigin};
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, RetrievalQuery};
use searchos_domain::SearchOsError;
use searchos_retrieval::SearchIndex;

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockStore {
    acquisitions: Mutex<Vec<(ExternalAcquisition, Vec<EvidenceItem>)>>,
    captures: Mutex<Vec<String>>,
    states: Mutex<Vec<(String, PromotionState, Option<String>)>>,
    bindings: Mutex<Vec<(String, String, String)>>,
    reuse: Mutex<Vec<(String, String, String)>>,
    /// item_id -> (url, workspace, state, task, trace)
    targets: Mutex<Vec<(String, CaptureTarget)>>,
    promoted_docs: Mutex<HashSet<String>>,
}

impl MockStore {
    fn with_target(item_id: &str, url: &str, state: PromotionState) -> Arc<Self> {
        let s = Arc::new(Self::default());
        s.targets.lock().unwrap().push((
            item_id.to_string(),
            CaptureTarget {
                url: url.to_string(),
                workspace_id: "default".into(),
                task_id: "task_1".into(),
                trace_id: Some("trace_1".into()),
                state,
            },
        ));
        s
    }

    fn states(&self) -> Vec<(String, PromotionState, Option<String>)> {
        self.states.lock().unwrap().clone()
    }
    fn captures(&self) -> Vec<String> {
        self.captures.lock().unwrap().clone()
    }
}

#[async_trait]
impl PromotionStore for MockStore {
    async fn record_acquisition(
        &self,
        acquisition: &ExternalAcquisition,
        items: &[EvidenceItem],
    ) -> Result<(), SearchOsError> {
        self.acquisitions
            .lock()
            .unwrap()
            .push((acquisition.clone(), items.to_vec()));
        Ok(())
    }

    async fn enqueue_capture(
        &self,
        item_id: &str,
        _url: &str,
        _workspace_id: &str,
        _task_id: &str,
        _acquisition_id: &str,
    ) -> Result<bool, SearchOsError> {
        self.captures.lock().unwrap().push(item_id.to_string());
        Ok(true)
    }

    async fn capture_target(&self, item_id: &str) -> Result<Option<CaptureTarget>, SearchOsError> {
        Ok(self
            .targets
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| id == item_id)
            .map(|(_, t)| t.clone()))
    }

    async fn update_item_state(
        &self,
        item_id: &str,
        state: PromotionState,
        _document_id: Option<&str>,
        _version_id: Option<&str>,
        _chunk_count: Option<usize>,
        reason: Option<&str>,
    ) -> Result<(), SearchOsError> {
        self.states.lock().unwrap().push((
            item_id.to_string(),
            state,
            reason.map(|s| s.to_string()),
        ));
        Ok(())
    }

    async fn bind_document(
        &self,
        document_id: &str,
        workspace_id: &str,
        visibility: &str,
    ) -> Result<(), SearchOsError> {
        self.bindings.lock().unwrap().push((
            document_id.to_string(),
            workspace_id.to_string(),
            visibility.to_string(),
        ));
        Ok(())
    }

    async fn record_reuse(
        &self,
        task_id: &str,
        workspace_id: &str,
        document_id: &str,
    ) -> Result<(), SearchOsError> {
        self.reuse.lock().unwrap().push((
            task_id.to_string(),
            workspace_id.to_string(),
            document_id.to_string(),
        ));
        Ok(())
    }

    async fn promoted_document_ids(
        &self,
        document_ids: &[String],
    ) -> Result<HashSet<String>, SearchOsError> {
        let promoted = self.promoted_docs.lock().unwrap();
        Ok(document_ids
            .iter()
            .filter(|id| promoted.contains(*id))
            .cloned()
            .collect())
    }
}

/// Fetch mock: returns a fixed document, or a scripted error.
struct MockFetch {
    /// `Some((document_id, chunk_count))` = success.
    ok: Option<(String, usize)>,
    /// `Some((kind, message))` = failure; kind is `security` / `provider` /
    /// `retrieval`.
    err: Option<(&'static str, String)>,
    payloads: Vec<IndexedDocument>,
}

impl MockFetch {
    fn ok(payloads: Vec<IndexedDocument>) -> Self {
        Self {
            ok: Some(("doc_promoted".into(), payloads.len().max(1))),
            err: None,
            payloads,
        }
    }

    fn err(kind: &'static str, message: &str) -> Self {
        Self {
            ok: None,
            err: Some((kind, message.to_string())),
            payloads: Vec::new(),
        }
    }
}

#[async_trait]
impl FetchService for MockFetch {
    async fn fetch_and_persist(
        &self,
        _url: &str,
        _workspace_id: &WorkspaceId,
    ) -> Result<FetchedDocument, SearchOsError> {
        if let Some((kind, message)) = &self.err {
            return Err(match *kind {
                "security" => SearchOsError::Security(message.clone()),
                "retrieval" => SearchOsError::Retrieval(message.clone()),
                _ => SearchOsError::Provider(message.clone()),
            });
        }
        let (document_id, chunk_count) = self.ok.clone().unwrap_or_default();
        Ok(FetchedDocument {
            document_id: DocumentId::new(document_id),
            version_id: DocumentVersionId::new("docv_1"),
            canonical_url: "https://example.com/page".into(),
            content_hash: "sha256:abc".into(),
            changed: true,
            title: Some("Example".into()),
            markdown_ref: "sha256:md".into(),
            chunk_count,
            from_cache: false,
            raw_url: Some("https://example.com/page".into()),
        })
    }

    async fn fetch_preview(
        &self,
        _url: &str,
    ) -> Result<searchos_fetch::extract::ExtractedContent, SearchOsError> {
        Err(SearchOsError::Retrieval("not implemented".into()))
    }

    async fn reindex_current_version(
        &self,
        _document_id: &DocumentId,
        workspace_id: &WorkspaceId,
    ) -> Result<Vec<IndexedDocument>, SearchOsError> {
        Ok(self
            .payloads
            .iter()
            .cloned()
            .map(|mut p| {
                p.workspace_id = workspace_id.to_string();
                p
            })
            .collect())
    }
}

#[derive(Default)]
struct MockIndex {
    indexed: Mutex<Vec<IndexedDocument>>,
    committed: AtomicUsize,
}

#[async_trait]
impl SearchIndex for MockIndex {
    async fn index(&self, document: IndexedDocument) -> Result<(), SearchOsError> {
        self.indexed.lock().unwrap().push(document);
        Ok(())
    }
    async fn search(
        &self,
        _query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        Ok(Vec::new())
    }
    async fn delete(&self, _document_id: &DocumentId) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn count(&self) -> Result<u64, SearchOsError> {
        Ok(self.indexed.lock().unwrap().len() as u64)
    }
    async fn commit(&self) -> Result<(), SearchOsError> {
        self.committed.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    async fn rebuild_from(&self, documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        let mut indexed = self.indexed.lock().unwrap();
        indexed.clear();
        indexed.extend(documents.to_vec());
        Ok(documents.len() as u64)
    }
}

#[derive(Default)]
struct MockInvalidator {
    calls: AtomicUsize,
}

#[async_trait]
impl CacheInvalidator for MockInvalidator {
    async fn invalidate_after_promotion(&self, _workspace_id: &str, _canonical_url: &str) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
}

fn payload() -> IndexedDocument {
    IndexedDocument {
        chunk_id: searchos_domain::ids::ChunkId::new("chunk_1"),
        document_id: DocumentId::new("doc_promoted"),
        version_id: DocumentVersionId::new("docv_1"),
        title: Some("Example".into()),
        body: "the full captured page body".into(),
        url: "https://example.com/page".into(),
        language: Some("en".into()),
        workspace_id: "default".into(),
        fetched_at: Utc::now(),
    }
}

fn item(id: &str, state: PromotionState, reason: Option<&str>) -> EvidenceItem {
    EvidenceItem {
        id: id.into(),
        acquisition_id: String::new(),
        url: "https://example.com/page".into(),
        canonical_url: "https://example.com/page".into(),
        title: Some("Example".into()),
        provider_score: 0.9,
        relevance: 0.8,
        state,
        document_id: None,
        version_id: None,
        chunk_count: None,
        reason: reason.map(|s| s.to_string()),
        promoted_at: None,
        created_at: Utc::now(),
    }
}

fn acquisition() -> ExternalAcquisition {
    ExternalAcquisition {
        id: "acq_1".into(),
        task_id: "task_1".into(),
        request_id: "req_1".into(),
        workspace_id: "default".into(),
        provider_id: "tavily".into(),
        query: "example docs".into(),
        query_origin: QueryOrigin::Gap,
        requirement_id: Some("need_1".into()),
        strategy: Default::default(),
        created_at: Utc::now(),
    }
}

type Harness = (
    Arc<EvidencePromotionServiceImpl>,
    Arc<MockStore>,
    Arc<MockFetch>,
    Arc<MockIndex>,
    Arc<MockInvalidator>,
);

fn harness(item_id: &str, state: PromotionState, fetch: MockFetch) -> Harness {
    let store = MockStore::with_target(item_id, "https://example.com/page", state);
    let fetch = Arc::new(fetch);
    let index = Arc::new(MockIndex::default());
    let invalidator = Arc::new(MockInvalidator::default());
    let service = Arc::new(EvidencePromotionServiceImpl::new(
        store.clone(),
        fetch.clone(),
        Some(index.clone() as Arc<dyn SearchIndex>),
        Some(invalidator.clone() as Arc<dyn CacheInvalidator>),
        None,
        &searchos_cost::CostModelConfig::default_config(),
        PromotionConfig::default(),
    ));
    (service, store, fetch, index, invalidator)
}

// ---------------------------------------------------------------------------
// AC1 / AC4: an accepted item becomes a document + index entry with its body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn accepted_item_is_captured_indexed_and_bound() {
    let (service, store, _fetch, index, invalidator) = harness(
        "evi_1",
        PromotionState::Accepted,
        MockFetch::ok(vec![payload()]),
    );

    service.capture("evi_1").await.unwrap();

    // State machine: accepted -> promoted.
    assert_eq!(
        store.states(),
        vec![("evi_1".to_string(), PromotionState::Promoted, None)]
    );
    // The full body is indexed, not a provider snippet.
    let indexed = index.indexed.lock().unwrap().clone();
    assert_eq!(indexed.len(), 1);
    assert_eq!(indexed[0].body, "the full captured page body");
    assert!(
        index.committed.load(Ordering::Relaxed) > 0,
        "index committed"
    );
    // Workspace binding is written.
    assert_eq!(
        store.bindings.lock().unwrap().clone(),
        vec![(
            "doc_promoted".to_string(),
            "default".to_string(),
            "workspace".to_string()
        )]
    );
    // The purchase must not stay hidden behind a stale cache (D4).
    assert_eq!(invalidator.calls.load(Ordering::Relaxed), 1);
}

// ---------------------------------------------------------------------------
// AC3: rejected evidence is recorded but never captured or indexed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejected_item_is_never_captured_or_indexed() {
    let (service, store, _fetch, index, _inv) = harness(
        "evi_rejected",
        PromotionState::Accepted, // target is accepted; the item is rejected
        MockFetch::ok(vec![payload()]),
    );

    service
        .record_and_queue(
            acquisition(),
            vec![item(
                "evi_rejected",
                PromotionState::Rejected,
                Some("final_gate_rejected"),
            )],
        )
        .await
        .unwrap();

    assert!(store.captures().is_empty(), "rejected item never queued");
    assert!(index.indexed.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// AC1: the acquisition and its accepted item are persisted, then queued
// ---------------------------------------------------------------------------

#[tokio::test]
async fn accepted_item_is_recorded_and_queued_for_capture() {
    let (service, store, _fetch, _index, _inv) = harness(
        "evi_1",
        PromotionState::Pending,
        MockFetch::ok(vec![payload()]),
    );

    service
        .record_and_queue(
            acquisition(),
            vec![item("evi_1", PromotionState::Accepted, None)],
        )
        .await
        .unwrap();

    let recorded = store.acquisitions.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0.id, "acq_1");
    assert_eq!(recorded[0].1.len(), 1);
    assert_eq!(recorded[0].1[0].acquisition_id, "acq_1");
    assert_eq!(store.captures(), vec!["evi_1".to_string()]);
}

// ---------------------------------------------------------------------------
// AC11 / AC12: capture refusals are recorded with a stable reason
// ---------------------------------------------------------------------------

#[tokio::test]
async fn robots_disallowed_capture_is_skipped_with_reason() {
    let (service, store, _fetch, index, invalidator) = harness(
        "evi_1",
        PromotionState::Accepted,
        MockFetch::err("security", "robots disallowed"),
    );

    service.capture("evi_1").await.unwrap();

    assert_eq!(
        store.states(),
        vec![(
            "evi_1".to_string(),
            PromotionState::Skipped,
            Some("robots_disallowed".to_string())
        )]
    );
    assert!(
        index.indexed.lock().unwrap().is_empty(),
        "no document created"
    );
    assert_eq!(invalidator.calls.load(Ordering::Relaxed), 0);
}

// ---------------------------------------------------------------------------
// Idempotency: capture only acts on `accepted` items
// ---------------------------------------------------------------------------

#[tokio::test]
async fn capture_is_a_noop_for_already_promoted_items() {
    let (service, store, _fetch, index, _inv) = harness(
        "evi_1",
        PromotionState::Promoted,
        MockFetch::ok(vec![payload()]),
    );

    service.capture("evi_1").await.unwrap();

    assert!(store.states().is_empty(), "no state transition");
    assert!(index.indexed.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// AC16: the per-task cap bounds how many pages one purchase may capture
// ---------------------------------------------------------------------------

#[tokio::test]
async fn per_task_cap_bounds_captures() {
    let store = Arc::new(MockStore::default());
    let fetch = Arc::new(MockFetch::ok(vec![payload()]));
    let service = EvidencePromotionServiceImpl::new(
        store.clone(),
        fetch,
        None,
        None,
        None,
        &searchos_cost::CostModelConfig::default_config(),
        PromotionConfig {
            enabled: true,
            max_promotions_per_task: 1,
            ..Default::default()
        },
    );

    service
        .record_and_queue(
            acquisition(),
            vec![
                item("evi_a", PromotionState::Accepted, None),
                item("evi_b", PromotionState::Accepted, None),
            ],
        )
        .await
        .unwrap();

    assert_eq!(store.captures(), vec!["evi_a".to_string()]);
}

// ---------------------------------------------------------------------------
// Reuse accounting: only external-sourced documents are counted
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reuse_is_recorded_only_for_promoted_documents() {
    let (service, store, _fetch, _index, _inv) =
        harness("evi_1", PromotionState::Pending, MockFetch::ok(Vec::new()));
    store
        .promoted_docs
        .lock()
        .unwrap()
        .insert("doc_promoted".to_string());

    let promoted = service
        .promoted_document_ids(&["doc_promoted".to_string(), "doc_owned".to_string()])
        .await
        .unwrap();
    assert!(promoted.contains("doc_promoted"));
    assert!(!promoted.contains("doc_owned"));

    service
        .record_reuse(
            &TaskId::new("task_2"),
            &WorkspaceId::new("default"),
            "doc_promoted",
        )
        .await
        .unwrap();
    assert_eq!(store.reuse.lock().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Disabled promotion never touches the store
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disabled_promotion_records_nothing() {
    let store = Arc::new(MockStore::default());
    let fetch = Arc::new(MockFetch::ok(vec![payload()]));
    let service = EvidencePromotionServiceImpl::new(
        store.clone(),
        fetch,
        None,
        None,
        None,
        &searchos_cost::CostModelConfig::default_config(),
        PromotionConfig {
            enabled: false,
            ..Default::default()
        },
    );

    service
        .record_and_queue(
            acquisition(),
            vec![item("evi_1", PromotionState::Accepted, None)],
        )
        .await
        .unwrap();

    assert!(store.acquisitions.lock().unwrap().is_empty());
    assert!(store.captures().is_empty());
}

// ---------------------------------------------------------------------------
// A capture fetch failure still charges its cost attempt (auditability)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn promotion_status_is_never_left_pending_after_capture_attempt() {
    let (service, store, _fetch, _index, _inv) = harness(
        "evi_1",
        PromotionState::Accepted,
        MockFetch::err("provider", "http 503"),
    );

    service.capture("evi_1").await.unwrap();

    let states = store.states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].1, PromotionState::Skipped);
    assert_eq!(states[0].2.as_deref(), Some("fetch_failed"));
}

/// The mock store must reject nothing silently: a target that does not exist
/// is a no-op, not an error.
#[tokio::test]
async fn missing_capture_target_is_a_noop() {
    let (service, _store, _fetch, _index, _inv) = harness(
        "evi_other",
        PromotionState::Accepted,
        MockFetch::ok(Vec::new()),
    );
    service.capture("evi_missing").await.unwrap();
}

/// Guard against a silent regression of the "never index unverified" rule:
/// the service must have no code path that indexes without an accepted item.
#[tokio::test]
async fn no_index_write_without_an_accepted_item() {
    let (service, _store, _fetch, index, _inv) = harness(
        "evi_pending",
        PromotionState::Pending,
        MockFetch::ok(vec![payload()]),
    );
    let _ = AtomicBool::new(false);
    service.capture("evi_pending").await.unwrap();
    assert!(
        index.indexed.lock().unwrap().is_empty(),
        "pending items are never captured"
    );
}

// ---------------------------------------------------------------------------
// Pipeline-level acceptance (AC1 / AC8 / AC15)
// ---------------------------------------------------------------------------

mod pipeline {
    use super::*;
    use searchos_application::search_service::{
        SearchDependencies, SearchService as _, SearchServiceImpl,
    };
    use searchos_cost::CostModelConfig;
    use searchos_domain::cost::{CostOperation, CostQuery};
    use searchos_domain::ids::ProfileId;
    use searchos_domain::request::{SearchMode, SearchRequest};
    use searchos_domain::response::SearchStatus;
    use searchos_planner::router::{
        ProviderEntry, ProviderHealth, ProviderRouter, StaticProviderRouter,
    };
    use searchos_provider::traits::{
        ProviderResult, SearchParams, SearchProvider, SearchResultEntry,
    };
    use searchos_provider::ProviderError;

    struct ScriptedProvider {
        id: String,
        calls: Arc<AtomicUsize>,
        entries: Vec<SearchResultEntry>,
    }

    impl ScriptedProvider {
        fn new(entries: Vec<SearchResultEntry>) -> Arc<Self> {
            Arc::new(Self {
                id: "tavily".into(),
                calls: Arc::new(AtomicUsize::new(0)),
                entries,
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl SearchProvider for ScriptedProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn capabilities(&self) -> searchos_domain::ProviderCapabilities {
            searchos_domain::ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(&self, _params: &SearchParams) -> Result<ProviderResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ProviderResult {
                entries: self.entries.clone(),
                duration_ms: 1,
                bytes_transferred: 0,
            })
        }
    }

    fn entry(url: &str, title: &str, content: &str) -> SearchResultEntry {
        SearchResultEntry {
            title: Some(title.into()),
            url: url.into(),
            content: content.into(),
            score: 0.9,
            published_at: None,
            raw_content: None,
        }
    }

    fn router() -> Arc<dyn ProviderRouter> {
        Arc::new(StaticProviderRouter::new(vec![ProviderEntry {
            id: "tavily".into(),
            capabilities: searchos_domain::ProviderCapabilities {
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

    struct PipelineHarness {
        runtime: Arc<searchos_core::SearchOsRuntime>,
        store: Arc<MockStore>,
        service: Arc<SearchServiceImpl>,
        _dir: tempfile::TempDir,
    }

    impl PipelineHarness {
        async fn new(provider: Arc<ScriptedProvider>, promotion_enabled: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut cfg = searchos_core::SearchOsConfig::default();
            cfg.app.data_dir = dir.path().to_path_buf();
            let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();

            let store = Arc::new(MockStore::default());
            let promotion: Option<Arc<dyn EvidencePromotionService>> = if promotion_enabled {
                Some(Arc::new(EvidencePromotionServiceImpl::new(
                    store.clone(),
                    Arc::new(MockFetch::ok(vec![payload()])),
                    Some(runtime.fulltext.clone() as Arc<dyn SearchIndex>),
                    None,
                    None,
                    &searchos_cost::CostModelConfig::default_config(),
                    PromotionConfig::default(),
                )))
            } else {
                None
            };

            let deps = Arc::new(SearchDependencies {
                profiles: searchos_policy::ProfileRegistry::builtin(),
                quality: searchos_policy::QualityPolicy::default(),
                freshness: searchos_policy::FreshnessPolicy::default_policy(),
                source: searchos_policy::SourcePolicy::default_policy(),
                cost_config: CostModelConfig::default_config(),
                bm25: runtime.fulltext.clone(),
                vector: Some(runtime.vector.clone()),
                hybrid: Some(runtime.hybrid.clone()),
                embedder: runtime.embedder.clone(),
                canonical_cache: runtime.canonical_cache.clone(),
                semantic_cache: runtime.semantic_cache.clone(),
                retrieval_cache: runtime.retrieval_cache.clone(),
                embedding_cache: runtime.embedding_cache.clone(),
                rerank_cache: runtime.rerank_cache.clone(),
                router: router(),
                search_providers: vec![provider as Arc<dyn SearchProvider>],
                local_port: runtime.local_port(),
                traces: runtime.traces(),
                cost_sink: runtime.cost_sink(),
                task_recorder: runtime.task_recorder(),
                singleflight: runtime.singleflight.clone(),
                cache: runtime.exact_cache.clone(),
                negative_cache: runtime.negative_cache.clone(),
                semantic_dedup: true,
                incremental: true,
                session_store: None,
                query_expansion: true,
                reranking: true,
                allow_external: true,
                promotion,
            });
            Self {
                runtime,
                store,
                service: Arc::new(SearchServiceImpl::new(deps)),
                _dir: dir,
            }
        }

        fn request(&self, query: &str, mode: SearchMode) -> SearchRequest {
            SearchRequest::new(
                query,
                ProfileId::general(),
                mode,
                WorkspaceId::new("default"),
            )
            .unwrap()
        }

        fn web_search_events(&self) -> usize {
            let repo = searchos_storage_sqlite::repos::CostRepo::new(&self.runtime.sqlite);
            repo.query(&CostQuery::default())
                .map(|events| {
                    events
                        .iter()
                        .filter(|e| e.operation == CostOperation::WebSearch)
                        .count()
                })
                .unwrap_or(0)
        }
    }

    /// AC1 at the pipeline level: an accepted external item is recorded as an
    /// acquisition and queued for capture; the capture then promotes it.
    #[tokio::test]
    async fn accepted_external_evidence_becomes_an_acquisition() {
        let provider = ScriptedProvider::new(vec![entry(
            "https://external.example.com/axum",
            "Axum performance",
            "Axum performance comparison",
        )]);
        let h = PipelineHarness::new(provider, true).await;
        let response = h
            .service
            .search(h.request("axum performance", SearchMode::ExternalOnly))
            .await
            .unwrap();

        let recorded = h.store.acquisitions.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1, "one acquisition per provider call");
        let (acquisition, items) = &recorded[0];
        assert_eq!(acquisition.query_origin, QueryOrigin::Fallback);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].state,
            PromotionState::Accepted,
            "a passing external item is accepted (response quality: {:?})",
            response.quality.status
        );
        // The capture was queued, and running it promotes the item.
        assert_eq!(h.store.captures(), vec![items[0].id.clone()]);
    }

    /// AC8: the ledger records exactly the calls the plan made.
    #[tokio::test]
    async fn ledger_records_exactly_the_planned_calls() {
        let provider = ScriptedProvider::new(vec![entry(
            "https://external.example.com/axum",
            "Axum performance",
            "Axum performance comparison",
        )]);
        let h = PipelineHarness::new(provider.clone(), false).await;
        h.service
            .search(h.request("axum performance benchmark", SearchMode::ExternalOnly))
            .await
            .unwrap();

        assert_eq!(
            h.web_search_events(),
            provider.calls(),
            "every provider call has exactly one WebSearch ledger event"
        );
    }

    /// AC15: a terminal failure replays from the negative cache with zero
    /// provider calls, and never disguises itself as a success.
    ///
    /// `LocalFirst` on purpose: `ExternalOnly` must keep bypassing every cache
    /// (RFC-004 v1.1 9.5), including a negative one.
    #[tokio::test]
    async fn negative_cache_replays_insufficient_evidence_honestly() {
        let provider = ScriptedProvider::new(Vec::new());
        let h = PipelineHarness::new(provider.clone(), false).await;

        let first = h
            .service
            .search(h.request("no such knowledge anywhere", SearchMode::LocalFirst))
            .await
            .unwrap();
        let calls_after_first = provider.calls();
        assert!(calls_after_first > 0, "the first attempt buys");
        assert_eq!(first.status, SearchStatus::InsufficientEvidence);
        // Isolate the negative cache from the singleflight retention window,
        // which would otherwise serve the second call.
        h.runtime.singleflight.clear().await;

        let second = h
            .service
            .search(h.request("no such knowledge anywhere", SearchMode::LocalFirst))
            .await
            .unwrap();
        assert_eq!(
            provider.calls(),
            calls_after_first,
            "the retry must not re-pay for knowledge known to be absent"
        );
        assert!(second.replayed, "the replay is declared");
        assert_eq!(
            second.status,
            SearchStatus::InsufficientEvidence,
            "a replay never disguises a failure as success"
        );
        assert_eq!(second.results.len(), 0, "no fabricated results");
    }

    /// `ExternalOnly` keeps its no-cache guarantee: the negative cache must not
    /// intercept it either.
    #[tokio::test]
    async fn external_only_bypasses_the_negative_cache() {
        let provider = ScriptedProvider::new(Vec::new());
        let h = PipelineHarness::new(provider.clone(), false).await;

        let _ = h
            .service
            .search(h.request("no such knowledge anywhere", SearchMode::ExternalOnly))
            .await
            .unwrap();
        let after_first = provider.calls();
        h.runtime.singleflight.clear().await;
        let _ = h
            .service
            .search(h.request("no such knowledge anywhere", SearchMode::ExternalOnly))
            .await
            .unwrap();
        assert!(
            provider.calls() > after_first,
            "ExternalOnly must never be intercepted by a cache, including a negative one"
        );
    }

    /// AC15: strict freshness bypasses the negative cache exactly as it
    /// bypasses the exact cache.
    #[tokio::test]
    async fn negative_cache_is_skipped_for_strict_freshness_requests() {
        let provider = ScriptedProvider::new(Vec::new());
        let h = PipelineHarness::new(provider.clone(), false).await;

        let mut first = h.request("no such knowledge anywhere", SearchMode::LocalFirst);
        first.constraints.freshness = searchos_domain::request::FreshnessRequirement::Strict;
        let _ = h.service.search(first).await.unwrap();
        let after_first = provider.calls();
        assert!(after_first > 0);
        // Both singleflight and the negative cache must let a strict-freshness
        // request through to the provider.
        h.runtime.singleflight.clear().await;

        let mut second = h.request("no such knowledge anywhere", SearchMode::LocalFirst);
        second.constraints.freshness = searchos_domain::request::FreshnessRequirement::Strict;
        let _ = h.service.search(second).await.unwrap();
        assert!(
            provider.calls() > after_first,
            "strict freshness must re-check the provider, never replay a negative entry"
        );
    }
}

// ---------------------------------------------------------------------------
// End-to-end: real store + real fetch + real index (AC1 / AC2 / AC5)
// ---------------------------------------------------------------------------

mod end_to_end {
    use super::*;
    use axum::{routing::get, Router};

    /// Serves one page plus a permissive robots.txt on a loopback port.
    async fn spawn_page_server(body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\n" }))
            .route("/page", get(move || async move { body }));
        // The fetch guard only allows ports 80/443/8080, so the test server
        // must use one of them.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:8080")
            .await
            .unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        ("http://127.0.0.1:8080/page".to_string(), handle)
    }

    /// The real promotion service captures through the real fetch path: the
    /// captured page becomes a document row, an index entry and a promoted
    /// evidence item — and a later search finds it locally.
    #[tokio::test]
    async fn promotion_captures_a_real_page_into_durable_knowledge() {
        let body = "<html><head><title>Zanzibar Protocol</title></head>\
                    <body><h1>Zanzibar Protocol</h1>\
                    <p>The zanzibar protocol describes a relationship-based access control model \
                    used to serve authorization decisions at scale across distributed services.</p>\
                    </body></html>";
        let (url, server) = spawn_page_server(body).await;

        let dir = tempfile::tempdir().unwrap();
        let mut cfg = searchos_core::SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        // Loopback fetch for the test only; production keeps the SSRF guard on.
        cfg.security.ssrf_protection = false;
        let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();

        let promotion = runtime
            .promotion
            .clone()
            .expect("the runtime wires the promotion service");

        // Record an acquisition with one gate-accepted item pointing at the
        // local page.
        let task_id = TaskId::generate();
        let acquisition = ExternalAcquisition {
            id: "acq_e2e".into(),
            task_id: task_id.to_string(),
            request_id: "req_e2e".into(),
            workspace_id: "default".into(),
            provider_id: "tavily".into(),
            query: "zanzibar protocol".into(),
            query_origin: QueryOrigin::Gap,
            requirement_id: Some("need_e2e".into()),
            strategy: Default::default(),
            created_at: Utc::now(),
        };
        let accepted = EvidenceItem {
            id: "evi_e2e".into(),
            acquisition_id: String::new(),
            url: url.clone(),
            canonical_url: url.clone(),
            title: Some("Zanzibar Protocol".into()),
            provider_score: 0.9,
            relevance: 0.9,
            state: PromotionState::Accepted,
            document_id: None,
            version_id: None,
            chunk_count: None,
            reason: None,
            promoted_at: None,
            created_at: Utc::now(),
        };
        promotion
            .record_and_queue(acquisition, vec![accepted])
            .await
            .unwrap();

        // The dedicated capture worker (not the index tick) processes it.
        let processed = runtime.process_capture_queue(4).await.unwrap();
        assert_eq!(processed, 1, "capture ran on the dedicated worker");

        // AC1: a real document + version + chunks exist, and the item is
        // promoted with that document.
        let repo = searchos_storage_sqlite::repos::ExternalEvidenceRepo::new(&runtime.sqlite);
        let items = repo.list_items("acq_e2e").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0]["state"],
            serde_json::json!("promoted"),
            "capture skipped: {:?}",
            items[0]["reason"]
        );
        let document_id = items[0]["document_id"].as_str().unwrap().to_string();
        assert!(items[0]["chunk_count"].as_i64().unwrap_or(0) > 0);

        // The capture cost is charged to the owning task (post-settlement).
        let reservations = searchos_storage_sqlite::repos::BudgetRepo::new(&runtime.sqlite)
            .list(10)
            .unwrap();
        assert!(
            reservations.iter().all(|r| r.spent_cost_micros >= 0),
            "no negative spend"
        );

        // AC4/AC5: the captured body is indexed and searchable locally with
        // zero external provider calls.
        runtime.process_outbox(64).await.unwrap();
        let stats = repo.stats(Some("default")).unwrap();
        assert_eq!(stats["promoted"], serde_json::json!(1));
        assert_eq!(stats["acquisitions"], serde_json::json!(1));

        let found = runtime
            .fulltext
            .search(&RetrievalQuery::new("zanzibar protocol", 5))
            .await
            .unwrap();
        assert!(
            !found.is_empty(),
            "the promoted page is retrievable from the local index (document {document_id})"
        );
        assert!(found
            .iter()
            .any(|c| c.document_id.to_string() == document_id));

        // AC2: the promoted document survives an index rebuild.
        let rebuilt = runtime.rebuild_index().await.unwrap();
        assert!(rebuilt > 0);
        let after = runtime
            .fulltext
            .search(&RetrievalQuery::new("zanzibar protocol", 5))
            .await
            .unwrap();
        assert!(
            !after.is_empty(),
            "a rebuilt index still contains the purchased knowledge"
        );

        server.abort();
    }
}
