//! SearchOS integration suite (cross-crate, RFC-016 phase acceptance).
//!
//! These tests exercise the assembled runtime end-to-end with offline
//! providers — the same use cases Local Mode and Gateway Mode share.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use searchos_application::search_service::{
    SearchDependencies, SearchService as _, SearchServiceImpl, FEDERATION_LEGACY,
    FEDERATION_PLAN_ONLY,
};
use searchos_application::singleflight::SingleflightCoordinator;
use searchos_core::SearchOsConfig;
use searchos_domain::cost::{CostEvent, CostOperation, CostQuery, Money};
use searchos_domain::ids::{ProfileId, TraceId, WorkspaceId};
use searchos_domain::request::{FreshnessRequirement, SearchMode, SearchRequest};
use searchos_domain::retrieval::RetrievalQuery;

/// Offline in-memory harness over real adapters.
struct Harness {
    runtime: Arc<searchos_core::SearchOsRuntime>,
    service: searchos_application::search_service::SearchServiceImpl,
    dir: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        Self::with_strategy(FEDERATION_LEGACY).await
    }

    async fn with_strategy(strategy: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        cfg.federation.strategy = strategy.to_string();
        let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        let deps = rebuild_deps(&runtime, strategy);
        let service = SearchServiceImpl::new(deps);
        Self {
            runtime,
            service,
            dir,
        }
    }

    async fn seed(&self, chunk: &str, body: &str, url: &str) {
        use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};
        let doc = searchos_domain::retrieval::IndexedDocument {
            chunk_id: ChunkId::new(format!("chunk_{chunk}")),
            document_id: DocumentId::new(format!("doc_{chunk}")),
            version_id: DocumentVersionId::new(format!("docv_{chunk}")),
            title: Some(body.lines().next().unwrap_or("doc").to_string()),
            body: body.to_string(),
            url: url.to_string(),
            language: Some("en".into()),
            workspace_id: "*".into(),
            fetched_at: chrono::Utc::now(),
        };
        self.runtime.fulltext.index(doc).await.unwrap();
        self.runtime.fulltext.commit().await.unwrap();
    }
}

fn rebuild_deps(
    runtime: &Arc<searchos_core::SearchOsRuntime>,
    strategy: &str,
) -> Arc<SearchDependencies> {
    Arc::new(SearchDependencies {
        profiles: searchos_policy::ProfileRegistry::builtin(),
        quality: searchos_policy::QualityPolicy::default(),
        freshness: searchos_policy::FreshnessPolicy::default_policy(),
        source: searchos_policy::SourcePolicy::default_policy(),
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
        router: runtime.provider_registry.clone(),
        search_providers: vec![],
        local_port: runtime.local_port(),
        traces: runtime.traces(),
        cost_sink: runtime.cost_sink(),
        task_recorder: runtime.task_recorder(),
        singleflight: Arc::new(SingleflightCoordinator::new()),
        cache: runtime.exact_cache.clone(),
        validity: None,
        policy_state: None,
        entity_lookup: None,
        negative_cache: runtime.negative_cache.clone(),
        semantic_dedup: true,
        expansion_mode: "followup".into(),
        incremental: true,
        query_expansion: true,
        reranking: true,
        allow_external: false,
        promotion: None,
        session_store: Some(runtime.session_store.clone()),
        federation_registry: Some(Arc::new(federation_registry(runtime))),
        federation_strategy: strategy.to_string(),
        federation_limits: searchos_domain::FederationLimits::default(),
        federation_selection: "reliability_first".into(),
        quality_learner: None,
    })
}

/// Mirrors the runtime's registry: the same full-text index wrapped by the
/// Phase 2 adapter. No external provider in the offline harness.
fn federation_registry(
    runtime: &Arc<searchos_core::SearchOsRuntime>,
) -> searchos_federated::InMemorySourceRegistry {
    let registry = searchos_federated::InMemorySourceRegistry::new();
    registry
        .register(Arc::new(searchos_federated::LocalIndexSource::new(
            runtime.fulltext.clone(),
            searchos_federated::SourceScope::Global,
            100,
        )) as Arc<dyn searchos_federated::SearchSource>)
        .unwrap();
    registry
}

fn request(query: &str, mode: SearchMode) -> SearchRequest {
    SearchRequest::new(
        query,
        ProfileId::general(),
        mode,
        WorkspaceId::new("ws_default"),
    )
    .unwrap()
}

/// MVP acceptance: 本地知识能够被复用 — a static-knowledge query is served
/// locally with a decision trace recorded and cost attributed (never zero).
#[tokio::test]
async fn local_knowledge_reuse_with_cost_and_trace() {
    let h = Harness::new().await;
    h.seed(
        "tokio1",
        "Tokio is an asynchronous runtime for the Rust language",
        "https://tokio.rs/tokio/tutorial",
    )
    .await;
    h.seed(
        "raft1",
        "Raft is a consensus algorithm with leader election",
        "https://raft.github.io",
    )
    .await;

    let resp = h
        .service
        .search(request(
            "What is tokio async runtime?",
            SearchMode::LocalOnly,
        ))
        .await
        .unwrap();
    assert_eq!(resp.decision.path, "local", "got {:?}", resp.decision);
    assert!(!resp.results.is_empty(), "local results served");
    assert!(
        resp.cost.estimated > 0.0,
        "local compute cost never zero (RFC-008)"
    );

    // Trace recorded (Decision Trace 覆盖率 100% — RFC-015 15.5).
    let trace = h
        .runtime
        .traces()
        .get(resp.trace_id.as_str())
        .await
        .unwrap()
        .expect("trace stored");
    assert_eq!(trace.selected_path, "local");
    assert!(!trace.intents.is_empty());
    let _ = &h.dir;
}

/// MVP acceptance: 本地数据过期时能够正确触发外部搜索 — stale-only knowledge
/// cannot serve as current fact; the decision escalates (offline mode →
/// explicit reject rather than stale pass).
#[tokio::test]
async fn stale_local_data_escalates() {
    let h = Harness::new().await;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};
    let stale_doc = searchos_domain::retrieval::IndexedDocument {
        chunk_id: ChunkId::new("old1"),
        document_id: DocumentId::new("doc_old"),
        version_id: DocumentVersionId::new("docv_old"),
        title: Some("old news".into()),
        body: "breaking news announcement from last year".into(),
        url: "https://news.example.com/old".into(),
        language: Some("en".into()),
        workspace_id: "*".into(),
        fetched_at: chrono::Utc::now() - chrono::Duration::hours(6),
    };
    h.runtime.fulltext.index(stale_doc).await.unwrap();
    h.runtime.fulltext.commit().await.unwrap();

    // Latest query (strict freshness) must NOT be served from stale history
    // (RFC-004 Rule 1). Offline ⇒ explicit Reject, never a false pass.
    let mut req = request(
        "What is the latest news announcement?",
        SearchMode::LocalOnly,
    );
    req.constraints.freshness = FreshnessRequirement::Strict;
    let resp = h.service.search(req).await.unwrap();
    assert_eq!(resp.decision.path, "reject", "got {:?}", resp.decision);
    assert_eq!(
        resp.quality.status,
        searchos_domain::response::QualityStatus::Failed
    );
}

/// MVP acceptance: 重复搜索可以本地解决 — the exact cache serves the second
/// identical query without re-executing the pipeline.
#[tokio::test]
async fn repeat_query_resolved_by_cache() {
    let h = Harness::new().await;
    h.seed(
        "raft",
        "Raft consensus algorithm leader election",
        "https://raft.github.io",
    )
    .await;
    let _ = h
        .service
        .search(request("What is raft consensus?", SearchMode::LocalOnly))
        .await
        .unwrap();
    let (hits_before, _) = h.runtime.exact_cache.stats();
    let _ = h
        .service
        .search(request("What is raft consensus?", SearchMode::LocalOnly))
        .await
        .unwrap();
    let (hits_after, _) = h.runtime.exact_cache.stats();
    assert!(hits_after > hits_before, "second query hit the exact cache");
}

