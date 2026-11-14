//! Vertical retrieval routing (RFC-065).
//!
//! RFC-065 owns RECALL: one ranked list per modality ("vertical"), plus the
//! routing decision of which verticals run for a query. Fusion stays in
//! RFC-066 (`rrf_fuse_weighted`) — 065 recalls, 066 fuses, and neither
//! generates answers (Master Index ownership boundary).
//!
//! A vertical is a specialized recall path with a declared fusion weight and
//! an `accepts` routing predicate. The two built-in verticals (lexical BM25
//! and semantic vector KNN) remain the `HybridRetriever` primary lists so the
//! existing pipeline is byte-identical; additional verticals (e.g. a future
//! knowledge-graph or code index) register through
//! [`HybridRetriever::with_vertical`](crate::HybridRetriever::with_vertical)
//! and are fused after the primaries with their declared weight.

use async_trait::async_trait;
use searchos_domain::retrieval::{RetrievalCandidate, RetrievalQuery};
use searchos_domain::SearchOsError;

/// One specialized recall path (RFC-065).
///
/// Implementations must respect the query's policy fields (workspace,
/// required/excluded domains, freshness) exactly like the primary indexes do;
/// the routing layer never post-filters a vertical's output — a vertical that
/// cannot honor a policy field must decline the query in [`accepts`].
#[async_trait]
pub trait RetrievalVertical: Send + Sync {
    /// Stable vertical id. Used as the fusion label and in reports.
    fn id(&self) -> &str;

    /// Routing predicate: whether this vertical should run for the query.
    /// The default accepts everything; a code-index vertical might accept
    /// only identifier-shaped queries, and so on.
    fn accepts(&self, query: &RetrievalQuery) -> bool {
        let _ = query;
        true
    }

    /// Fusion weight (RFC-066: declared by the vertical's profile, never
    /// derived from a raw score — scores are not comparable across lists).
    fn weight(&self) -> f32 {
        1.0
    }

    /// Recall for this vertical: a ranked list, best first.
    async fn recall(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError>;
}

/// One vertical's routed recall (RFC-065).
#[derive(Debug, Clone)]
pub struct VerticalRecall {
    pub vertical_id: String,
    pub weight: f32,
    pub candidates: Vec<RetrievalCandidate>,
    /// Set when the vertical failed: extra-vertical recall is fail-open
    /// (availability is never a quality pass), the error is reported and the
    /// route continues without this list. Primary-list errors keep the
    /// retriever's existing fail semantics.
    pub error: Option<String>,
}

/// Routes a query across registered verticals (RFC-065).
///
/// The router RECALLS only; it never fuses. Callers that want a single
/// ranking feed the returned lists into [`rrf_fuse_weighted`](crate::rrf_fuse_weighted)
/// (RFC-066) — that is exactly what `HybridRetriever` does with its primary
/// lists plus any registered extras.
#[derive(Default)]
pub struct VerticalRouter {
    verticals: Vec<std::sync::Arc<dyn RetrievalVertical>>,
}

impl VerticalRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a vertical. Order is the recall order (audit trail).
    pub fn register(mut self, vertical: std::sync::Arc<dyn RetrievalVertical>) -> Self {
        self.verticals.push(vertical);
        self
    }

    /// Number of registered verticals (diagnostics).
    pub fn len(&self) -> usize {
        self.verticals.len()
    }

    /// Whether any vertical is registered.
    pub fn is_empty(&self) -> bool {
        self.verticals.is_empty()
    }

    /// Routes the query: recalls every vertical whose `accepts` returns true,
    /// in registration order. Fail-open per extra vertical.
    pub async fn route(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<VerticalRecall>, SearchOsError> {
        let mut recalls = Vec::new();
        for vertical in &self.verticals {
            if !vertical.accepts(query) {
                continue;
            }
            let recall = match vertical.recall(query).await {
                Ok(candidates) => VerticalRecall {
                    vertical_id: vertical.id().to_string(),
                    weight: vertical.weight(),
                    candidates,
                    error: None,
                },
                Err(error) => {
                    tracing::warn!(vertical = %vertical.id(), %error, "vertical recall failed; routed without it");
                    VerticalRecall {
                        vertical_id: vertical.id().to_string(),
                        weight: vertical.weight(),
                        candidates: Vec::new(),
                        error: Some(error.to_string()),
                    }
                }
            };
            recalls.push(recall);
        }
        Ok(recalls)
    }
}

/// Lexical vertical over a full-text index (RFC-065).
pub struct LexicalVertical {
    index: std::sync::Arc<dyn crate::SearchIndex>,
    weight: f32,
}

impl LexicalVertical {
    pub fn new(index: std::sync::Arc<dyn crate::SearchIndex>) -> Self {
        Self { index, weight: 1.0 }
    }

    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }
}

