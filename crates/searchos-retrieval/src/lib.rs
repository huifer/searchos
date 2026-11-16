//! Retrieval architecture (RFC-007).
//!
//! Pipeline: Exact Cache → Canonical Cache → BM25 → Vector → Metadata
//! Filtering → RRF Fusion → Optional Reranking → Evidence Evaluation.
//!
//! This crate owns the index/reranker traits and the fusion logic; concrete
//! adapters live in `searchos-index-tantivy` / `searchos-vector-lancedb`.

pub mod fusion;
pub mod rerank;
pub mod reranker_adapters;
pub mod traits;
pub mod vertical;

pub use fusion::{rrf_fuse, rrf_fuse_weighted, RankedSource, RrfConfig};
pub use rerank::{NeverReranker, Reranker, RerankerPolicy};
pub use reranker_adapters::{
    build_reranker, default_policy, is_workspace_owned, EgressRules, FeatureReranker,
    RemoteReranker, RemoteRerankerConfig,
};
pub use traits::{SearchIndex, VectorIndex};
pub use vertical::{
    LexicalVertical, RetrievalVertical, SemanticVertical, VerticalRecall, VerticalRouter,
};

use searchos_domain::retrieval::{RetrievalCandidate, RetrievalQuery};

/// A hybrid retriever combining BM25 and vector search with RRF.
pub struct HybridRetriever {
    bm25: std::sync::Arc<dyn SearchIndex>,
    vector: Option<std::sync::Arc<dyn VectorIndex>>,
    config: RrfConfig,
    reranker: Option<std::sync::Arc<dyn Reranker>>,
    policy: RerankerPolicy,
    /// RFC-065: specialized verticals fused after the two primary lists.
    /// Empty by default, which keeps the two-list behaviour byte-identical.
    verticals: Vec<std::sync::Arc<dyn vertical::RetrievalVertical>>,
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
            verticals: Vec::new(),
        }
    }

    /// Registers an additional retrieval vertical (RFC-065). Extra verticals
    /// are routed per query (`accepts`), recalled fail-open, and fused AFTER
    /// the primary lists with their declared weight (RFC-066). With no
    /// verticals registered the retriever is byte-identical to the two-list
    /// form.
    pub fn with_vertical(
        mut self,
        vertical: std::sync::Arc<dyn vertical::RetrievalVertical>,
    ) -> Self {
        self.verticals.push(vertical);
        self
    }

    /// Number of registered extra verticals (diagnostics).
    pub fn vertical_count(&self) -> usize {
        self.verticals.len()
    }

    /// Whether a reranker is attached (runtime wiring diagnostics).
    pub fn has_reranker(&self) -> bool {
        self.reranker.is_some()
    }

    /// Model id of the attached reranker, billed to the cost ledger.
    pub fn reranker_model_id(&self) -> Option<&str> {
        self.reranker.as_ref().map(|r| r.model_id())
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

    /// Runs BM25 (+vector +registered verticals), applies metadata filtering,
    /// fuses with weighted RRF, and optionally reranks (RFC-007 7.1).
    pub async fn retrieve(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, searchos_domain::SearchOsError> {
        let bm25_hits = self.bm25.search(query).await?;
        let vector_hits = match &self.vector {
            Some(v) => v.search(query).await?,
            None => Vec::new(),
        };
        // RFC-065: route the query across the registered verticals. Extra
        // verticals are fail-open — a failing vertical is reported through
        // `VerticalRecall.error` and the route continues without its list;
        // the primary lists keep their existing failure semantics (`?`).
        let mut extras = Vec::new();
        for registered in &self.verticals {
            if !registered.accepts(query) {
                continue;
            }
            let recall = match registered.recall(query).await {
                Ok(candidates) => vertical::VerticalRecall {
                    vertical_id: registered.id().to_string(),
                    weight: registered.weight(),
                    candidates,
                    error: None,
                },
                Err(error) => {
                    tracing::warn!(
                        vertical = %registered.id(),
                        %error,
                        "vertical recall failed; continuing without it"
                    );
                    vertical::VerticalRecall {
                        vertical_id: registered.id().to_string(),
                        weight: registered.weight(),
                        candidates: Vec::new(),
                        error: Some(error.to_string()),
                    }
                }
            };
            extras.push(recall);
        }
        // RFC-066: one weighted fusion over every routed list. With no extra
        // verticals this is exactly the two-list `rrf_fuse` form.
        let mut sources = Vec::with_capacity(2 + extras.len());
        sources.push(fusion::RankedSource::new("bm25", 1.0, &bm25_hits));
        sources.push(fusion::RankedSource::new("vector", 1.0, &vector_hits));
        for recall in &extras {
            sources.push(fusion::RankedSource::new(
                &recall.vertical_id,
                recall.weight,
                &recall.candidates,
            ));
        }
        let mut fused = rrf_fuse_weighted(&sources, &self.config);

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

/// Host extraction used by the domain policy filter.
///
/// Public because `searchos-federated` implements the *same* policy for
/// external results; a shared implementation is the only way the two paths
/// cannot drift, and the consistency is locked by a cross-crate test
/// (`local_and_external_domain_policy_agree`).
pub fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    Some(host.split(':').next()?.to_lowercase())
}