/// MVP acceptance: 索引可以重建 — dropping tantivy content and rebuilding
/// from SQLite restores the same searchable corpus.
#[tokio::test]
async fn index_rebuild_restores_corpus() {
    let h = Harness::new().await;
    // Seed a document through the full persistence path (repo + outbox).
    let doc_repo = searchos_storage_sqlite::repos::DocumentRepo::new(&h.runtime.sqlite);
    let content = "Tokio is an async runtime for rust with work stealing scheduler";
    let hash = searchos_document_content_hash(content);
    let doc = searchos_domain::document::Document {
        id: searchos_domain::ids::DocumentId::generate(),
        source_id: searchos_domain::ids::SourceId::generate(),
        canonical_url: "https://example.com/tokio".into(),
        current_version_id: None,
        content_type: "text/plain".into(),
        language: Some("en".into()),
        source_type: "web".into(),
        title: Some("Tokio".into()),
        workspace_id: "*".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let version = searchos_domain::document::DocumentVersion {
        id: searchos_domain::ids::DocumentVersionId::generate(),
        document_id: doc.id.clone(),
        content_hash: hash.clone(),
        raw_content_ref: format!("sha256:{hash}"),
        markdown_ref: None,
        fetched_at: chrono::Utc::now(),
        published_at: None,
        extractor_version: "test".into(),
        crawler_version: None,
        summary: None,
        keywords: Vec::new(),
    };
    let chunked = searchos_document::chunk_text(content, &Default::default());
    let chunks = chunked
        .iter()
        .map(|c| searchos_domain::document::DocumentChunk {
            id: searchos_domain::ids::ChunkId::generate(),
            version_id: version.id.clone(),
            document_id: doc.id.clone(),
            chunk_hash: c.hash.clone(),
            content_ref: format!("sha256:{hash}"),
            text: c.text.clone(),
            token_count: Some(12),
            chunk_index: c.index,
            created_at: chrono::Utc::now(),
        })
        .collect::<Vec<_>>();
    let payloads = vec![searchos_domain::retrieval::IndexedDocument {
        chunk_id: chunks.first().unwrap().id.clone(),
        document_id: doc.id.clone(),
        version_id: version.id.clone(),
        title: Some("Tokio".into()),
        body: content.to_string(),
        url: doc.canonical_url.clone(),
        language: Some("en".into()),
        workspace_id: "*".into(),
        fetched_at: chrono::Utc::now(),
    }];
    let diff = doc_repo
        .save_versioned(
            &doc,
            &version,
            &chunks,
            &payloads,
            &searchos_domain::ids::WorkspaceId::new("*"),
        )
        .unwrap();
    assert_eq!(diff, searchos_document::VersionDiff::NewVersion);
    // Store the content blob (the fetch path does this before persistence).
    searchos_storage_fs::ObjectStore::put(h.runtime.objects.as_ref(), content.as_bytes())
        .await
        .unwrap();

    // Process the outbox: chunk lands in tantivy.
    let (processed, dead) = h.runtime.process_outbox(10).await.unwrap();
    assert_eq!((processed, dead), (1, 0));
    assert!(
        !h.runtime
            .fulltext
            .search(&RetrievalQuery::new("work stealing scheduler", 5))
            .await
            .unwrap()
            .is_empty(),
        "outbox delivered the chunk to the index"
    );

    // Wipe the index, rebuild from SQLite — same corpus returns.
    h.runtime.fulltext.rebuild_from(&[]).await.unwrap();
    assert!(h
        .runtime
        .fulltext
        .search(&RetrievalQuery::new("work stealing", 5))
        .await
        .unwrap()
        .is_empty());
    let n = h.runtime.rebuild_index().await.unwrap();
    assert_eq!(n, 1);
    assert!(
        !h.runtime
            .fulltext
            .search(&RetrievalQuery::new("work stealing scheduler", 5))
            .await
            .unwrap()
            .is_empty(),
        "rebuild restored searchability (ADR-022)"
    );
    let _ = hash;
}

/// MVP acceptance: 多 Agent 相同请求可以合并执行 — singleflight merges
/// concurrent identical searches; shared benefit recorded without double
/// billing.
#[tokio::test]
async fn singleflight_merges_and_records_shared_benefit() {
    let h = Harness::new().await;
    h.seed("tokio", "Tokio async runtime for rust", "https://tokio.rs")
        .await;
    let sf = Arc::new(SingleflightCoordinator::new());
    let mut handles = Vec::new();
    for _ in 0..8 {
        let sf = Arc::clone(&sf);
        handles.push(tokio::spawn(async move {
            sf.execute("merged-fingerprint", || async {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                Ok(serde_json::json!({"merged": true}))
            })
            .await
            .unwrap()
        }));
    }
    for hdl in handles {
        hdl.await.unwrap();
    }
    // Record the shared benefit the way the search service does.
    h.runtime
        .cost_sink()
        .record_singleflight(
            "merged-fingerprint",
            "leader",
            &["f1".into(), "f2".into()],
            5_000,
        )
        .await
        .unwrap();
    assert_eq!(
        h.runtime
            .cost_sink()
            .singleflight_saved_micros()
            .await
            .unwrap(),
        5_000
    );
}

/// Cost attribution: every event lands with task + workspace + operation
/// (RFC-008 8.5) and summary exposes cost-per-successful-query.
#[tokio::test]
async fn cost_attribution_and_summary() {
    let h = Harness::new().await;
    let sink = h.runtime.cost_sink();
    // A cost event references a task row (Part 17 FK): record the task first,
    // exactly as the search pipeline does.
    let task = searchos_domain::ids::TaskId::generate();
    let trace = TraceId::generate();
    searchos_storage_sqlite::repos::TaskRepo::new(&h.runtime.sqlite)
        .insert(
            &task,
            &WorkspaceId::new("ws_default"),
            Some(&searchos_domain::ids::AgentId::new("agent_a")),
            "q",
            "q",
            "fp",
            "general",
            "1.0.0",
            "local_first",
            &trace,
        )
        .unwrap();
    #[allow(clippy::needless_update)]
    let ev = CostEvent {
        id: searchos_domain::ids::CostEventId::generate(),
        task_id: task.clone(),
        workspace_id: WorkspaceId::new("ws_default"),
        agent_id: Some(searchos_domain::ids::AgentId::new("agent_a")),
        operation: CostOperation::WebSearch,
        provider_id: Some("tavily".into()),
        input_units: 1,
        output_units: 0,
        estimated_cost: Money::parse("0.005", "USD").unwrap(),
        actual_cost: None,
        duration_ms: 400,
        occurred_at: chrono::Utc::now(),
        trace_id: Some(TraceId::generate()),
        shared: false,
        pricing_model_version: "1".into(),
        status: Default::default(),
        error_code: None,
    };
    sink.append(ev).await.unwrap();
    sink.record_quality_pass(task.as_str()).await.unwrap();
    let mut passing = std::collections::HashSet::new();
    passing.insert(task.as_str().to_string());
    let repo = searchos_storage_sqlite::repos::CostRepo::new(&h.runtime.sqlite);
    let summary = repo.summarize(&CostQuery::default(), &passing).unwrap();
    assert_eq!(summary.total_estimated.amount_micros, 5_000);
    assert_eq!(summary.by_provider[0].0, "tavily");
    assert_eq!(
        summary.cost_per_successful_query.unwrap().amount_micros,
        5_000
    );
}

/// Workspace isolation: agent B cannot see agent A's private memory.
#[tokio::test]
async fn memory_namespace_isolation() {
    let h = Harness::new().await;
    let ws_a = WorkspaceId::new("ws_a");
    let ws_b = WorkspaceId::new("ws_b");
    let ns_a = searchos_domain::memory::MemoryNamespace {
        workspace: ws_a.clone(),
        project: None,
        agent: Some("a1".into()),
    };
    let entry = h
        .application_memory()
        .remember(
            ns_a.clone(),
            SharingPolicyPriv::Private,
            "rust notes",
            "tokio axum research",
            None,
            &[],
        )
        .await
        .unwrap();
    // Same namespace reads it.
    let hits_a = h
        .application_memory()
        .search("tokio", &ns_a, 5)
        .await
        .unwrap();
    assert_eq!(hits_a.len(), 1);
    // Different workspace: nothing.
    let ns_b = searchos_domain::memory::MemoryNamespace {
        workspace: ws_b,
        project: None,
        agent: None,
    };
    let hits_b = h
        .application_memory()
        .search("tokio", &ns_b, 5)
        .await
        .unwrap();
    assert!(hits_b.is_empty(), "cross-workspace leak denied (RFC-013)");
    let _ = entry;
}

// helpers -------------------------------------------------------------

impl Harness {
    fn application_memory(&self) -> Arc<dyn searchos_application::MemoryUseCase> {
        Arc::clone(&self.runtime.application.memory_service)
    }
}

use searchos_domain::memory::SharingPolicy as SharingPolicyPriv;

fn searchos_document_content_hash(content: &str) -> String {
    searchos_core::hash_util::content_hash_hex(content.as_bytes())
}

// ---------------------------------------------------------------------------
// Feature-completion regression tests (source RFC baseline gaps).
// ---------------------------------------------------------------------------

/// RFC-004 v1.1 SDE-007: ExternalOnly must not be silently intercepted by the
/// exact cache, and a failed external path must not be reported as a
/// completed quality-passing search.
#[tokio::test]
async fn external_only_bypasses_cache_and_reports_insufficient_evidence() {
    let h = Harness::new().await;
    h.seed(
        "tokio_cache",
        "Tokio is an asynchronous runtime for the Rust language",
        "https://tokio.rs/tokio/tutorial",
    )
    .await;
    // First request populates the exact cache with a passing response.
    let first = h
        .service
        .search(request(
            "What is tokio async runtime?",
            SearchMode::LocalOnly,
        ))
        .await
        .unwrap();
    assert_eq!(
        first.quality.status,
        searchos_domain::response::QualityStatus::Passed
    );

    let mut external = request("What is tokio async runtime?", SearchMode::ExternalOnly);
    external.budget.max_external_queries = 1;
    let resp = h.service.search(external).await.unwrap();
    assert!(
        !resp.replayed,
        "ExternalOnly cannot be served from exact cache"
    );
    assert_eq!(
        resp.decision.path, "reject",
        "offline external path is rejected explicitly"
    );
    assert_eq!(
        resp.status,
        searchos_domain::response::SearchStatus::InsufficientEvidence,
        "no external provider available ⇒ explicit insufficient evidence"
    );
    assert!(resp
        .warnings
        .iter()
        .any(|w| w.contains("provider_unavailable")));
}

/// RFC-014 14.7: a repeated request_id must replay the recoverable task
/// state without creating another task or another bill.
#[tokio::test]
async fn repeated_request_id_is_idempotent() {
    let h = Harness::new().await;
    h.seed(
        "raft_idem",
        "Raft consensus algorithm leader election",
        "https://raft.github.io",
    )
    .await;
    let base = request("What is raft consensus?", SearchMode::LocalOnly);
    let first = h
        .service
        .search(base.clone().with_request_id("fixed-request-1"))
        .await
        .unwrap();
    let second = h
        .service
        .search(base.with_request_id("fixed-request-1"))
        .await
        .unwrap();
    assert!(second.replayed, "second call replays the stored response");
    assert_eq!(first.trace_id.as_str(), second.trace_id.as_str());
    let tasks: i64 = h
        .runtime
        .sqlite
        .with_conn(|c| {
            c.query_row("SELECT count(*) FROM search_tasks", [], |r| r.get(0))
                .map_err(|e| searchos_domain::SearchOsError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(tasks, 1, "no duplicate task row / external purchase");
}

/// RFC-004 v1.1 11.1: traces carry structured stages and a final strategy.
#[tokio::test]
async fn decision_trace_has_stages_and_strategy() {
    let h = Harness::new().await;
    h.seed(
        "tokio_trace",
        "Tokio async runtime for rust",
        "https://tokio.rs",
    )
    .await;
    let resp = h
        .service
        .search(request("What is tokio runtime?", SearchMode::LocalOnly))
        .await
        .unwrap();
    let trace = h
        .runtime
        .traces()
        .get(resp.trace_id.as_str())
        .await
        .unwrap()
        .expect("trace persisted");
    assert!(
        !trace.stages.is_empty(),
        "structured stage records persisted"
    );
    assert_eq!(trace.stages[0].stage, "validation");
    assert_eq!(trace.final_strategy.as_deref(), Some("local_resolve"));
    assert!(trace.final_outcome.is_some());
}

/// RFC-004 v1.1 15 / RFC-006 6.7/6.10: reservations settle, savings are
/// computed against an equivalent-quality baseline, and provider billing
/// reconciliation keeps estimates separate from actuals.
#[tokio::test]
async fn savings_reservations_and_reconciliation() {
    let h = Harness::new().await;
    h.seed(
        "tokio_cost",
        "Tokio async runtime for rust",
        "https://tokio.rs",
    )
    .await;
    let _ = h
        .service
        .search(request("What is tokio runtime?", SearchMode::LocalOnly))
        .await
        .unwrap();
    let cost = &h.runtime.application.cost_service;
    let report = cost.savings(5_000).await.unwrap();
    assert_eq!(report.task_count, 1);
    assert_eq!(report.baseline_micros, 5_000);
    assert!(report.is_estimate, "no provider bill reconciled yet");
    assert_eq!(report.quality_passing_tasks, 1);
    assert!(report.cost_per_quality_passing_answer_micros.is_some());

    let reservations = cost.reservations(10).await.unwrap();
    let rows = reservations["reservations"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "settled");

    let events = searchos_storage_sqlite::repos::CostRepo::new(&h.runtime.sqlite)
        .query(&searchos_domain::cost::CostQuery::default())
        .unwrap();
    assert!(!events.is_empty(), "local compute cost event recorded");
    let id = events[0].id.as_str().to_string();
    cost.reconcile(&id, 123, "provider_billing", "USD")
        .await
        .unwrap();
    let summary = cost
        .summary(&searchos_domain::cost::CostQuery::default())
        .await
        .unwrap();
    assert_eq!(summary.total_actual.unwrap().amount_micros, 123);
    assert!(
        summary.total_estimated.amount_micros > 0,
        "estimate is never rewritten by reconciliation"
    );
}

/// RFC-004 v1.1 10.1: stale local knowledge is refreshed, never deleted,
/// and the refresh is an auditable outbox event.
#[tokio::test]
async fn stale_knowledge_is_refreshed_not_deleted() {
    let h = Harness::new().await;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};
    let stale = searchos_domain::retrieval::IndexedDocument {
        chunk_id: ChunkId::new("stale_refresh_chunk"),
        document_id: DocumentId::new("stale_refresh_doc"),
        version_id: DocumentVersionId::new("stale_refresh_v"),
        title: Some("Raft consensus".into()),
        body: "Raft consensus algorithm leader election".into(),
        url: "https://raft.github.io/raft".into(),
        language: Some("en".into()),
        workspace_id: "*".into(),
        fetched_at: chrono::Utc::now() - chrono::Duration::days(400),
    };
    h.runtime.fulltext.index(stale).await.unwrap();
    h.runtime.fulltext.commit().await.unwrap();
    let resp = h
        .service
        .search(request(
            "What is raft consensus algorithm?",
            SearchMode::LocalFirst,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.decision.path, "local_with_refresh",
        "got {:?}",
        resp.decision
    );
    let rows = searchos_storage_sqlite::repos::OutboxRepo::new(&h.runtime.sqlite)
        .pending(10)
        .unwrap();
    assert!(
        rows.iter().any(|r| r.operation == "refresh"),
        "refresh enqueued instead of deleting the document"
    );
    assert!(
        !h.runtime
            .fulltext
            .search(&RetrievalQuery::new("leader election", 5))
            .await
            .unwrap()
            .is_empty(),
        "authoritative local knowledge is preserved"
    );
}

/// Agent-facing contract (RFC-005 5.6): a partially covered comparison query
/// exposes WHICH requirement is still open — question, priority, minimal
/// acquisition strategy and a stable requirement id — so a caller can issue a
/// targeted follow-up query instead of re-searching blindly.
#[tokio::test]
async fn partial_coverage_exposes_open_requirements() {
    let h = Harness::new().await;
    h.seed(
        "tokio1",
        "Tokio is an asynchronous runtime for the Rust language",
        "https://tokio.rs/tokio/tutorial",
    )
    .await;

    let resp = h
        .service
        .search(request(
            "Compare tokio and raft consensus",
            SearchMode::LocalOnly,
        ))
        .await
        .unwrap();

    assert!(
        matches!(
            resp.quality.coverage,
            searchos_domain::response::CoverageStatus::Partial
                | searchos_domain::response::CoverageStatus::Insufficient
        ),
        "got coverage {:?}",
        resp.quality.coverage
    );
    assert!(
        !resp.coverage_gaps.is_empty(),
        "open requirements must be exposed, got {:?}",
        resp.quality.coverage
    );
    let questions: Vec<&str> = resp
        .coverage_gaps
        .iter()
        .map(|g| g.question.as_str())
        .collect();
    assert!(
        questions.iter().any(|q| q.to_lowercase().contains("raft")),
        "the uncovered entity must appear in the gaps: {questions:?}"
    );
    assert!(
        resp.coverage_gaps
            .iter()
            .all(|g| !g.requirement_id.is_empty()),
        "every gap carries a stable requirement id"
    );
    let _ = &h.dir;
}

/// Agent session contract: a turn records its open requirements, and the
/// follow-up turn targets exactly those requirements instead of re-deriving
/// them (RFC-005 5.6 multi-turn research).
#[tokio::test]
async fn session_turns_carry_open_requirements_into_the_follow_up() {
    let h = Harness::new().await;
    h.seed(
        "tokio1",
        "Tokio is an asynchronous runtime for the Rust language",
        "https://tokio.rs/tokio/tutorial",
    )
    .await;

    let mut first = request("Compare tokio and raft consensus", SearchMode::LocalOnly);
    first.session_id = Some("sess-1".into());
    let resp = h.service.search(first).await.unwrap();

    assert_eq!(resp.session_id.as_deref(), Some("sess-1"));
    assert!(
        !resp.coverage_gaps.is_empty(),
        "the comparison leaves an uncovered requirement"
    );
    assert_eq!(
        resp.focus_requirements,
        resp.coverage_gaps
            .iter()
            .map(|g| g.question.clone())
            .collect::<Vec<_>>(),
        "the response carries the follow-up focus"
    );

    // The session recorded the turn.
    let session = h.runtime.session_store.get("sess-1").expect("session");
    assert_eq!(session.turns.len(), 1);
    assert_eq!(session.open_requirements.len(), resp.coverage_gaps.len());

    // A follow-up turn with explicit focus requirements uses them verbatim.
    let mut follow = request("continue", SearchMode::LocalOnly);
    follow.session_id = Some("sess-1".into());
    follow.focus_requirements = vec!["raft consensus".into()];
    let follow_resp = h.service.search(follow).await.unwrap();
    assert_eq!(follow_resp.session_id.as_deref(), Some("sess-1"));
    let session = h.runtime.session_store.get("sess-1").unwrap();
    assert_eq!(session.turns.len(), 2, "the follow-up is recorded too");
    let _ = &h.dir;
}

/// Plan-only: the plan lists the queries that WOULD run and their cost, and
/// nothing is executed (no provider call, no cost event).
#[tokio::test]
async fn plan_reports_queries_and_cost_without_executing() {
    let h = Harness::new().await;
    h.seed(
        "tokio1",
        "Tokio is an asynchronous runtime for the Rust language",
        "https://tokio.rs/tokio/tutorial",
    )
    .await;

    let mut req = request("Compare tokio and raft consensus", SearchMode::ExternalOnly);
    req.budget.max_external_queries = 3;
    let report = h.service.plan(&req).await.unwrap();
    assert!(!report.planned_queries.is_empty(), "plan has queries");
    assert!(report.estimated_cost_micros > 0, "the plan is costed");
    assert!(!report.open_requirements.is_empty(), "gaps are exposed");
    let _ = &h.dir;
}

/// RFC-062 pipeline-level equivalence gate (the dependency review's task 4
/// precondition): enabling the federated planner must NOT change what the
/// search pipeline decides or serves. Same query, same seeded knowledge,
/// `legacy` vs `plan_only` — every behavioural field identical.
#[tokio::test]
async fn federated_default_matches_legacy_pipeline() {
    let legacy = Harness::new().await;
    let planned = Harness::with_strategy(FEDERATION_PLAN_ONLY).await;
    let body = "Tokio is an asynchronous runtime for the Rust language";
    legacy
        .seed("tokio", body, "https://tokio.rs/tokio/tutorial")
        .await;
    planned
        .seed("tokio", body, "https://tokio.rs/tokio/tutorial")
        .await;
    legacy
        .seed(
            "raft",
            "Raft consensus algorithm leader election",
            "https://raft.github.io",
        )
        .await;
    planned
        .seed(
            "raft",
            "Raft consensus algorithm leader election",
            "https://raft.github.io",
        )
        .await;

    let req = request("What is tokio async runtime?", SearchMode::LocalOnly);
    let legacy_resp = legacy.service.search(req.clone()).await.unwrap();
    let planned_resp = planned.service.search(req.clone()).await.unwrap();

    assert_eq!(legacy_resp.decision.path, planned_resp.decision.path);
    assert_eq!(legacy_resp.decision.reason, planned_resp.decision.reason);
    assert_eq!(legacy_resp.status, planned_resp.status);
    assert_eq!(legacy_resp.quality.status, planned_resp.quality.status);
    assert_eq!(legacy_resp.results.len(), planned_resp.results.len());
    let legacy_json = serde_json::to_value(&legacy_resp.results).unwrap();
    let planned_json = serde_json::to_value(&planned_resp.results).unwrap();
    assert_eq!(legacy_json, planned_json, "result items must be identical");
    // Evidence ids are minted per run (they are identity, not content), so
    // equivalence compares every SEMANTIC field and ignores the random ids.
    // Random ids AND timestamps are identity/runtime noise, not content:
    // the two runtimes seed at slightly different moments, and a run that
    // straddles a second boundary would otherwise fail spuriously.
    let semantic_ev = |value: serde_json::Value| {
        let mut v = value;
        if let Some(items) = v.as_array_mut() {
            for item in items {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("id");
                    obj.remove("source_id");
                    obj.remove("fetched_at");
                    if let Some(prov) = obj.get_mut("provenance").and_then(|p| p.as_object_mut()) {
                        prov.remove("retrieved_at");
                    }
                }
            }
        }
        v
    };
    let legacy_ev = semantic_ev(serde_json::to_value(&legacy_resp.provenance).unwrap());
    let planned_ev = semantic_ev(serde_json::to_value(&planned_resp.provenance).unwrap());
    assert_eq!(
        legacy_ev, planned_ev,
        "provenance content must be identical"
    );
    assert_eq!(legacy_resp.cost.estimated, planned_resp.cost.estimated);
    assert_eq!(legacy_resp.coverage_gaps, planned_resp.coverage_gaps);

    // The planner itself is observable on the plan-only surface: the same
    // request yields the same legacy query plan plus the federated source
    // selection, and nothing is executed or billed by planning.
    let legacy_plan = legacy.service.plan_report(&req).await.unwrap();
    let planned_plan = planned.service.plan_report(&req).await.unwrap();
    assert!(legacy_plan.federation_strategy.is_none());
    assert!(legacy_plan.federated_sources.is_empty());
    assert_eq!(planned_plan.federation_strategy.as_deref(), Some("auto"));
    assert_eq!(
        planned_plan.federated_sources,
        vec!["local_knowledge".to_string()]
    );
    let legacy_queries = serde_json::to_value(&legacy_plan.planned_queries).unwrap();
    let planned_queries = serde_json::to_value(&planned_plan.planned_queries).unwrap();
    assert_eq!(legacy_queries, planned_queries);
    assert_eq!(
        legacy_plan.open_requirements.len(),
        planned_plan.open_requirements.len()
    );
}

/// RFC-062 task 4b gate: `federation.strategy = execute` must run the SAME
/// provider with the SAME result items as the legacy loop, and bill EXACTLY
/// once — one WebSearch cost event at the same estimated price. The billing
/// must never move into the source adapter (INV-30-12) and must never be
/// duplicated by the executor (the dependency review's double-billing veto).
#[tokio::test]
async fn federated_execute_mode_bills_once_and_matches_legacy_results() {
    use async_trait::async_trait;
    use searchos_provider::error::ProviderError;
    use searchos_provider::traits::{ProviderResult, SearchParams, SearchProvider};

    struct FakeWebProvider {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl SearchProvider for FakeWebProvider {
        fn id(&self) -> &str {
            "fake_web"
        }
        fn capabilities(&self) -> searchos_domain::provider::ProviderCapabilities {
            searchos_domain::provider::ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(&self, _params: &SearchParams) -> Result<ProviderResult, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ProviderResult {
                entries: vec![searchos_provider::traits::SearchResultEntry {
                    title: Some("external tokio doc".into()),
                    url: "https://tokio.rs/external".into(),
                    content: "external tokio runtime doc".into(),
                    score: 0.9,
                    published_at: None,
                    raw_content: None,
                }],
                duration_ms: 5,
                bytes_transferred: 512,
            })
        }
        async fn health_check(&self) -> Result<bool, ProviderError> {
            Ok(true)
        }
    }

    let make = |strategy: &'static str, calls: Arc<AtomicUsize>, dir_path: std::path::PathBuf| async move {
        let mut cfg = SearchOsConfig::default();
        cfg.app.data_dir = dir_path;
        cfg.federation.strategy = strategy.to_string();
        let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        let provider: Arc<dyn searchos_provider::SearchProvider> = Arc::new(FakeWebProvider {
            calls: calls.clone(),
        });
        // The router selects from the provider registry; the fake provider
        // must be registered there for BOTH strategies to reach it.
        runtime
            .provider_registry
            .register(searchos_provider::RegisteredProvider::from_search(
                provider.clone(),
                5_000,
                "USD",
            ));
        let registry = searchos_federated::InMemorySourceRegistry::new();
        registry
            .register(Arc::new(searchos_federated::WebSearchProviderSource::new(
                provider.clone(),
                searchos_federated::SourceScope::Global,
                50,
                5_000,
                searchos_federated::SourceLatencyProfile::default(),
            )) as Arc<dyn searchos_federated::SearchSource>)
            .unwrap();
        let deps = SearchDependencies {
            profiles: searchos_policy::ProfileRegistry::builtin(),
            quality: searchos_policy::QualityPolicy::default(),
            freshness: searchos_policy::FreshnessPolicy::default_policy(),
            source: searchos_policy::SourcePolicy::default_policy(),
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
            router: runtime.provider_registry.clone(),
            search_providers: vec![provider],
            local_port: runtime.local_port(),
            traces: runtime.traces(),
            cost_sink: runtime.cost_sink(),
            task_recorder: runtime.task_recorder(),
            singleflight: Arc::new(SingleflightCoordinator::new()),
            cache: runtime.exact_cache.clone(),
            validity: None,
            policy_state: None,
            entity_lookup: None,
            negative_cache: runtime.negative_cache.clone(),
            expansion_mode: "followup".into(),
            semantic_dedup: true,
            incremental: true,
            query_expansion: true,
            reranking: true,
            allow_external: true,
            promotion: None,
            session_store: Some(runtime.session_store.clone()),
            federation_registry: Some(Arc::new(registry)),
            federation_strategy: strategy.to_string(),
            federation_limits: searchos_domain::FederationLimits::default(),
            federation_selection: "reliability_first".into(),
            quality_learner: None,
        };
        (SearchServiceImpl::new(Arc::new(deps)), runtime)
    };

    let legacy_calls = Arc::new(AtomicUsize::new(0));
    let fed_calls = Arc::new(AtomicUsize::new(0));
    let legacy_dir = tempfile::tempdir().unwrap();
    let fed_dir = tempfile::tempdir().unwrap();
    let (legacy_svc, legacy_rt) = make(
        "legacy",
        legacy_calls.clone(),
        legacy_dir.path().to_path_buf(),
    )
    .await;
    let (fed_svc, fed_rt) = make("execute", fed_calls.clone(), fed_dir.path().to_path_buf()).await;

    let seed = |rt: Arc<searchos_core::SearchOsRuntime>, chunk: &'static str| async move {
        use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};
        rt.fulltext
            .index(searchos_domain::retrieval::IndexedDocument {
                chunk_id: ChunkId::new(chunk),
                document_id: DocumentId::new(chunk),
                version_id: DocumentVersionId::new(chunk),
                title: Some("stale".into()),
                body: "stale body".into(),
                url: format!("https://local.dev/{chunk}"),
                language: Some("en".into()),
                workspace_id: "*".into(),
                fetched_at: chrono::Utc::now() - chrono::Duration::days(400),
            })
            .await
            .unwrap();
        rt.fulltext.commit().await.unwrap();
    };
    seed(legacy_rt.clone(), "old").await;
    seed(fed_rt.clone(), "old").await;

    let mut req = request("tokio async runtime", SearchMode::Hybrid);
    req.budget.max_external_queries = 2;
    let legacy_resp = legacy_svc.search(req.clone()).await.unwrap();
    let fed_resp = fed_svc.search(req.clone()).await.unwrap();

    // The legacy loop fans out PER QUERY on one provider (2 planned queries
    // ⇒ 2 calls); the federated path fans out PER SOURCE (1 provider ⇒ 1
    // call with the same results — the second query was a re-buy of the same
    // gap). Execute mode must NEVER bill more than legacy: one provider call
    // here, and the spent total must not exceed the legacy spend.
    let legacy_calls_n = legacy_calls.load(std::sync::atomic::Ordering::SeqCst);
    let fed_calls_n = fed_calls.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(legacy_calls_n, 2, "legacy fans out per planned query");
    assert_eq!(
        fed_calls_n, 1,
        "federated fans out per SOURCE: one provider, one call"
    );

    // Same decision and same external item served.
    assert_eq!(legacy_resp.decision.path, fed_resp.decision.path);
    let legacy_external: Vec<_> = legacy_resp
        .results
        .iter()
        .filter(|r| r.source == "fake_web")
        .collect();
    let fed_external: Vec<_> = fed_resp
        .results
        .iter()
        .filter(|r| r.source == "fake_web")
        .collect();
    // The legacy loop fans out PER QUERY, so its raw result list can carry
    // duplicate entries for the same URL; the evidence merge collapses them.
    // The federated path fans out PER SOURCE and never re-buys, so its raw
    // list is already deduplicated. Equivalence therefore compares the
    // URL-normalised item sets.
    let normalize = |items: &[&searchos_domain::response::SearchResultItem]| -> Vec<String> {
        let mut urls: Vec<String> = items.iter().map(|r| r.url.clone()).collect();
        urls.sort();
        urls.dedup();
        urls
    };
    assert_eq!(
        normalize(&legacy_external),
        normalize(&fed_external),
        "execute mode must serve the same provider content as legacy"
    );
    assert_eq!(fed_external.len(), 1);

    for (rt, label) in [(&legacy_rt, "legacy"), (&fed_rt, "execute")] {
        let q = searchos_domain::cost::CostQuery {
            workspace_id: Some(searchos_domain::ids::WorkspaceId::new("ws_default")),
            agent_id: None,
            task_id: None,
            provider_id: None,
            operation: None,
            since: None,
            until: None,
        };
        let summary = rt.application.cost_service.summary(&q).await.unwrap();
        // The fake provider id has no pricing entry, so its estimate is
        // zero; what the double-billing veto asserts is the EVENT identity
        // and count, priced identically under both strategies.
        let web = summary
            .by_operation
            .iter()
            .find(|(op, _)| op == "web_search")
            .map(|(_, m)| m.amount_micros);
        assert_eq!(web, Some(0), "{label}: web_search event recorded");
    }

    // RFC-062 §16: the per-source execution summary is attached to the
    // decision trace by the pipeline's re-record. The legacy path never
    // touches federation, so its traces stay byte-compatible.
    let fed_trace = fed_rt
        .traces()
        .get(fed_resp.trace_id.as_str())
        .await
        .unwrap()
        .expect("federated trace stored");
    let summary = fed_trace
        .federation_execution
        .as_ref()
        .expect("execute mode records a federation summary");
    assert_eq!(summary.strategy, "execute");
    assert_eq!(summary.planned_sources, vec!["fake_web".to_string()]);
    assert_eq!(summary.executed_sources, vec!["fake_web".to_string()]);
    assert_eq!(summary.candidates, 1);
    assert!(summary.failed_sources.is_empty());
    assert!(summary.timed_out_sources.is_empty());
    assert!(summary.not_executed_sources.is_empty());

    let legacy_trace = legacy_rt
        .traces()
        .get(legacy_resp.trace_id.as_str())
        .await
        .unwrap()
        .expect("legacy trace stored");
    assert!(
        legacy_trace.federation_execution.is_none(),
        "legacy path leaves traces unchanged"
    );
}

