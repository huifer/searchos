//! RFC-029 local data ingestion acceptance tests.
//!
//! Exercises the assembled runtime: ingest text / file / batch, then prove the
//! imported knowledge is retrievable under `local_only`, deduplicates
//! correctly, survives rebuild, and stays workspace-isolated.

use std::sync::Arc;

use searchos_application::ingest_service::{IngestBatchItem, IngestPayload, IngestRequest};
use searchos_application::search_service::{
    SearchDependencies, SearchService as _, SearchServiceImpl,
};
use searchos_application::singleflight::SingleflightCoordinator;
use searchos_core::SearchOsConfig;
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};

struct Harness {
    runtime: Arc<searchos_core::SearchOsRuntime>,
    service: SearchServiceImpl,
    _dir: tempfile::TempDir,
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
            _dir: dir,
        }
    }

    async fn drain(&self) {
        let _ = self.runtime.process_outbox(200).await;
    }
}

fn rebuild_deps(runtime: &Arc<searchos_core::SearchOsRuntime>) -> Arc<SearchDependencies> {
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
        semantic_dedup: true,
        incremental: true,
        query_expansion: true,
        reranking: true,
        allow_external: false,
    })
}

fn request(query: &str, workspace: &str) -> SearchRequest {
    SearchRequest::new(
        query,
        ProfileId::general(),
        SearchMode::LocalOnly,
        WorkspaceId::new(workspace),
    )
    .unwrap()
}

fn text_request(workspace: &str) -> IngestRequest {
    IngestRequest::new(WorkspaceId::new(workspace))
}

/// AC1/AC2: ingested text is indexed and served by `local_only` search.
#[tokio::test]
async fn ingest_text_is_searchable_local_only() {
    let h = Harness::new().await;
    let mut req = text_request("default");
    req.source_key = Some("notes/tokio".into());
    req.title = Some("Tokio notes".into());
    let doc = h
        .runtime
        .application
        .ingest_service
        .ingest_text(
            req,
            "Tokio is an async runtime for Rust with a work-stealing scheduler.",
        )
        .await
        .unwrap();
    assert!(doc.changed);
    assert_eq!(doc.source_type, "local_text");
    assert_eq!(
        doc.canonical_url, "searchos-local://default/notes/tokio",
        "canonical local URI is stable"
    );
    h.drain().await;

    let resp = h
        .service
        .search(request("What is tokio async runtime?", "default"))
        .await
        .unwrap();
    assert_eq!(resp.decision.path, "local", "got {:?}", resp.decision);
    assert!(
        resp.results
            .iter()
            .any(|r| r.content.to_lowercase().contains("work-stealing")),
        "imported content served locally: {:?}",
        resp.results
    );
}

/// AC3: same source_key + identical content is a no-op.
#[tokio::test]
async fn same_key_same_content_is_unchanged() {
    let h = Harness::new().await;
    let mut req = text_request("default");
    req.source_key = Some("notes/dedup".into());
    let first = h
        .runtime
        .application
        .ingest_service
        .ingest_text(req.clone(), "identical body")
        .await
        .unwrap();
    let outbox = searchos_storage_sqlite::repos::OutboxRepo::new(&h.runtime.sqlite);
    let (pending_before, _, _) = outbox.counts().unwrap();

    let second = h
        .runtime
        .application
        .ingest_service
        .ingest_text(req, "identical body")
        .await
        .unwrap();
    assert!(
        !second.changed,
        "identical content must not create a version"
    );
    assert_eq!(first.document_id, second.document_id);
    assert_eq!(first.version_id, second.version_id);

    let (pending_after, _, _) = outbox.counts().unwrap();
    assert_eq!(
        pending_before, pending_after,
        "unchanged content must not enqueue new outbox rows"
    );
}

/// AC4: same source_key + changed content creates a new version.
#[tokio::test]
async fn same_key_new_content_creates_new_version() {
    let h = Harness::new().await;
    let mut req = text_request("default");
    req.source_key = Some("notes/versioned".into());
    let first = h
        .runtime
        .application
        .ingest_service
        .ingest_text(req.clone(), "version one body")
        .await
        .unwrap();
    let second = h
        .runtime
        .application
        .ingest_service
        .ingest_text(req, "version two body with more detail")
        .await
        .unwrap();
    assert!(second.changed);
    assert_eq!(first.document_id, second.document_id);
    assert_ne!(first.version_id, second.version_id, "new version id");

    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&h.runtime.sqlite);
    let doc = repo.get(&first.document_id).unwrap().unwrap();
    assert_eq!(
        doc.current_version_id.as_ref().map(|v| v.as_str()),
        Some(second.version_id.as_str())
    );
}

/// AC5: a local document is never stale purely by age.
#[tokio::test]
async fn local_document_is_never_stale_by_age() {
    let policy = searchos_policy::FreshnessPolicy::default_policy();
    let category = searchos_policy::classify_content("searchos-local://default/notes/x", None);
    assert_eq!(category, searchos_policy::ContentCategory::Local);
    // Far beyond the strict max age.
    assert!(policy.is_fresh(category, 10 * 365 * 24 * 3600));
}

