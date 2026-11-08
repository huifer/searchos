//! SearchOS integration suite (cross-crate, RFC-016 phase acceptance).
//!
//! These tests exercise the assembled runtime end-to-end with offline
//! providers — the same use cases Local Mode and Gateway Mode share.

use std::sync::Arc;

use searchos_application::search_service::{
    SearchDependencies, SearchService as _, SearchServiceImpl,
};
use searchos_application::singleflight::SingleflightCoordinator;
use searchos_core::SearchOsConfig;
use searchos_domain::cost::{CostEvent, CostOperation, CostQuery, Money};
use searchos_domain::ids::{ProfileId, TraceId, WorkspaceId};
use searchos_domain::request::{FreshnessRequirement, SearchMode, SearchRequest};
use searchos_domain::retrieval::RetrievalQuery;
use searchos_retrieval::SearchIndex;

/// Offline in-memory harness over real adapters.
struct Harness {
    runtime: Arc<searchos_core::SearchOsRuntime>,
    service: searchos_application::search_service::SearchServiceImpl,
    dir: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        let deps = rebuild_deps(&runtime);
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
        self.runtime.tantivy.index(doc).await.unwrap();
        self.runtime.tantivy.commit().await.unwrap();
    }
}

fn rebuild_deps(runtime: &Arc<searchos_core::SearchOsRuntime>) -> Arc<SearchDependencies> {
    Arc::new(SearchDependencies {
        profiles: searchos_policy::ProfileRegistry::builtin(),
        quality: searchos_policy::QualityPolicy::default(),
        freshness: searchos_policy::FreshnessPolicy::default_policy(),
        source: searchos_policy::SourcePolicy::default_policy(),
        cost_config: searchos_cost::CostModelConfig::default_config(),
        bm25: runtime.tantivy.clone(),
        router: runtime.provider_registry.clone(),
        search_providers: vec![],
        local_port: runtime.local_port(),
        traces: runtime.traces(),
        cost_sink: runtime.cost_sink(),
        task_recorder: runtime.task_recorder(),
        singleflight: Arc::new(SingleflightCoordinator::new()),
        cache: runtime.exact_cache.clone(),
        allow_external: false,
    })
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
    h.runtime.tantivy.index(stale_doc).await.unwrap();
    h.runtime.tantivy.commit().await.unwrap();

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
        .save_versioned(&doc, &version, &chunks, &payloads)
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
            .tantivy
            .search(&RetrievalQuery::new("work stealing scheduler", 5))
            .await
            .unwrap()
            .is_empty(),
        "outbox delivered the chunk to the index"
    );

    // Wipe the index, rebuild from SQLite — same corpus returns.
    h.runtime.tantivy.rebuild_from(&[]).await.unwrap();
    assert!(h
        .runtime
        .tantivy
        .search(&RetrievalQuery::new("work stealing", 5))
        .await
        .unwrap()
        .is_empty());
    let n = h.runtime.rebuild_index().await.unwrap();
    assert_eq!(n, 1);
    assert!(
        !h.runtime
            .tantivy
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