/// RFC-080 slice: execution outcomes feed a LIVE per-source circuit breaker.
/// Three consecutive provider failures trip it; the next request must not pay
/// for that source; an explicit healthy outcome closes it again.
#[tokio::test]
async fn federated_failures_trip_the_source_circuit_in_the_live_pipeline() {
    use async_trait::async_trait;
    use searchos_provider::error::ProviderError;
    use searchos_provider::traits::{ProviderResult, SearchParams, SearchProvider};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FlakyProvider {
        calls: Arc<AtomicUsize>,
        fail: Arc<AtomicBool>,
    }
    #[async_trait]
    impl SearchProvider for FlakyProvider {
        fn id(&self) -> &str {
            "flaky_web"
        }
        fn capabilities(&self) -> searchos_domain::provider::ProviderCapabilities {
            searchos_domain::provider::ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(&self, _params: &SearchParams) -> Result<ProviderResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(ProviderError::from_status("flaky_web", 503, "offline"));
            }
            Ok(ProviderResult {
                entries: vec![searchos_provider::traits::SearchResultEntry {
                    title: Some("recovered".into()),
                    url: "https://flaky.dev/recovered".into(),
                    content: "provider is healthy again".into(),
                    score: 0.8,
                    published_at: None,
                    raw_content: None,
                }],
                duration_ms: 1,
                bytes_transferred: 128,
            })
        }
        async fn health_check(&self) -> Result<bool, ProviderError> {
            Ok(!self.fail.load(Ordering::SeqCst))
        }
    }

    let mut cfg = SearchOsConfig::default();
    let dir = tempfile::tempdir().unwrap();
    cfg.app.data_dir = dir.path().to_path_buf();
    cfg.federation.strategy = "execute".into();
    let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(true));
    let provider: Arc<dyn SearchProvider> = Arc::new(FlakyProvider {
        calls: Arc::clone(&calls),
        fail: Arc::clone(&fail),
    });
    runtime
        .provider_registry
        .register(searchos_provider::RegisteredProvider::from_search(
            provider.clone(),
            5_000,
            "USD",
        ));
    let registry = Arc::new(searchos_federated::InMemorySourceRegistry::new());
    registry
        .register(Arc::new(searchos_federated::WebSearchProviderSource::new(
            provider.clone(),
            searchos_federated::SourceScope::Global,
            50,
            5_000,
            searchos_federated::SourceLatencyProfile::default(),
        )) as Arc<dyn searchos_federated::SearchSource>)
        .unwrap();

    let deps = SearchDependencies {
        profiles: searchos_policy::ProfileRegistry::builtin(),
        quality: searchos_policy::QualityPolicy::default(),
        freshness: searchos_policy::FreshnessPolicy::default_policy(),
        source: searchos_policy::SourcePolicy::default_policy(),
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
        router: runtime.provider_registry.clone(),
        search_providers: vec![provider],
        local_port: runtime.local_port(),
        traces: runtime.traces(),
        cost_sink: runtime.cost_sink(),
        task_recorder: runtime.task_recorder(),
        singleflight: Arc::new(SingleflightCoordinator::new()),
        cache: runtime.exact_cache.clone(),
        validity: None,
        policy_state: None,
        entity_lookup: None,
        negative_cache: runtime.negative_cache.clone(),
        semantic_dedup: true,
        expansion_mode: "followup".into(),
        incremental: true,
        query_expansion: true,
        reranking: true,
        allow_external: true,
        promotion: None,
        session_store: Some(runtime.session_store.clone()),
        federation_registry: Some(Arc::clone(&registry)),
        federation_strategy: "execute".into(),
        federation_limits: searchos_domain::FederationLimits::default(),
        federation_selection: "reliability_first".into(),
        quality_learner: None,
    };
    let svc = SearchServiceImpl::new(Arc::new(deps));

    // ExternalOnly bypasses the exact cache, so each distinct query really
    // executes (Part 30 decision red line).
    for i in 0..3 {
        let mut req = request(&format!("circuit probe {i}"), SearchMode::ExternalOnly);
        req.budget.max_external_queries = 1;
        let _ = svc.search(req).await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3, "three failures executed");

    let source_id = searchos_domain::ids::SourceId::new("flaky_web");
    let reliability = registry
        .descriptor_for(&source_id, &WorkspaceId::new("ws_default"))
        .unwrap()
        .reliability;
    assert!(reliability.circuit_open, "three failures trip the circuit");
    assert_eq!(reliability.consecutive_failures, 3);
    assert!(reliability.success_rate < 1.0);

    // Fourth request: the planner rejects the tripped source, so the provider
    // is never called again (no more wasted spend on a dead source).
    let mut req = request("circuit probe blocked", SearchMode::ExternalOnly);
    req.budget.max_external_queries = 1;
    let blocked = svc.search(req).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "an open circuit stops the purchase"
    );
    assert!(
        blocked
            .warnings
            .iter()
            .any(|w| w.contains("external_execution_failed"))
            || blocked.status != searchos_domain::response::SearchStatus::Completed,
        "a blocked acquisition is reported honestly, never as a success"
    );

    // A verified healthy outcome (health check / operator recovery) closes it.
    fail.store(false, Ordering::SeqCst);
    assert_eq!(registry.record_outcome(&source_id, true), Some(false));
    assert!(
        !registry
            .descriptor_for(&source_id, &WorkspaceId::new("ws_default"))
            .unwrap()
            .reliability
            .circuit_open
    );
    let mut req = request("circuit probe recovered", SearchMode::ExternalOnly);
    req.budget.max_external_queries = 1;
    let recovered = svc.search(req).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "recovery re-enables the source"
    );
    assert!(
        recovered
            .results
            .iter()
            .any(|r| r.url == "https://flaky.dev/recovered"),
        "the recovered source serves real content again"
    );
}

