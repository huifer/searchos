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
        // The RUNTIME's coordinator: `invalidate_caches` clears THIS
        // instance, so a harness must share it, never build its own.
        singleflight: runtime.singleflight.clone(),
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
        session_store: None,
        federation_registry: None,
        federation_strategy: "legacy".into(),
        federation_limits: searchos_domain::FederationLimits::default(),
        federation_selection: "reliability_first".into(),
        quality_learner: None,
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

/// Hybrid retrieval must actually have a vector leg at runtime (G-02):
/// `VectorIndex::upsert` used to have zero production callers, so the
/// LanceDB table stayed empty and RRF fused BM25 against nothing. The
/// outbox write path must populate vectors for every indexed chunk — and
/// remove them again on delete.
#[tokio::test]
async fn runtime_write_path_populates_and_clears_vectors() {
    let h = Harness::new().await;
    use searchos_retrieval::VectorIndex as _;

    let mut req = IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("vec/hybrid".into());
    req.title = Some("Hybrid vector wiring".into());
    let doc = h
        .runtime
        .application
        .ingest_service
        .ingest_text(
            req,
            "Unique hybrid recall probe: vectorupsert wrote a dense row for this chunk.",
        )
        .await
        .unwrap();
    h.drain().await;

    let after_upsert = h.runtime.lancedb.count().await.unwrap();
    assert!(
        after_upsert >= doc.chunk_count as u64,
        "vector rows must exist after the outbox drain, got {after_upsert}"
    );

    // Delete removes both legs.
    h.runtime.delete_document(&doc.document_id).await.unwrap();
    let after_delete = h.runtime.lancedb.count().await.unwrap();
    assert_eq!(after_delete, 0, "vector rows must be removed on delete");
}

/// RFC-087: ingest-time secret redaction is wired from config to the real
/// runtime — secrets never reach object storage or the index, and an
/// explicit config opt-out keeps the original bytes.
#[tokio::test]
async fn ingest_time_secret_redaction_is_wired_through_config() {
    // Programmatic secret shape: no credential-looking literal in the tree.
    let secret_value = format!("sk-{}{}", "Ab3Cd5Ef7Gh9Jk2", "Lm4Np6Qr8St0Uv2");
    let text = format!(
        "Ledger entry: OPENAI_API_KEY = {secret_value} and the quarterly totals are public."
    );

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let mut req = text_request("default");
    req.source_key = Some("notes/secret".into());
    let doc = runtime
        .application
        .ingest_service
        .ingest_text(req, &text)
        .await
        .unwrap();
    let report = doc
        .redaction
        .as_ref()
        .expect("redaction report is returned");
    assert!(report.redacted);
    assert!(!report.hits.is_empty());

    runtime.process_outbox(256).await.unwrap();
    let response = runtime
        .application
        .search_service
        .search(request("quarterly totals", "default"))
        .await
        .unwrap();
    let snippets: String = response
        .results
        .iter()
        .map(|r| r.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !snippets.contains(&secret_value),
        "the secret is not searchable: {snippets}"
    );
    assert!(
        snippets.contains("[REDACTED:"),
        "the redacted form is what got indexed: {snippets}"
    );

    // Explicit operator opt-out: config field is consumed (INV-30-20) and
    // the original bytes are preserved.
    let dir2 = tempfile::tempdir().unwrap();
    let mut cfg2 = SearchOsConfig::default();
    cfg2.app.data_dir = dir2.path().to_path_buf();
    cfg2.ingest.redact_secrets = false;
    let runtime2 = searchos_core::SearchOsRuntime::init(cfg2).await.unwrap();
    let mut req2 = text_request("default");
    req2.source_key = Some("notes/secret".into());
    let doc2 = runtime2
        .application
        .ingest_service
        .ingest_text(req2, &text)
        .await
        .unwrap();
    assert!(
        doc2.redaction.is_none(),
        "with_redaction(false) disables the whole pass"
    );
}

