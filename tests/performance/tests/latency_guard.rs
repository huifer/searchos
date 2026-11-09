//! Performance sanity suite (RFC-016 Phase 8: benchmark).
//!
//! Not a full benchmark (that is the release-pipeline benchmark step); this is
//! a regression guard: local search latency stays bounded on a small
//! corpus. All numbers are Estimated machine-relative figures.

use std::sync::Arc;

use searchos_core::SearchOsConfig;
use searchos_core::SearchOsRuntime;
use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};
use searchos_domain::retrieval::{IndexedDocument, RetrievalQuery};

/// Builds a corpus of `n` chunks of ~500 chars each.
async fn build_corpus(rt: &SearchOsRuntime, n: usize) {
    for i in 0..n {
        let body = format!(
            "chunk {i}: rust tokio async runtime work stealing scheduler {}",
            "filler text about networking and storage. ".repeat(20)
        );
        rt.fulltext
            .index(IndexedDocument {
                chunk_id: ChunkId::new(format!("c{i}")),
                document_id: DocumentId::new(format!("d{}", i / 4)),
                version_id: DocumentVersionId::new(format!("v{}", i / 4)),
                title: Some(format!("Document {i}")),
                body,
                url: format!("https://example.com/doc/{i}"),
                language: Some("en".into()),
                workspace_id: "*".into(),
                fetched_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
    }
    rt.fulltext.commit().await.unwrap();
}

#[tokio::test]
async fn local_search_latency_bounded_on_2k_corpus() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    build_corpus(&rt, 2000).await;

    // Warmup.
    let q = RetrievalQuery::new("tokio async runtime", 10);
    let _ = rt.fulltext.search(&q).await.unwrap();

    let mut samples = Vec::new();
    for _ in 0..50 {
        let t = std::time::Instant::now();
        let hits = rt.fulltext.search(&q).await.unwrap();
        samples.push(t.elapsed().as_millis() as u64);
        assert!(!hits.is_empty());
    }
    samples.sort();
    let p95 = samples[samples.len() * 95 / 100];
    // Generous guard for CI machines (debug build): p95 under 250ms.
    assert!(p95 < 250, "local search p95 {p95}ms over budget");
    let _ = Arc::strong_count(&rt);
}

#[tokio::test]
async fn ingest_throughput_minimal_bound() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
    let t = std::time::Instant::now();
    build_corpus(&rt, 500).await;
    let elapsed = t.elapsed();
    // 500 chunks should commit in well under 10s on CI hardware.
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "ingest took {elapsed:?}"
    );
}