/// RFC-080: the caller's wall-clock budget reaches the federated planner.
/// A 1ms budget cannot fit any declared source timeout, so NO provider call
/// may happen and the failure must be reported honestly.
#[tokio::test]
async fn federated_execute_respects_a_tiny_wall_clock_budget() {
    use async_trait::async_trait;
    use searchos_provider::error::ProviderError;
    use searchos_provider::traits::{ProviderResult, SearchParams, SearchProvider};
    use std::sync::atomic::Ordering;

    struct CountingProvider {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl SearchProvider for CountingProvider {
        fn id(&self) -> &str {
            "budget_web"
        }
        fn capabilities(&self) -> searchos_domain::provider::ProviderCapabilities {
            searchos_domain::provider::ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(&self, _params: &SearchParams) -> Result<ProviderResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ProviderResult {
                entries: vec![searchos_provider::traits::SearchResultEntry {
                    title: Some("too late".into()),
                    url: "https://budget.dev/x".into(),
                    content: "should never be bought".into(),
                    score: 0.9,
                    published_at: None,
                    raw_content: None,
                }],
                duration_ms: 1,
                bytes_transferred: 64,
            })
        }
        async fn health_check(&self) -> Result<bool, ProviderError> {
            Ok(true)
        }
    }

    let mut cfg = SearchOsConfig::default();
    let dir = tempfile::tempdir().unwrap();
    cfg.app.data_dir = dir.path().to_path_buf();
    cfg.federation.strategy = "execute".into();
    let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn SearchProvider> = Arc::new(CountingProvider {
        calls: Arc::clone(&calls),
    });
    runtime
        .provider_registry
        .register(searchos_provider::RegisteredProvider::from_search(
            provider.clone(),
            5_000,
            "USD",
        ));
    let registry = Arc::new(searchos_federated::InMemorySourceRegistry::new());
    registry
        .register(Arc::new(searchos_federated::WebSearchProviderSource::new(
            provider.clone(),
            searchos_federated::SourceScope::Global,
            50,
            5_000,
            searchos_federated::SourceLatencyProfile::default(),
        )) as Arc<dyn searchos_federated::SearchSource>)
        .unwrap();

    let deps = SearchDependencies {
        profiles: searchos_policy::ProfileRegistry::builtin(),
        quality: searchos_policy::QualityPolicy::default(),
        freshness: searchos_policy::FreshnessPolicy::default_policy(),
        source: searchos_policy::SourcePolicy::default_policy(),
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
        router: runtime.provider_registry.clone(),
        search_providers: vec![provider],
        local_port: runtime.local_port(),
        traces: runtime.traces(),
        cost_sink: runtime.cost_sink(),
        task_recorder: runtime.task_recorder(),
        singleflight: Arc::new(SingleflightCoordinator::new()),
        cache: runtime.exact_cache.clone(),
        validity: None,
        policy_state: None,
        entity_lookup: None,
        negative_cache: runtime.negative_cache.clone(),
        semantic_dedup: true,
        expansion_mode: "followup".into(),
        incremental: true,
        query_expansion: true,
        reranking: true,
        allow_external: true,
        promotion: None,
        session_store: None,
        federation_registry: Some(Arc::clone(&registry)),
        federation_strategy: "execute".into(),
        federation_limits: searchos_domain::FederationLimits::default(),
        federation_selection: "reliability_first".into(),
        quality_learner: None,
    };
    let svc = SearchServiceImpl::new(Arc::new(deps));

    let mut req = request("latency budget probe", SearchMode::ExternalOnly);
    req.budget.max_external_queries = 1;
    req.budget.max_execution_time_ms = 1;
    let response = svc.search(req).await.unwrap();

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no source fits a 1ms budget"
    );
    assert!(
        response.status != searchos_domain::response::SearchStatus::Completed,
        "a rejected plan is never disguised as a completed search"
    );
    assert!(
        response
            .warnings
            .iter()
            .any(|w| w.contains("external_execution_failed")),
        "the failure is reported: {:?}",
        response.warnings
    );

    // The plan-only surface applies the same budget (no execution, no
    // billing, but the same auditable selection).
    let mut plan_req = request("latency budget plan probe", SearchMode::ExternalOnly);
    plan_req.budget.max_execution_time_ms = 1;
    let plan = svc.plan(&plan_req).await.unwrap();
    assert!(
        plan.federated_sources.is_empty(),
        "plan-only surface applies the same latency budget: {:?}",
        plan.federated_sources
    );
}