/// RFC-087: the opt-in PII pass is consumed from config end to end.
#[tokio::test]
async fn pii_redaction_follows_the_config_flag() {
    // OFF (default): contact details stay retrievable knowledge.
    let dir_off = tempfile::tempdir().unwrap();
    let mut cfg_off = SearchOsConfig::default();
    cfg_off.app.data_dir = dir_off.path().to_path_buf();
    let rt_off = searchos_core::SearchOsRuntime::init(cfg_off).await.unwrap();
    let email = "ops-team@reserved-example.org";
    let text = format!("ping {email} about the quarterly review notes");
    let mut req = text_request("default");
    req.source_key = Some("notes/oncall".into());
    let doc_off = rt_off
        .application
        .ingest_service
        .ingest_text(req, &text)
        .await
        .unwrap();
    assert!(
        doc_off.redaction.as_ref().map(|r| r.redacted) != Some(true),
        "PII pass is opt-in"
    );

    // ON: the same text loses its contact detail.
    let dir_on = tempfile::tempdir().unwrap();
    let mut cfg_on = SearchOsConfig::default();
    cfg_on.app.data_dir = dir_on.path().to_path_buf();
    cfg_on.ingest.redact_pii = true;
    let rt_on = searchos_core::SearchOsRuntime::init(cfg_on).await.unwrap();
    let mut req_on = text_request("default");
    req_on.source_key = Some("notes/oncall".into());
    let doc_on = rt_on
        .application
        .ingest_service
        .ingest_text(req_on, &text)
        .await
        .unwrap();
    let report = doc_on.redaction.as_ref().expect("PII report returned");
    assert!(report.hits.iter().any(|h| h.rule == "email"));
    rt_on.process_outbox(256).await.unwrap();
    let stored_via_search = rt_on
        .application
        .search_service
        .search(request("quarterly review notes", "default"))
        .await
        .unwrap();
    let snippets: String = stored_via_search
        .results
        .iter()
        .map(|r| r.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!snippets.contains(email), "{snippets}");
    assert!(snippets.contains("[REDACTED:email]"), "{snippets}");
}

/// RFC-087: every delete writes an append-only propagation audit row whose
/// `index_verified` re-checks the drained outbox; a phantom delete writes
/// nothing, and the deleted knowledge is verifiably unsearchable.
#[tokio::test]
async fn deletion_propagation_is_audited() {
    let h = Harness::new().await;
    let mut req = text_request("default");
    req.source_key = Some("notes/audit".into());
    let doc = h
        .runtime
        .application
        .ingest_service
        .ingest_text(
            req,
            "The audit trail records what a deletion propagated across stores.",
        )
        .await
        .unwrap();
    h.drain().await;
    // Warm the caches so `cache_invalidated` refers to something real.
    eprintln!("DEBUG === warm search begin");
    let _ = h
        .service
        .search(request("audit trail propagated", "default"))
        .await
        .unwrap();
    eprintln!("DEBUG === warm search done");

    h.runtime.delete_document(&doc.document_id).await.unwrap();

    let repo = searchos_storage_sqlite::repos::DeletionAuditRepo::new(&h.runtime.sqlite);
    let rows = repo
        .list(10, Some("default"), Some(doc.document_id.as_str()))
        .unwrap();
    assert_eq!(rows.len(), 1, "exactly one audit row per real delete");
    let row = &rows[0];
    assert!(row.chunk_count > 0, "chunks were removed: {row:?}");
    assert_eq!(row.outbox_events, row.chunk_count);
    assert!(
        row.index_verified,
        "the drain left no pending index-delete event"
    );
    assert!(row.cache_invalidated);
    assert_eq!(row.workspace_id.as_deref(), Some("default"));
    assert!(
        row.content_hash.is_some(),
        "the deleted version is identified"
    );

    // A phantom delete is a no-op and writes NO audit row.
    h.runtime.delete_document(&doc.document_id).await.unwrap();
    assert_eq!(
        repo.list(10, Some("default"), Some(doc.document_id.as_str()))
            .unwrap()
            .len(),
        1,
        "no audit row for a document that did not exist"
    );

    // The deleted knowledge is verifiably unsearchable.
    eprintln!("DEBUG === final search begin");
    let resp = h
        .service
        .search(request("audit trail propagated", "default"))
        .await
        .unwrap();
    eprintln!("DEBUG === final search done");
    let mut q = searchos_domain::retrieval::RetrievalQuery::new("audit trail propagated", 10);
    q.workspace_id = Some("default".into());
    assert!(
        !resp.results.iter().any(|r| r.url == doc.canonical_url),
        "deleted knowledge is not served"
    );
}

