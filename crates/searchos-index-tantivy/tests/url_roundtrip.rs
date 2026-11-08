#[tokio::test]
async fn url_roundtrip() {
    use searchos_domain::ids::*;
    use searchos_domain::retrieval::{IndexedDocument, RetrievalQuery};
    use searchos_retrieval::SearchIndex;
    let idx = searchos_index_tantivy::TantivyIndex::in_memory().unwrap();
    idx.index(IndexedDocument {
        chunk_id: ChunkId::new("c1"),
        document_id: DocumentId::new("d1"),
        version_id: DocumentVersionId::new("v1"),
        title: Some("T".into()),
        body: "raft consensus algorithm".into(),
        url: "https://raft.github.io".into(),
        language: Some("en".into()),
        workspace_id: "*".into(),
        fetched_at: chrono::Utc::now(),
    })
    .await
    .unwrap();
    idx.commit().await.unwrap();
    let hits = idx
        .search(&RetrievalQuery::new("raft consensus", 5))
        .await
        .unwrap();
    println!(
        "url={:?} title={:?} chunk={:?} ws={:?}",
        hits[0].url, hits[0].title, hits[0].chunk_id, hits[0].version_id
    );
}