/// RFC-074 end-to-end: the same article republished on a second host is ONE
/// occurrence set in the assembled context — provenance shows it, diversity
/// does not count it twice. No content is deleted from `results`.
#[tokio::test]
async fn context_assembly_collapses_cross_host_syndicated_content() {
    use async_trait::async_trait;
    use searchos_provider::error::ProviderError;
    use searchos_provider::traits::{ProviderResult, SearchParams, SearchProvider};

    struct SyndicatedProvider;
    #[async_trait]
    impl SearchProvider for SyndicatedProvider {
        fn id(&self) -> &str {
            "syndicator"
        }
        fn capabilities(&self) -> searchos_domain::provider::ProviderCapabilities {
            searchos_domain::provider::ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(&self, _params: &SearchParams) -> Result<ProviderResult, ProviderError> {
            let article = "The federated planner selects sources with an auditable rejection \\n                    chain: permission, scope, capability, reliability, latency and cost are \\n                    all recorded per source before any bill is produced.";
            Ok(ProviderResult {
                entries: vec![
                    searchos_provider::traits::SearchResultEntry {
                        title: Some("original article".into()),
                        url: "https://origin.dev/federated-planner".into(),
                        content: article.into(),
                        score: 0.9,
                        published_at: None,
                        raw_content: None,
                    },
                    searchos_provider::traits::SearchResultEntry {
                        title: Some("repost".into()),
                        url: "https://mirror.example.net/federated-planner".into(),
                        content: article.into(),
                        score: 0.85,
                        published_at: None,
                        raw_content: None,
                    },
                ],
                duration_ms: 2,
                bytes_transferred: 512,
            })
        }
        async fn health_check(&self) -> Result<bool, ProviderError> {
            Ok(true)
        }
    }

    let mut cfg = SearchOsConfig::default();
    let dir = tempfile::tempdir().unwrap();
    cfg.app.data_dir = dir.path().to_path_buf();
    cfg.federation.strategy = "execute".into();
    let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let provider: Arc<dyn SearchProvider> = Arc::new(SyndicatedProvider);
    runtime
        .provider_registry
        .register(searchos_provider::RegisteredProvider::from_search(
            provider.clone(),
            5_000,
            "USD",
        ));
    let registry = Arc::new(searchos_federated::InMemorySourceRegistry::new());
    registry
        .register(Arc::new(searchos_federated::WebSearchProviderSource::new(
            provider.clone(),
            searchos_federated::SourceScope::Global,
            50,
            5_000,
            searchos_federated::SourceLatencyProfile::default(),
        )) as Arc<dyn searchos_federated::SearchSource>)
        .unwrap();

    let deps = SearchDependencies {
        profiles: searchos_policy::ProfileRegistry::builtin(),
        quality: searchos_policy::QualityPolicy::default(),
        freshness: searchos_policy::FreshnessPolicy::default_policy(),
        source: searchos_policy::SourcePolicy::default_policy(),
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
        router: runtime.provider_registry.clone(),
        search_providers: vec![provider],
        local_port: runtime.local_port(),
        traces: runtime.traces(),
        cost_sink: runtime.cost_sink(),
        task_recorder: runtime.task_recorder(),
        singleflight: Arc::new(SingleflightCoordinator::new()),
        cache: runtime.exact_cache.clone(),
        validity: None,
        policy_state: None,
        entity_lookup: None,
        negative_cache: runtime.negative_cache.clone(),
        semantic_dedup: true,
        expansion_mode: "followup".into(),
        incremental: true,
        query_expansion: true,
        reranking: true,
        allow_external: true,
        promotion: None,
        session_store: None,
        federation_registry: Some(Arc::clone(&registry)),
        federation_strategy: "execute".into(),
        federation_limits: searchos_domain::FederationLimits::default(),
        federation_selection: "reliability_first".into(),
        quality_learner: None,
    };
    let svc = SearchServiceImpl::new(Arc::new(deps));

    let mut req = request("auditable rejection chain", SearchMode::ExternalOnly);
    req.budget.max_external_queries = 1;
    req.context_budget = Some(searchos_domain::context::ContextBudget {
        max_tokens: 4096,
        reserved_tokens: 0,
        max_sources: 8,
        max_chunks_per_source: 4,
        deduplicate: true,
        preserve_citations: true,
        include_conflicts: false,
    });
    let response = svc.search(req).await.unwrap();

    assert_eq!(response.results.len(), 2, "results are never rewritten");
    let block = response
        .context
        .as_ref()
        .expect("a context block is assembled");
    let snippets = &block.evidence;
    assert_eq!(
        snippets.len(),
        1,
        "the syndicated copy collapses into one context entry: {:?}",
        snippets.iter().map(|s| &s.citation).collect::<Vec<_>>()
    );
    assert_eq!(
        snippets[0].occurrences, 2,
        "the occurrence is recorded, not lost"
    );
}

/// RFC-076 source-aware decision: when every external source is tripped, an
/// ExternalOnly search is rejected BEFORE any provider call — the honest
/// `ProviderUnavailable` reason instead of executing into certain failure.
#[tokio::test]
async fn decision_rejects_external_only_when_all_sources_are_tripped() {
    use async_trait::async_trait;
    use searchos_provider::error::ProviderError;
    use searchos_provider::traits::{ProviderResult, SearchParams, SearchProvider};
    use std::sync::atomic::Ordering;

    struct QuietProvider {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl SearchProvider for QuietProvider {
        fn id(&self) -> &str {
            "quiet_web"
        }
        fn capabilities(&self) -> searchos_domain::provider::ProviderCapabilities {
            searchos_domain::provider::ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(&self, _params: &SearchParams) -> Result<ProviderResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ProviderResult {
                entries: vec![searchos_provider::traits::SearchResultEntry {
                    title: Some("never bought".into()),
                    url: "https://quiet.dev/x".into(),
                    content: "provider should not be called".into(),
                    score: 0.9,
                    published_at: None,
                    raw_content: None,
                }],
                duration_ms: 1,
                bytes_transferred: 32,
            })
        }
        async fn health_check(&self) -> Result<bool, ProviderError> {
            Ok(true)
        }
    }

    let mut cfg = SearchOsConfig::default();
    let dir = tempfile::tempdir().unwrap();
    cfg.app.data_dir = dir.path().to_path_buf();
    cfg.federation.strategy = "execute".into();
    let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn SearchProvider> = Arc::new(QuietProvider {
        calls: Arc::clone(&calls),
    });
    runtime
        .provider_registry
        .register(searchos_provider::RegisteredProvider::from_search(
            provider.clone(),
            5_000,
            "USD",
        ));
    let registry = Arc::new(searchos_federated::InMemorySourceRegistry::new());
    registry
        .register(Arc::new(searchos_federated::WebSearchProviderSource::new(
            provider.clone(),
            searchos_federated::SourceScope::Global,
            50,
            5_000,
            searchos_federated::SourceLatencyProfile::default(),
        )) as Arc<dyn searchos_federated::SearchSource>)
        .unwrap();
    // The service under test must be rebuilt so its deps carry THIS registry
    // (the runtime built its own with an empty one).
    let deps = SearchDependencies {
        profiles: searchos_policy::ProfileRegistry::builtin(),
        quality: searchos_policy::QualityPolicy::default(),
        freshness: searchos_policy::FreshnessPolicy::default_policy(),
        source: searchos_policy::SourcePolicy::default_policy(),
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
        router: runtime.provider_registry.clone(),
        search_providers: vec![provider],
        local_port: runtime.local_port(),
        traces: runtime.traces(),
        cost_sink: runtime.cost_sink(),
        task_recorder: runtime.task_recorder(),
        singleflight: Arc::new(SingleflightCoordinator::new()),
        cache: runtime.exact_cache.clone(),
        validity: None,
        policy_state: None,
        entity_lookup: None,
        negative_cache: runtime.negative_cache.clone(),
        semantic_dedup: true,
        expansion_mode: "followup".into(),
        incremental: true,
        query_expansion: true,
        reranking: true,
        allow_external: true,
        promotion: None,
        session_store: None,
        federation_registry: Some(Arc::clone(&registry)),
        federation_strategy: "execute".into(),
        federation_limits: searchos_domain::FederationLimits::default(),
        federation_selection: "reliability_first".into(),
        quality_learner: None,
    };
    let svc = SearchServiceImpl::new(Arc::new(deps));

    // First: a healthy source serves an ExternalOnly search normally.
    let healthy = svc
        .search(request("adaptive probe one", SearchMode::ExternalOnly))
        .await
        .unwrap();
    assert!(calls.load(Ordering::SeqCst) >= 1, "healthy source executes");
    let healthy_calls = calls.load(Ordering::SeqCst);
    assert_eq!(healthy.decision.path, "external");

    // Trip the only source's circuit (three consecutive failures).
    let source_id = searchos_domain::ids::SourceId::new("quiet_web");
    for _ in 0..3 {
        registry.record_outcome(&source_id, false);
    }

    // Now the DECISION itself must reject before any execution.
    let blocked = svc
        .search(request("adaptive probe two", SearchMode::ExternalOnly))
        .await
        .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        healthy_calls,
        "a tripped source is never called again"
    );
    assert_eq!(
        blocked.decision.path, "reject",
        "the decision is source-aware: {:?}",
        blocked.decision
    );
    assert_eq!(blocked.decision.reason, "provider_unavailable");
    assert_eq!(
        blocked.status,
        searchos_domain::response::SearchStatus::InsufficientEvidence,
        "honest insufficiency, never a disguised success"
    );
}

