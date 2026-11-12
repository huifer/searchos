use std::sync::Arc;

use searchos_domain::ids::WorkspaceId;
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, RetrievalQuery};
use searchos_domain::{ChunkId, DocumentId, SearchOsError};
use searchos_retrieval::SearchIndex;

#[tokio::test]
async fn runtime_assembles_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    // schema migrated
    assert_eq!(
        rt.sqlite.schema_version().unwrap(),
        searchos_storage_sqlite::migrations::MIGRATIONS
            .last()
            .unwrap()
            .version
    );
    // default workspace usable
    let ws = searchos_domain::ids::WorkspaceId::new("ws_default");
    searchos_storage_sqlite::repos::ensure_workspace(&rt.sqlite, &ws, "default").unwrap();
    // index an chunk through tantivy and check consistency vs sqlite (0 == 0)
    let (ti, si) = rt.index_consistency().await.unwrap();
    assert_eq!(ti, si);
}

/// Part 30 §30.11: a bad manifest is isolated and the failure is counted.
#[tokio::test]
async fn discovery_failure_is_logged_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("extensions").join("bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("manifest.yaml"), "this: is: not: an extension").unwrap();
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let before =
        searchos_observability::MetricsRegistry::global().counter("extensions.load_failed_total");
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    assert!(!rt.extensions.quarantined().is_empty());
    assert!(
        searchos_observability::MetricsRegistry::global().counter("extensions.load_failed_total")
            > before
    );
}

struct FailingIndex;

#[async_trait::async_trait]
impl SearchIndex for FailingIndex {
    async fn index(&self, _document: IndexedDocument) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn search(
        &self,
        _query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        Ok(Vec::new())
    }
    async fn delete(&self, _document_id: &DocumentId) -> Result<(), SearchOsError> {
        Err(SearchOsError::Storage("injected delete failure".into()))
    }
    async fn delete_by_chunk_ids(&self, _chunk_ids: &[ChunkId]) -> Result<(), SearchOsError> {
        Err(SearchOsError::Storage(
            "injected chunk delete failure".into(),
        ))
    }
    async fn count(&self) -> Result<u64, SearchOsError> {
        Ok(0)
    }
    async fn commit(&self) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn rebuild_from(&self, _documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        Ok(0)
    }
}

/// Part 30 §30.11: a failed index delete keeps the outbox row pending for retry.
#[tokio::test]
async fn delete_outbox_retry_after_index_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let mut req =
        searchos_application::ingest_service::IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("notes/delete-retry".into());
    let doc = rt
        .application
        .ingest_service
        .ingest_text(req, "delete me after injected index failure")
        .await
        .unwrap();
    rt.process_outbox(10).await.unwrap();
    searchos_storage_sqlite::repos::DocumentRepo::new(&rt.sqlite)
        .delete(&doc.document_id)
        .unwrap();
    let outbox = searchos_storage_sqlite::repos::OutboxRepo::new(&rt.sqlite);
    let pending_rows = outbox.pending(10).unwrap();
    let pending_before = pending_rows.len();
    assert!(pending_before >= 1);
    let row_id = pending_rows[0].id.clone();
    let (processed, dead) = rt
        .process_outbox_with_index(Arc::new(FailingIndex), 10)
        .await
        .unwrap();
    assert_eq!(processed, 0);
    assert_eq!(dead, 0, "first failure retries, does not DLQ");
    let (status, attempts): (String, i64) = rt
        .sqlite
        .with_conn(|c| {
            c.query_row(
                "SELECT status, attempts FROM index_outbox WHERE id = ?1",
                [row_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| SearchOsError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(attempts, 1, "failure is retried, not silently processed");
}

/// RFC-007 7.7: the runtime attaches a REAL reranker (not NeverReranker) when
/// enabled, and honors the disable switch.
#[tokio::test]
async fn reranker_is_wired_into_the_hybrid_retriever() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    assert!(
        rt.hybrid.has_reranker(),
        "a reranker must be attached by default"
    );
    assert_eq!(rt.hybrid.reranker_model_id(), Some("feature-reranker-v1"));

    let dir2 = tempfile::tempdir().unwrap();
    let mut cfg2 = searchos_core::SearchOsConfig::default();
    cfg2.app.data_dir = dir2.path().to_path_buf();
    cfg2.intelligence.reranking = false;
    let rt2 = searchos_core::SearchOsRuntime::init(cfg2).await.unwrap();
    assert!(!rt2.hybrid.has_reranker(), "disable switch is honored");
}

/// Agent sessions survive a process restart: the follow-up contract (open
/// requirements + turn history) is durable in SQLite (migration v7), not only
/// in the in-memory store.
#[tokio::test]
async fn agent_sessions_survive_runtime_restart() {
    use searchos_application::session::{SessionStore, SessionTurn};
    use searchos_domain::decision::CoverageGap;
    use searchos_domain::ids::TraceId;

    let dir = tempfile::tempdir().unwrap();
    let mk_cfg = || {
        let mut cfg = searchos_core::SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        cfg
    };

    let rt1 = searchos_core::SearchOsRuntime::init(mk_cfg())
        .await
        .unwrap();
    let store: Arc<SessionStore> = Arc::clone(&rt1.session_store);
    store.create_or_get("persist-1", "ws_default");
    store.record_turn(
        "persist-1",
        "ws_default",
        SessionTurn {
            query: "compare tokio and raft".into(),
            trace_id: TraceId::new("trace_p1"),
            status: "partial".into(),
            decision_path: "hybrid".into(),
            open_requirements: 1,
            recorded_at: chrono::Utc::now().to_rfc3339(),
        },
        vec![CoverageGap::new("raft consensus", "evidence")],
    );
    assert_eq!(store.get("persist-1").unwrap().turns.len(), 1);

    // Simulate a process restart: drop the runtime and the in-memory view...
    drop(store);
    drop(rt1);

    // ...then re-open the same data directory.
    let rt2 = searchos_core::SearchOsRuntime::init(mk_cfg())
        .await
        .unwrap();
    let recovered = rt2
        .session_store
        .get("persist-1")
        .expect("session recovered after restart");
    assert_eq!(recovered.workspace_id, "ws_default");
    assert_eq!(recovered.turns.len(), 1);
    assert_eq!(recovered.turns[0].query, "compare tokio and raft");
    assert_eq!(recovered.open_requirements.len(), 1);
    assert_eq!(recovered.open_requirements[0].question, "raft consensus");

    // Closing a session removes it durably too.
    rt2.session_store.close("persist-1");
    drop(rt2);
    let rt3 = searchos_core::SearchOsRuntime::init(mk_cfg())
        .await
        .unwrap();
    assert!(rt3.session_store.get("persist-1").is_none());
}
