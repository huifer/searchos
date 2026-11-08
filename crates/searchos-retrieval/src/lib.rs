//! Retrieval architecture (RFC-007).
//!
//! Pipeline: Exact Cache → Canonical Cache → BM25 → Vector → Metadata
//! Filtering → RRF Fusion → Optional Reranking → Evidence Evaluation.
//!
//! This crate owns the index/reranker traits and the fusion logic; concrete
//! adapters live in `searchos-index-tantivy` / `searchos-vector-lancedb`.

pub mod fusion;
pub mod rerank;
pub mod traits;

pub use fusion::{rrf_fuse, RrfConfig};
pub use rerank::{NeverReranker, Reranker, RerankerPolicy};
pub use traits::{SearchIndex, VectorIndex};

use searchos_domain::retrieval::{RetrievalCandidate, RetrievalQuery};

/// A hybrid retriever combining BM25 and vector search with RRF.
pub struct HybridRetriever {
    bm25: std::sync::Arc<dyn SearchIndex>,
    vector: Option<std::sync::Arc<dyn VectorIndex>>,
    config: RrfConfig,
    reranker: Option<std::sync::Arc<dyn Reranker>>,
    policy: RerankerPolicy,
}

impl HybridRetriever {
    /// Builds a hybrid retriever; vector search is optional (MVP Phase 2
    /// runs BM25-only).
    pub fn new(
        bm25: std::sync::Arc<dyn SearchIndex>,
        vector: Option<std::sync::Arc<dyn VectorIndex>>,
        config: RrfConfig,
    ) -> Self {
        Self {
            bm25,
            vector,
            config,
            reranker: None,
            policy: RerankerPolicy::default(),
        }
    }

    /// Attaches an optional reranker and its trigger policy (RFC-007 7.7).
    pub fn with_reranker(
        mut self,
        reranker: std::sync::Arc<dyn Reranker>,
        policy: RerankerPolicy,
    ) -> Self {
        self.reranker = Some(reranker);
        self.policy = policy;
        self
    }

    /// Runs BM25 (+vector), applies metadata filtering, fuses with RRF, and
    /// optionally reranks (RFC-007 7.1).
    pub async fn retrieve(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, searchos_domain::SearchOsError> {
        let bm25_hits = self.bm25.search(query).await?;
        let vector_hits = match &self.vector {
            Some(v) => v.search(query).await?,
            None => Vec::new(),
        };
        let mut fused = rrf_fuse(&bm25_hits, &vector_hits, &self.config);

        // Metadata filtering (domain constraints, RFC-007 7.5).
        fused.retain(|c| candidate_allowed(query, c));

        // Optional reranking — triggers only per policy (RFC-007 7.7).
        if let Some(reranker) = self.reranker.as_ref() {
            if self
                .policy
                .should_rerank(&bm25_hits, &vector_hits, fused.len())
            {
                fused = reranker.rerank(query, fused).await?;
            }
        }
        fused.truncate(query.top_k);
        Ok(fused)
    }
}

/// Metadata filtering: required/excluded domains and freshness cutoff.
pub fn candidate_allowed(query: &RetrievalQuery, c: &RetrievalCandidate) -> bool {
    let url_host = host_of(&c.url);
    if !query.required_domains.is_empty() {
        let ok = url_host
            .as_ref()
            .map(|h| {
                query
                    .required_domains
                    .iter()
                    .any(|d| h == d || h.ends_with(&format!(".{d}")))
            })
            .unwrap_or(false);
        if !ok {
            return false;
        }
    }
    if let Some(h) = &url_host {
        if query
            .excluded_domains
            .iter()
            .any(|d| h == d || h.ends_with(&format!(".{d}")))
        {
            return false;
        }
    }
    if let Some(after) = query.fetched_after {
        if !query.include_stale && c.fetched_at < after {
            return false;
        }
    }
    true
}

fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    Some(host.split(':').next()?.to_lowercase())
}