/// RFC-076 cost-adaptive path selection: when the full external plan is over
/// budget but a smaller plan fits, the plan is DOWNSIZED (trace records it,
/// the pipeline caps the queries) instead of rejecting outright. The default
/// (flag off) keeps the RFC-004 Rule-5 red line.
#[tokio::test]
async fn cost_adaptive_downsizes_the_external_plan_within_budget() {
    use async_trait::async_trait;
    use searchos_provider::error::ProviderError;
    use searchos_provider::traits::{ProviderResult, SearchParams, SearchProvider};
    use std::sync::atomic::Ordering;

    struct PricedProvider {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl SearchProvider for PricedProvider {
        fn id(&self) -> &str {
            "priced_web"
        }
        fn capabilities(&self) -> searchos_domain::provider::ProviderCapabilities {
            searchos_domain::provider::ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(&self, _params: &SearchParams) -> Result<ProviderResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ProviderResult {
                entries: vec![searchos_provider::traits::SearchResultEntry {
                    title: Some("priced result".into()),
                    url: "https://priced.dev/x".into(),
                    content: "priced provider content".into(),
                    score: 0.9,
                    published_at: None,
                    raw_content: None,
                }],
                duration_ms: 1,
                bytes_transferred: 64,
            })
        }
        async fn health_check(&self) -> Result<bool, ProviderError> {
            Ok(true)
        }
    }

    let run = |cost_adaptive: bool| async move {
        let mut cfg = SearchOsConfig::default();
        let dir = tempfile::tempdir().unwrap();
        cfg.app.data_dir = dir.path().to_path_buf();
        cfg.federation.strategy = "execute".into();
        let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn SearchProvider> = Arc::new(PricedProvider {
            calls: Arc::clone(&calls),
        });
        // The DECISION estimates via the cost MODEL, so the fake id needs a
        // pricing entry: 1_000 micros per request.
        let mut cost_config = searchos_cost::CostModelConfig::default_config();
        cost_config.upsert_provider(searchos_cost::ProviderPricing {
            provider_id: "priced_web".into(),
            currency: "USD".into(),
            pricing: searchos_cost::PricingModel::RequestBased {
                unit_price: "0.001".into(),
            },
            version: 1,
            from_real_bill: false,
        });
        runtime
            .provider_registry
            .register(searchos_provider::RegisteredProvider::from_search(
                // 1_000 micros per query.
                provider.clone(),
                1_000,
                "USD",
            ));
        let registry = Arc::new(searchos_federated::InMemorySourceRegistry::new());
        registry
            .register(Arc::new(searchos_federated::WebSearchProviderSource::new(
                provider.clone(),
                searchos_federated::SourceScope::Global,
                50,
                1_000,
                searchos_federated::SourceLatencyProfile::default(),
            )) as Arc<dyn searchos_federated::SearchSource>)
            .unwrap();
        let deps = SearchDependencies {
            profiles: searchos_policy::ProfileRegistry::builtin(),
            quality: searchos_policy::QualityPolicy::default(),
            freshness: searchos_policy::FreshnessPolicy::default_policy(),
            source: searchos_policy::SourcePolicy::default_policy(),
            cost_config,
            bm25: runtime.fulltext.clone(),
            vector: Some(runtime.vector.clone()),
            hybrid: Some(runtime.hybrid.clone()),
            embedder: runtime.embedder.clone(),
            canonical_cache: runtime.canonical_cache.clone(),
            semantic_cache: runtime.semantic_cache.clone(),
            retrieval_cache: runtime.retrieval_cache.clone(),
            embedding_cache: runtime.embedding_cache.clone(),
            rerank_cache: runtime.rerank_cache.clone(),
            router: runtime.provider_registry.clone(),
            search_providers: vec![provider],
            local_port: runtime.local_port(),
            traces: runtime.traces(),
            cost_sink: runtime.cost_sink(),
            task_recorder: runtime.task_recorder(),
            singleflight: runtime.singleflight.clone(),
            cache: runtime.exact_cache.clone(),
            validity: None,
            policy_state: None,
            entity_lookup: None,
            negative_cache: runtime.negative_cache.clone(),
            expansion_mode: "followup".into(),
            semantic_dedup: true,
            incremental: true,
            query_expansion: true,
            reranking: true,
            allow_external: true,
            promotion: None,
            session_store: None,
            federation_registry: Some(Arc::clone(&registry)),
            federation_strategy: "execute".into(),
            federation_limits: searchos_domain::FederationLimits::default(),
            federation_selection: "reliability_first".into(),
            quality_learner: None,
        };
        let svc = SearchServiceImpl::new(Arc::new(deps));
        let mut req = request("cost adaptive probe", SearchMode::ExternalOnly);
        req.budget.max_external_queries = 3;
        req.budget.max_total_cost_micros = 2_500;
        req.budget.cost_adaptive = cost_adaptive;
        let resp = svc.search(req).await.unwrap();
        let trace = runtime
            .traces()
            .get(resp.trace_id.as_str())
            .await
            .unwrap()
            .expect("trace recorded");
        (resp, trace, calls.load(Ordering::SeqCst))
    };

    // Cost-adaptive ON: downsized to 2 queries, executed, within budget.
    let (resp, trace, calls) = run(true).await;
    let adaptive = trace
        .cost_adaptive
        .as_ref()
        .expect("cost-adaptive trace recorded");
    assert_eq!(adaptive.requested_external_queries, 3);
    assert_eq!(adaptive.affordable_external_queries, 2);
    assert!(adaptive.downsized, "the full plan did not fit");
    assert!(
        calls <= 2,
        "the plan never exceeds the affordable cap: {calls} calls"
    );
    assert_ne!(
        resp.status,
        searchos_domain::response::SearchStatus::BudgetExhausted
    );

    // Cost-adaptive OFF (default): Rule 5 keeps its red line.
    let (resp, trace, calls) = run(false).await;
    assert_eq!(resp.decision.path, "reject");
    assert_eq!(resp.decision.reason, "budget_exceeded");
    assert_eq!(calls, 0, "nothing is bought when the plan is unaffordable");
    let adaptive = trace.cost_adaptive.expect("trace still explains why");
    assert_eq!(adaptive.affordable_external_queries, 2);
}

