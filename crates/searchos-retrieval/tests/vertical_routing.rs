//! RFC-065 vertical routing acceptance tests.
//!
//! The load-bearing properties:
//! 1. **Byte-identical default** — with no extra verticals registered, the
//!    retriever's fused output is exactly the two-list form it had before
//!    the vertical layer existed.
//! 2. **Routing + fusion** — an extra vertical's candidates join the fused
//!    ranking with its declared weight (RFC-066 semantics at the retriever).
//! 3. **Fail-open** — a failing extra vertical never fails the request.

use async_trait::async_trait;
use chrono::Utc;
use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};
use searchos_domain::retrieval::{
    IndexedDocument, RetrievalCandidate, RetrievalQuery, RetrievalSource,
};
use searchos_domain::SearchOsError;
use searchos_retrieval::{
    HybridRetriever, RankedSource, RetrievalVertical, RrfConfig, VerticalRouter,
};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Stub indexes: canned ranked lists, no engine.
// ---------------------------------------------------------------------------

struct StubIndex {
    hits: Vec<RetrievalCandidate>,
}

#[async_trait]
impl searchos_retrieval::SearchIndex for StubIndex {
    async fn index(&self, _document: IndexedDocument) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn search(
        &self,
        _query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        Ok(self.hits.clone())
    }
    async fn delete(&self, _document_id: &DocumentId) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn count(&self) -> Result<u64, SearchOsError> {
        Ok(self.hits.len() as u64)
    }
    async fn rebuild_from(&self, _documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        Ok(0)
    }
}

struct StubVector {
    hits: Vec<RetrievalCandidate>,
}

#[async_trait]
impl searchos_retrieval::VectorIndex for StubVector {
    async fn upsert(
        &self,
        _chunk_id: &str,
        _document_id: &str,
        _vector: Vec<f32>,
        _model_id: &str,
    ) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn search(
        &self,
        _query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        Ok(self.hits.clone())
    }
    async fn delete_document(&self, _document_id: &str) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn count(&self) -> Result<u64, SearchOsError> {
        Ok(self.hits.len() as u64)
    }
}

fn candidate(chunk: &str, url: &str) -> RetrievalCandidate {
    RetrievalCandidate {
        chunk_id: ChunkId::new(chunk),
        document_id: DocumentId::new("doc"),
        version_id: DocumentVersionId::new("docv"),
        title: None,
        snippet: "shared snippet text".into(),
        url: url.into(),
        score: 1.0,
        source: RetrievalSource::Bm25,
        fetched_at: Utc::now(),
        vector_score: None,
        bm25_score: None,
        workspace_id: None,
        language: None,
        highlight: None,
    }
}

/// A configurable extra vertical.
struct StubVertical {
    id: String,
    accept: bool,
    fail: bool,
    weight: f32,
    hits: Vec<RetrievalCandidate>,
}

#[async_trait]
impl RetrievalVertical for StubVertical {
    fn id(&self) -> &str {
        &self.id
    }
    fn accepts(&self, _query: &RetrievalQuery) -> bool {
        self.accept
    }
    fn weight(&self) -> f32 {
        self.weight
    }
    async fn recall(
        &self,
        _query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        if self.fail {
            return Err(SearchOsError::Retrieval("extra vertical down".into()));
        }
        Ok(self.hits.clone())
    }
}

fn retriever(bm25: Vec<RetrievalCandidate>, vector: Vec<RetrievalCandidate>) -> HybridRetriever {
    HybridRetriever::new(
        Arc::new(StubIndex { hits: bm25 }),
        Some(Arc::new(StubVector { hits: vector })),
        RrfConfig::default(),
    )
}

#[tokio::test]
async fn two_list_output_is_byte_identical_with_and_without_verticals() {
    let bm25 = vec![
        candidate("bm1", "https://a.dev/1"),
        candidate("shared", "https://a.dev/2"),
    ];
    let vector = vec![
        candidate("vec1", "https://b.dev/1"),
        candidate("shared", "https://b.dev/2"),
    ];
    let query = RetrievalQuery::new("q", 10);

    let plain = retriever(bm25.clone(), vector.clone())
        .retrieve(&query)
        .await
        .unwrap();
    assert_eq!(plain.len(), 3);

    // The same retriever with an EMPTY-vertical equivalent must match the
    // raw two-list fusion byte for byte (chunk ids, fused scores, order).
    let expected = searchos_retrieval::rrf_fuse_weighted(
        &[
            RankedSource::new("bm25", 1.0, &bm25),
            RankedSource::new("vector", 1.0, &vector),
        ],
        &RrfConfig::default(),
    );
    assert_eq!(
        plain
            .iter()
            .map(|c| (c.chunk_id.as_str(), c.score))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|c| (c.chunk_id.as_str(), c.score))
            .collect::<Vec<_>>(),
        "the vertical layer must not perturb the default two-list ranking"
    );
}

