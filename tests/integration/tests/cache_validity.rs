//! RFC-077 acceptance: hit-time evidence validity across processes.
//!
//! The runtime clears its in-process caches on mutation — but only in the
//! SAME process. When another process (CLI ingest, a second replica) writes
//! to the shared data directory, warm in-memory caches go stale without any
//! local signal. These tests simulate exactly that: a second `SqliteStore`
//! on the same directory performs the mutation, the first runtime NEVER
//! clears its caches, and the next search must still be correct because the
//! exact-cache hit is validated against the durable store before reuse.

use std::sync::Arc;

use searchos_application::ingest_service::IngestRequest;
use searchos_core::SearchOsConfig;
use searchos_domain::ids::{DocumentId, ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_storage_sqlite::repos;
use searchos_storage_sqlite::store::SqliteStore;

const ARTICLE: &str = "The tokio scheduler redesign reduces tail latency under high fanout \
     by redesigning the task budget accounting model across worker threads.";

/// The runtime's database lives one level down: `<data_dir>/data/searchos.db`
/// (see `SearchOsConfig::data_db_path`). The "other process" must open the
/// SAME file or the simulation is meaningless.
fn shared_db(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("data").join("searchos.db")
}

async fn runtime_on(dir: &std::path::Path) -> Arc<searchos_core::SearchOsRuntime> {
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.to_path_buf();
    searchos_core::SearchOsRuntime::init(cfg).await.unwrap()
}

fn request(query: &str) -> SearchRequest {
    SearchRequest::new(
        query,
        ProfileId::general(),
        SearchMode::LocalOnly,
        WorkspaceId::new("default"),
    )
    .unwrap()
}

/// The "other process": opens the shared database file directly and
/// soft-deletes the document (outbox rows included), leaving the first
/// runtime's in-memory caches untouched.
fn delete_from_another_process(dir: &std::path::Path, document_id: &DocumentId) {
    let store = SqliteStore::open(shared_db(dir)).unwrap();
    repos::DocumentRepo::new(&store)
        .delete(document_id)
        .unwrap();
}

#[tokio::test]
async fn deletion_in_another_process_invalidates_cached_responses() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;

    let mut req = IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("notes/scheduler".into());
    let doc = rt
        .application
        .ingest_service
        .ingest_text(req, ARTICLE)
        .await
        .unwrap();
    // Ingest writes go through the outbox; drain so the chunks are searchable.
    rt.process_outbox(256).await.unwrap();

    // Every search uses a FRESH request id: an identical request id is a
    // durable IDEMPOTENCY replay (INV-30-02 must return the recorded payload
    // verbatim) and bypasses the cache layers entirely — the RFC-077 gate
    // guards the caches, never the idempotency contract.
    let first = rt
        .application
        .search_service
        .search(request("tokio scheduler tail latency"))
        .await
        .unwrap();
    assert!(
        first
            .provenance
            .iter()
            .any(|p| p.provenance.document_id.as_ref() == Some(&doc.document_id)),
        "fresh search serves the ingested document"
    );

    // Second identical search: served from the exact cache (replayed).
    let cached = rt
        .application
        .search_service
        .search(request("tokio scheduler tail latency"))
        .await
        .unwrap();
    assert!(cached.replayed, "the warm cache serves the second search");

    // Another process deletes the document. This runtime's caches are NOT
    // cleared — only the durable store changes.
    delete_from_another_process(dir.path(), &doc.document_id);
    // Sanity: the deletion is visible to a fresh handle on the shared file.
    {
        let check = SqliteStore::open(shared_db(dir.path())).unwrap();
        let (status, version): (String, Option<String>) = check
            .with_conn(|c| {
                c.query_row(
                    "SELECT status, current_version_id FROM documents WHERE id = ?1",
                    rusqlite::params![doc.document_id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|er| searchos_domain::SearchOsError::Internal(er.to_string()))
            })
            .unwrap();
        assert_eq!(status, "deleted", "the shared file must show the deletion");
        assert!(version.is_none());
    }
    // The shared index must also lose the chunks: draining the outbox is
    // what any process (here or there) would do next.
    rt.process_outbox(256).await.unwrap();

    // The stale cache entry must now be REJECTED, not served.
    let after = rt
        .application
        .search_service
        .search(request("tokio scheduler tail latency"))
        .await
        .unwrap();
    assert!(
        !after
            .provenance
            .iter()
            .any(|p| p.provenance.document_id.as_ref() == Some(&doc.document_id)),
        "deleted knowledge is never served from a warm cache"
    );
    assert!(
        !after.replayed,
        "the response is recomputed, not a cache replay"
    );
}

#[tokio::test]
async fn version_change_in_another_process_rejects_the_cached_hit() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_on(dir.path()).await;

    let mut req = IngestRequest::new(WorkspaceId::new("default"));
    req.source_key = Some("notes/scheduler".into());
    let doc = rt
        .application
        .ingest_service
        .ingest_text(req, ARTICLE)
        .await
        .unwrap();
    // Ingest writes go through the outbox; drain so the chunks are searchable.
    rt.process_outbox(256).await.unwrap();

    let _first = rt
        .application
        .search_service
        .search(request("tokio scheduler task budget"))
        .await
        .unwrap();
    let cached = rt
        .application
        .search_service
        .search(request("tokio scheduler task budget"))
        .await
        .unwrap();
    assert!(cached.replayed, "warm cache serves the second search");

    // The "other process" moved the document to a NEWER version (a changed
    // re-ingest would do this). The cached candidates cite the OLD version.
    let store = SqliteStore::open(shared_db(dir.path())).unwrap();
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE documents SET current_version_id = 'docv_forced_next' WHERE id = ?1",
                rusqlite::params![doc.document_id.as_str()],
            )
            .map_err(|er| searchos_domain::SearchOsError::Internal(er.to_string()))
        })
        .unwrap();

    // The validity gate must reject the hit; the search is recomputed
    // (replayed == false even though the exact cache still holds an entry).
    let after = rt
        .application
        .search_service
        .search(request("tokio scheduler task budget"))
        .await
        .unwrap();
    assert!(
        !after.replayed,
        "a version drift invalidates the cached response"
    );
}