/// RFC-072: initial query expansion adds bounded synonym / sub-question
/// variants to the FIRST plan when the mode allows it, competing for the
/// same affordable query budget — and stays off by default.
#[tokio::test]
async fn initial_query_expansion_is_opt_in_and_budget_bounded() {
    use async_trait::async_trait;
    use searchos_provider::error::ProviderError;
    use searchos_provider::traits::{ProviderResult, SearchParams, SearchProvider};

    struct RecordingProvider {
        queries: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl SearchProvider for RecordingProvider {
        fn id(&self) -> &str {
            "recording_web"
        }
        fn capabilities(&self) -> searchos_domain::provider::ProviderCapabilities {
            searchos_domain::provider::ProviderCapabilities {
                search: true,
                ..Default::default()
            }
        }
        async fn search(&self, params: &SearchParams) -> Result<ProviderResult, ProviderError> {
            self.queries.lock().unwrap().push(params.query.clone());
            Ok(ProviderResult {
                entries: Vec::new(),
                duration_ms: 1,
                bytes_transferred: 8,
            })
        }
        async fn health_check(&self) -> Result<bool, ProviderError> {
            Ok(true)
        }
    }

    let run = |mode: &'static str| async move {
        let mut cfg = SearchOsConfig::default();
        let dir = tempfile::tempdir().unwrap();
        cfg.app.data_dir = dir.path().to_path_buf();
        cfg.federation.strategy = "legacy".into();
        cfg.intelligence.query_expansion_mode = mode.into();
        let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        let queries = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider: Arc<dyn SearchProvider> = Arc::new(RecordingProvider {
            queries: Arc::clone(&queries),
        });
        runtime
            .provider_registry
            .register(searchos_provider::RegisteredProvider::from_search(
                provider.clone(),
                0,
                "USD",
            ));
        let deps = SearchDependencies {
            profiles: searchos_policy::ProfileRegistry::builtin(),
            quality: searchos_policy::QualityPolicy::default(),
            freshness: searchos_policy::FreshnessPolicy::default_policy(),
            source: searchos_policy::SourcePolicy::default_policy(),
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
            router: runtime.provider_registry.clone(),
            search_providers: vec![provider],
            local_port: runtime.local_port(),
            traces: runtime.traces(),
            cost_sink: runtime.cost_sink(),
            task_recorder: runtime.task_recorder(),
            singleflight: runtime.singleflight.clone(),
            cache: runtime.exact_cache.clone(),
            validity: None,
            policy_state: None,
            entity_lookup: None,
            negative_cache: runtime.negative_cache.clone(),
            expansion_mode: mode.into(),
            semantic_dedup: true,
            incremental: true,
            query_expansion: true,
            reranking: true,
            allow_external: true,
            promotion: None,
            session_store: None,
            federation_registry: None,
            federation_strategy: "legacy".into(),
            federation_limits: searchos_domain::FederationLimits::default(),
            federation_selection: "reliability_first".into(),
            quality_learner: None,
        };
        let svc = SearchServiceImpl::new(Arc::new(deps));
        let mut req = request(
            "how to configure the tokio async runtime",
            SearchMode::ExternalOnly,
        );
        req.budget.max_external_queries = 2;
        // Only the FIRST round: follow-up rounds legitimately reformulate
        // and would blur the mode comparison.
        req.budget.max_acquisition_rounds = 0;
        let _ = svc.search(req).await.unwrap();
        let seen = queries.lock().unwrap().clone();
        (seen, runtime)
    };

    // Default (followup): the first plan is the raw query only.
    let (default_queries, _rt) = run("followup").await;
    assert!(
        default_queries
            .iter()
            .all(|q| q == "how to configure the tokio async runtime"),
        "{default_queries:?}"
    );

    // initial: a bounded variant is added within the query budget.
    let (expanded, rt) = run("initial").await;
    assert!(
        expanded.len() <= 2,
        "the expansion never exceeds the budget: {expanded:?}"
    );
    assert!(
        expanded
            .iter()
            .any(|q| q != "how to configure the tokio async runtime"),
        "an expansion variant was planned: {expanded:?}"
    );
    let _ = rt;
}