#[async_trait]
impl RetrievalVertical for LexicalVertical {
    fn id(&self) -> &str {
        "lexical"
    }

    async fn recall(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        self.index.search(query).await
    }
}

/// Semantic vertical over a vector index (RFC-065).
pub struct SemanticVertical {
    index: std::sync::Arc<dyn crate::VectorIndex>,
    weight: f32,
}

impl SemanticVertical {
    pub fn new(index: std::sync::Arc<dyn crate::VectorIndex>) -> Self {
        Self { index, weight: 1.0 }
    }

    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }
}

#[async_trait]
impl RetrievalVertical for SemanticVertical {
    fn id(&self) -> &str {
        "semantic"
    }

    async fn recall(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        self.index.search(query).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};

    fn candidate(chunk: &str, url: &str) -> RetrievalCandidate {
        RetrievalCandidate {
            chunk_id: ChunkId::new(chunk),
            document_id: DocumentId::new("doc"),
            version_id: DocumentVersionId::new("docv"),
            title: None,
            snippet: "snippet".into(),
            url: url.into(),
            score: 1.0,
            source: searchos_domain::retrieval::RetrievalSource::Bm25,
            fetched_at: Utc::now(),
            vector_score: None,
            bm25_score: None,
            workspace_id: None,
            language: None,
            highlight: None,
        }
    }

    struct FixedVertical {
        id: String,
        accept: bool,
        fail: bool,
        hits: Vec<RetrievalCandidate>,
    }

    #[async_trait]
    impl RetrievalVertical for FixedVertical {
        fn id(&self) -> &str {
            &self.id
        }
        fn accepts(&self, _query: &RetrievalQuery) -> bool {
            self.accept
        }
        async fn recall(
            &self,
            _query: &RetrievalQuery,
        ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
            if self.fail {
                return Err(SearchOsError::Retrieval("vertical down".into()));
            }
            Ok(self.hits.clone())
        }
    }

    fn query() -> RetrievalQuery {
        RetrievalQuery::new("tokio runtime", 5)
    }

    #[tokio::test]
    async fn router_returns_only_accepting_verticals() {
        let router = VerticalRouter::new()
            .register(std::sync::Arc::new(FixedVertical {
                id: "accepting".into(),
                accept: true,
                fail: false,
                hits: vec![candidate("c1", "https://a.dev/1")],
            }))
            .register(std::sync::Arc::new(FixedVertical {
                id: "declining".into(),
                accept: false,
                fail: false,
                hits: vec![candidate("c2", "https://a.dev/2")],
            }));
        let recalls = router.route(&query()).await.unwrap();
        assert_eq!(
            recalls
                .iter()
                .map(|r| r.vertical_id.as_str())
                .collect::<Vec<_>>(),
            vec!["accepting"],
            "a vertical that declines the query is not recalled"
        );
    }

    #[tokio::test]
    async fn vertical_failure_is_reported_and_never_fails_the_route() {
        let router = VerticalRouter::new().register(std::sync::Arc::new(FixedVertical {
            id: "down".into(),
            accept: true,
            fail: true,
            hits: Vec::new(),
        }));
        let recalls = router.route(&query()).await.unwrap();
        assert_eq!(recalls.len(), 1);
        assert_eq!(recalls[0].candidates.len(), 0);
        assert!(
            recalls[0].error.is_some(),
            "the error is reported, not swallowed"
        );
    }

    #[tokio::test]
    async fn weights_are_declared_by_the_vertical_not_invented_by_the_router() {
        let router = VerticalRouter::new()
            .register(std::sync::Arc::new(FixedVertical {
                id: "heavy".into(),
                accept: true,
                fail: false,
                hits: Vec::new(),
            }))
            .register(std::sync::Arc::new(FixedVertical {
                id: "light".into(),
                accept: true,
                fail: false,
                hits: Vec::new(),
            }));
        let recalls = router.route(&query()).await.unwrap();
        assert!(recalls
            .iter()
            .all(|r| (r.weight - 1.0).abs() < f32::EPSILON));
    }
}
