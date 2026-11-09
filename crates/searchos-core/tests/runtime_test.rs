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