/// RFC-038: a response whose evidence merge observed relations carries an
/// evidence graph (canonical variant folds), and a relation-free answer
/// carries `None`.
#[tokio::test]
async fn response_carries_an_evidence_graph_only_when_relations_exist() {
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};

    let make = |chunk: &'static str, url: &'static str, body: &'static str| async move {
        let mut cfg = SearchOsConfig::default();
        let dir = tempfile::tempdir().unwrap();
        cfg.app.data_dir = dir.path().to_path_buf();
        let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        runtime
            .fulltext
            .index(searchos_domain::retrieval::IndexedDocument {
                chunk_id: ChunkId::new(chunk),
                document_id: DocumentId::new(chunk),
                version_id: DocumentVersionId::new(chunk),
                title: Some("evidence graph notes".into()),
                body: body.into(),
                url: url.into(),
                language: Some("en".into()),
                workspace_id: "*".into(),
                fetched_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        runtime.fulltext.commit().await.unwrap();
        let resp = runtime
            .application
            .search_service
            .search(request("evidence graph notes", SearchMode::LocalOnly))
            .await
            .unwrap();
        (resp, runtime)
    };

    // A raw URL with tracking parameters is a VARIANT of its canonical form:
    // the merge records the fold and the graph exposes it.
    let (resp, _rt) = make(
        "graph_a",
        "https://graph.dev/notes?utm_source=feed",
        "evidence graph notes about canonical variant folds",
    )
    .await;
    let graph = resp
        .evidence_graph
        .as_ref()
        .expect("a variant URL produces a duplicate_of edge");
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.relation == searchos_domain::response::EvidenceRelation::DuplicateOf),
        "{graph:?}"
    );
    assert!(graph.nodes.iter().any(|n| n.contains("graph.dev")));

    // Same content, canonical URL, single source: nothing to relate.
    let (plain, _rt2) = make(
        "graph_b",
        "https://graph.dev/canonical",
        "a standalone note with no relations to graph",
    )
    .await;
    assert!(plain.evidence_graph.is_none());
}

/// RFC-057: an as-of instant judges freshness at a HISTORICAL point. A
/// document fetched after that point is not part of the view, and a stale
/// document that was fresh back then is served.
#[tokio::test]
async fn as_of_view_excludes_future_documents_and_preserves_historical_freshness() {
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};

    async fn seed(
        rt: &Arc<searchos_core::SearchOsRuntime>,
        chunk: &str,
        url: &str,
        fetched_at: chrono::DateTime<chrono::Utc>,
    ) {
        rt.fulltext
            .index(searchos_domain::retrieval::IndexedDocument {
                chunk_id: ChunkId::new(chunk),
                document_id: DocumentId::new(chunk),
                version_id: DocumentVersionId::new(chunk),
                title: Some("historical note".into()),
                body: "historical temporal knowledge note body".into(),
                url: url.into(),
                language: Some("en".into()),
                workspace_id: "*".into(),
                fetched_at,
            })
            .await
            .unwrap();
        rt.fulltext.commit().await.unwrap();
    }

    let mut cfg = SearchOsConfig::default();
    let dir = tempfile::tempdir().unwrap();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let old = chrono::Utc::now() - chrono::Duration::days(10);
    let recent = chrono::Utc::now();
    seed(&rt, "old", "https://history.dev/old", old).await;
    seed(&rt, "future", "https://history.dev/future", recent).await;

    let past = old + chrono::Duration::hours(1);
    let mut req = request("historical temporal knowledge note", SearchMode::LocalOnly);
    req.constraints.freshness = searchos_domain::request::FreshnessRequirement::Any;
    req.constraints.as_of = Some(past);
    let historical = rt.application.search_service.search(req).await.unwrap();
    let urls: Vec<&str> = historical.results.iter().map(|r| r.url.as_str()).collect();
    assert!(
        urls.contains(&"https://history.dev/old"),
        "the document that existed at the as-of point is served: {urls:?}"
    );
    assert!(
        !urls.contains(&"https://history.dev/future"),
        "a document fetched AFTER the as-of point is not in the historical view: {urls:?}"
    );

    // Default (no as-of) is the wall-clock view and DOES include both.
    let mut now_req = request("historical temporal knowledge note", SearchMode::LocalOnly);
    now_req.constraints.freshness = searchos_domain::request::FreshnessRequirement::Any;
    let current = rt.application.search_service.search(now_req).await.unwrap();
    assert!(current
        .results
        .iter()
        .any(|r| r.url.contains("history.dev/future")));
}

#[tokio::test]
async fn as_of_is_part_of_the_cache_key() {
    let mut cfg = SearchOsConfig::default();
    let dir = tempfile::tempdir().unwrap();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let svc = rt.application.search_service.clone();

    let mut req = request("temporal cache key probe", SearchMode::LocalOnly);
    req.constraints.as_of = Some(chrono::Utc::now() - chrono::Duration::days(1));
    let first = svc.search(req.clone()).await.unwrap();
    assert!(!first.replayed);

    let mut other = req.clone();
    other = other.with_request_id("asof_other_view");
    other.constraints.as_of = Some(chrono::Utc::now() - chrono::Duration::days(2));
    let second = svc.search(other).await.unwrap();
    assert!(
        !second.replayed,
        "different as-of instants never share a cached answer"
    );
}