#[tokio::test]
async fn extra_vertical_candidates_join_the_fused_ranking() {
    let bm25 = vec![candidate("bm1", "https://a.dev/1")];
    let retriever = retriever(bm25, Vec::new()).with_vertical(Arc::new(StubVertical {
        id: "code_index".into(),
        accept: true,
        fail: false,
        weight: 1.0,
        hits: vec![candidate("code1", "https://code.dev/1")],
    }));
    assert_eq!(retriever.vertical_count(), 1);
    let query = RetrievalQuery::new("q", 10);
    let fused = retriever.retrieve(&query).await.unwrap();
    let chunks: Vec<&str> = fused.iter().map(|c| c.chunk_id.as_str()).collect();
    assert!(
        chunks.contains(&"code1"),
        "the extra vertical's candidate is fused in: {chunks:?}"
    );
}

#[tokio::test]
async fn extra_vertical_weight_orders_equal_rank_contributions() {
    // Two extra verticals each contribute ONE candidate absent from the
    // primaries; with equal ranks the heavier vertical's candidate must rank
    // first in the fused output.
    let heavy = Arc::new(StubVertical {
        id: "heavy".into(),
        accept: true,
        fail: false,
        weight: 1.0,
        hits: vec![candidate("heavy_hit", "https://h.dev/1")],
    });
    let light = Arc::new(StubVertical {
        id: "light".into(),
        accept: true,
        fail: false,
        weight: 0.1,
        hits: vec![candidate("light_hit", "https://l.dev/1")],
    });
    let retriever = retriever(Vec::new(), Vec::new())
        .with_vertical(heavy)
        .with_vertical(light);
    let fused = retriever
        .retrieve(&RetrievalQuery::new("q", 10))
        .await
        .unwrap();
    assert_eq!(fused.len(), 2);
    assert_eq!(
        fused[0].chunk_id.as_str(),
        "heavy_hit",
        "equal ranks: the declared weight decides the order"
    );
}

#[tokio::test]
async fn routing_skips_verticals_that_decline_the_query() {
    let declining = Arc::new(StubVertical {
        id: "declining".into(),
        accept: false,
        fail: false,
        weight: 1.0,
        hits: vec![candidate("declined", "https://d.dev/1")],
    });
    let retriever = retriever(Vec::new(), Vec::new()).with_vertical(declining);
    let fused = retriever
        .retrieve(&RetrievalQuery::new("q", 10))
        .await
        .unwrap();
    assert!(
        fused.is_empty(),
        "a vertical that declines the query is never recalled"
    );
}

#[tokio::test]
async fn extra_vertical_failure_is_isolated() {
    let failing = Arc::new(StubVertical {
        id: "down".into(),
        accept: true,
        fail: true,
        weight: 1.0,
        hits: Vec::new(),
    });
    let retriever =
        retriever(vec![candidate("bm1", "https://a.dev/1")], Vec::new()).with_vertical(failing);
    let fused = retriever
        .retrieve(&RetrievalQuery::new("q", 10))
        .await
        .unwrap();
    assert_eq!(
        fused.len(),
        1,
        "a failing extra vertical is skipped, not fatal"
    );
    assert_eq!(fused[0].chunk_id.as_str(), "bm1");
}

#[test]
fn router_exposes_the_registered_verticals_for_diagnostics() {
    let router = VerticalRouter::new()
        .register(Arc::new(StubVertical {
            id: "one".into(),
            accept: true,
            fail: false,
            weight: 1.0,
            hits: Vec::new(),
        }))
        .register(Arc::new(StubVertical {
            id: "two".into(),
            accept: true,
            fail: false,
            weight: 1.0,
            hits: Vec::new(),
        }));
    assert_eq!(router.len(), 2);
    assert!(!router.is_empty());
}