/// RFC-036 v1: ingest computes an extractive summary and keywords from the
/// stored text, persists them with the version, and survives a rebuild.
#[tokio::test]
async fn ingest_stores_document_intelligence() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();

    let mut req = IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("notes/intelligence".into());
    let doc = rt
        .application
        .ingest_service
        .ingest_text(
            req,
            "SearchOS extracts an extractive summary at ingest time. The summary is verbatim source text. Keywords are tokens observed in the document.",
        )
        .await
        .unwrap();

    let version = searchos_storage_sqlite::repos::DocumentRepo::new(&rt.sqlite)
        .get_current_version(&doc.document_id)
        .unwrap()
        .expect("current version persisted");
    assert!(
        version.summary.is_some(),
        "a sentence-rich document gets a summary"
    );
    let summary = version.summary.unwrap();
    assert!(
        summary.contains("SearchOS extracts an extractive summary at ingest time."),
        "the summary is verbatim: {summary}"
    );
    assert!(
        version.keywords.iter().any(|k| k == "searchos"),
        "observed keywords are stored: {:?}",
        version.keywords
    );
    assert!(
        !version.keywords.iter().any(|k| k == "the" || k == "and"),
        "stopwords are excluded"
    );

    // A rebuild preserves the derived fields (they live on the version row).
    rt.rebuild_index().await.unwrap();
    let after = searchos_storage_sqlite::repos::DocumentRepo::new(&rt.sqlite)
        .get_current_version(&doc.document_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.summary, Some(summary));
    assert_eq!(after.keywords, version.keywords);
}

#[tokio::test]
async fn short_documents_have_honest_empty_insights() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let mut req = IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("notes/tiny".into());
    let doc = rt
        .application
        .ingest_service
        .ingest_text(req, "ok")
        .await
        .unwrap();
    let version = searchos_storage_sqlite::repos::DocumentRepo::new(&rt.sqlite)
        .get_current_version(&doc.document_id)
        .unwrap()
        .unwrap();
    assert!(version.summary.is_none());
    assert!(version.keywords.is_empty());
}

/// RFC-030 v1: collections are workspace-scoped, idempotent to create, and
/// their counts hide soft-deleted documents without losing membership.
#[tokio::test]
async fn knowledge_collections_track_live_membership() {
    use searchos_domain::collection::Collection;
    use searchos_storage_sqlite::repos::CollectionRepo;

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let repo = CollectionRepo::new(&rt.sqlite);
    let ws = WorkspaceId::new("ws_collections");

    let collection = Collection::new(&ws, "Reading List", Some("curated".into()));
    let created = repo.ensure(&collection).unwrap();
    assert_eq!(created.id, collection.id);
    let same = repo.ensure(&collection).unwrap();
    assert_eq!(same.id, created.id, "create is idempotent");

    let mut req = IngestRequest::new(ws.clone());
    req.source_key = Some("notes/collection".into());
    let doc = rt
        .application
        .ingest_service
        .ingest_text(
            req,
            "A document that belongs to the reading list collection.",
        )
        .await
        .unwrap();
    repo.add_document(&created.id, &doc.document_id).unwrap();
    repo.add_document(&created.id, &doc.document_id).unwrap(); // idempotent

    let summaries = repo.list(Some("ws_collections")).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].document_count, 1);
    assert!(
        repo.list(Some("other_ws")).unwrap().is_empty(),
        "workspace scoped"
    );
    assert_eq!(
        repo.documents(&created.id).unwrap(),
        vec![doc.document_id.clone()]
    );

    // Soft-delete hides the member from the LIVE view but keeps membership.
    rt.delete_document(&doc.document_id).await.unwrap();
    assert_eq!(
        repo.list(Some("ws_collections")).unwrap()[0].document_count,
        0
    );
    assert!(repo.documents(&created.id).unwrap().is_empty());
    repo.remove_document(&created.id, &doc.document_id).unwrap();
    assert_eq!(repo.documents(&created.id).unwrap().len(), 0);
}