/// P0-2: a refresh keeps the document's owning workspace.
#[tokio::test]
async fn document_refresh_keeps_workspace() {
    let h = Harness::new().await;
    let mut req = text_request("ws_refresh");
    req.source_key = Some("notes/refresh".into());
    let doc = h
        .runtime
        .application
        .ingest_service
        .ingest_text(req, "refresh me")
        .await
        .unwrap();

    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&h.runtime.sqlite);
    let stored = repo.get(&doc.document_id).unwrap().unwrap();
    assert_eq!(stored.workspace_id, "ws_refresh");

    // The CLI refresh path derives the workspace from the document row; the
    // repository seam must keep returning that owning workspace, never a
    // hard-coded default. A live fetch is not required to prove the seam.
    let payloads = h
        .runtime
        .application
        .fetch_service
        .reindex_current_version(&doc.document_id, &WorkspaceId::new(&stored.workspace_id))
        .await
        .unwrap();
    assert!(
        payloads.iter().all(|p| p.workspace_id == "ws_refresh"),
        "refresh payloads stay in the owning workspace: {payloads:?}"
    );
}

/// AC6: a document in workspace A is not retrievable from workspace B.
#[tokio::test]
async fn workspace_isolation_blocks_other_workspace() {
    let h = Harness::new().await;
    let mut req = text_request("ws_a");
    req.source_key = Some("private/alpha".into());
    h.runtime
        .application
        .ingest_service
        .ingest_text(req, "workspace A secret knowledge about zanzibar")
        .await
        .unwrap();
    h.drain().await;

    // Same query from ws_b must not see ws_a's document.
    let resp = h
        .service
        .search(request("zanzibar knowledge", "ws_b"))
        .await
        .unwrap();
    assert!(
        !resp
            .results
            .iter()
            .any(|r| r.content.to_lowercase().contains("zanzibar")),
        "cross-workspace leak: {:?}",
        resp.results
    );

    // And the owning workspace does see it.
    let resp_a = h
        .service
        .search(request("zanzibar knowledge", "ws_a"))
        .await
        .unwrap();
    assert!(
        resp_a
            .results
            .iter()
            .any(|r| r.content.to_lowercase().contains("zanzibar")),
        "owner workspace cannot see its own document: {:?}",
        resp_a.results
    );
}

/// AC8: document listing filters by source type and workspace.
#[tokio::test]
async fn document_list_filters_by_source_type() {
    let h = Harness::new().await;
    let mut req = text_request("default");
    req.source_key = Some("notes/listed".into());
    h.runtime
        .application
        .ingest_service
        .ingest_text(req, "list me please")
        .await
        .unwrap();

    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&h.runtime.sqlite);
    let local = repo.list_filtered(50, Some("local_text"), None).unwrap();
    assert!(
        local
            .iter()
            .any(|d| d.canonical_url == "searchos-local://default/notes/listed"),
        "local document listed: {:?}",
        local
    );
    let web = repo.list_filtered(50, Some("web"), None).unwrap();
    assert!(
        !web.iter()
            .any(|d| d.canonical_url == "searchos-local://default/notes/listed"),
        "local document must not appear in the web filter"
    );
    let in_ws = repo.list_filtered(50, None, Some("default")).unwrap();
    assert!(in_ws
        .iter()
        .any(|d| d.canonical_url == "searchos-local://default/notes/listed"));
}

/// AC9: batch import reports per-item outcomes and continues past failures.
#[tokio::test]
async fn batch_import_reports_outcomes() {
    let h = Harness::new().await;
    let items = vec![
        IngestBatchItem {
            request: {
                let mut r = text_request("default");
                r.source_key = Some("batch/one".into());
                r
            },
            payload: IngestPayload::Text("first batch body".into()),
        },
        IngestBatchItem {
            request: {
                let mut r = text_request("default");
                r.source_key = Some("batch/two".into());
                r
            },
            payload: IngestPayload::Text("second batch body".into()),
        },
        IngestBatchItem {
            // Empty content is a per-item failure, not a batch abort.
            request: {
                let mut r = text_request("default");
                r.source_key = Some("batch/empty".into());
                r
            },
            payload: IngestPayload::Text("   ".into()),
        },
    ];
    let result = h
        .runtime
        .application
        .ingest_service
        .ingest_batch(items)
        .await
        .unwrap();
    assert_eq!(result.total_items, 3);
    assert_eq!(result.ok_items, 2, "two items should succeed");
    assert_eq!(result.failed_items, 1, "empty content should fail");
    assert_eq!(result.state, "partial");
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].source_key, "batch/empty");

    // Job tracking rows exist.
    let jobs = searchos_storage_sqlite::repos::IngestJobRepo::new(&h.runtime.sqlite);
    let job = jobs.get(&result.job_id).unwrap().expect("job recorded");
    assert_eq!(job.state, "partial");
    assert_eq!(job.ok_items, 2);
    assert_eq!(job.failed_items, 1);
    let job_items = jobs.list_items(&result.job_id).unwrap();
    assert_eq!(job_items.len(), 3);
}

/// AC10: `rebuild_from` reconstructs imported documents.
#[tokio::test]
async fn rebuild_restores_imported_documents() {
    let h = Harness::new().await;
    let mut req = text_request("default");
    req.source_key = Some("notes/rebuild".into());
    let doc = h
        .runtime
        .application
        .ingest_service
        .ingest_text(req, "rebuild me: quokka habitat on rotte island")
        .await
        .unwrap();
    h.drain().await;

    // Rebuild the index from SQLite + object store.
    let rebuilt = h.runtime.rebuild_index().await.unwrap();
    assert!(rebuilt >= 1, "index rebuilt with imported chunks");

    let resp = h
        .service
        .search(request("quokka habitat", "default"))
        .await
        .unwrap();
    assert!(
        resp.results
            .iter()
            .any(|r| r.content.to_lowercase().contains("quokka")),
        "imported content survives rebuild: {:?}",
        resp.results
    );
    let _ = doc;
}
