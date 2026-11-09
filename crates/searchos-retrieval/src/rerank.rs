//! Optional reranking (RFC-007 7.7).
//!
//! Reranker 不默认执行 — triggers only when the policy says so; its cost is
//! billed through the cost ledger by the caller.

use async_trait::async_trait;
use searchos_domain::retrieval::{RetrievalCandidate, RetrievalQuery, RetrievalSource};
use serde::{Deserialize, Serialize};

/// Trigger policy (RFC-007 7.7 trigger conditions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankerPolicy {
    /// Rerank when fused candidate count exceeds this.
    pub candidate_threshold: usize,
    /// Rerank when BM25/vector top-overlap is below this ratio (divergence).
    pub divergence_threshold: f32,
    /// Profiles requiring high precision force reranking.
    pub force_for_high_precision_profiles: Vec<String>,
}

impl Default for RerankerPolicy {
    fn default() -> Self {
        Self {
            candidate_threshold: 30,
            divergence_threshold: 0.3,
            force_for_high_precision_profiles: vec!["research".into(), "latest".into()],
        }
    }
}

impl RerankerPolicy {
    /// Decides whether reranking should run for this query (RFC-007 7.7).
    pub fn should_rerank(
        &self,
        bm25: &[RetrievalCandidate],
        vector: &[RetrievalCandidate],
        fused_len: usize,
    ) -> bool {
        if fused_len > self.candidate_threshold {
            return true;
        }
        if !bm25.is_empty() && !vector.is_empty() {
            let overlap = top_overlap(bm25, vector, 10);
            if overlap < self.divergence_threshold {
                return true;
            }
        }
        false
    }

    /// Profile-based forcing (used by the application layer).
    pub fn force_for_profile(&self, profile_id: &str) -> bool {
        self.force_for_high_precision_profiles
            .iter()
            .any(|p| p == profile_id)
    }
}

fn top_overlap(a: &[RetrievalCandidate], b: &[RetrievalCandidate], k: usize) -> f32 {
    let sa: std::collections::HashSet<&str> =
        a.iter().take(k).map(|c| c.chunk_id.as_str()).collect();
    let sb: std::collections::HashSet<&str> =
        b.iter().take(k).map(|c| c.chunk_id.as_str()).collect();
    let inter = sa.intersection(&sb).count();
    if sa.is_empty() {
        return 0.0;
    }
    inter as f32 / sa.len() as f32
}

/// Reranker trait (pluggable provider, RFC Part 22).
#[async_trait]
pub trait Reranker: Send + Sync {
    /// Model identifier billed to the cost ledger.
    fn model_id(&self) -> &str;

    /// Reorders candidates; output must keep RetrievalSource::Reranked.
    async fn rerank(
        &self,
        query: &RetrievalQuery,
        candidates: Vec<RetrievalCandidate>,
    ) -> Result<Vec<RetrievalCandidate>, searchos_domain::SearchOsError>;
}

/// MVP default: no-op reranker that keeps the fused order but marks it
/// reranked — a placeholder until a real model provider is plugged in.
pub struct NeverReranker;

#[async_trait]
impl Reranker for NeverReranker {
    fn model_id(&self) -> &str {
        "none"
    }

    async fn rerank(
        &self,
        _query: &RetrievalQuery,
        mut candidates: Vec<RetrievalCandidate>,
    ) -> Result<Vec<RetrievalCandidate>, searchos_domain::SearchOsError> {
        for c in &mut candidates {
            c.source = RetrievalSource::Reranked;
        }
        Ok(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};

    fn cand(chunk: &str, source: RetrievalSource) -> RetrievalCandidate {
        RetrievalCandidate {
            chunk_id: ChunkId::new(chunk),
            document_id: DocumentId::new("doc"),
            version_id: DocumentVersionId::new("docv"),
            title: None,
            snippet: String::new(),
            url: String::new(),
            score: 1.0,
            source,
            fetched_at: Utc::now(),
            vector_score: None,
            bm25_score: None,
            workspace_id: None,
            language: None,
        }
    }

    #[test]
    fn triggers_on_divergence() {
        let p = RerankerPolicy::default();
        let bm25: Vec<_> = (0..10)
            .map(|i| cand(&format!("b{i}"), RetrievalSource::Bm25))
            .collect();
        let vec: Vec<_> = (0..10)
            .map(|i| cand(&format!("v{i}"), RetrievalSource::Vector))
            .collect();
        assert!(p.should_rerank(&bm25, &vec, 10)); // zero overlap
    }

    #[test]
    fn no_trigger_on_consensus_small_sets() {
        let p = RerankerPolicy::default();
        let bm25: Vec<_> = (0..5)
            .map(|i| cand(&format!("c{i}"), RetrievalSource::Bm25))
            .collect();
        let vec: Vec<_> = (0..5)
            .map(|i| cand(&format!("c{i}"), RetrievalSource::Vector))
            .collect();
        assert!(!p.should_rerank(&bm25, &vec, 5));
    }

    #[tokio::test]
    async fn never_reranker_marks_and_keeps_order() {
        let r = NeverReranker;
        let q = RetrievalQuery::new("test", 5);
        let out = r
            .rerank(
                &q,
                vec![
                    cand("a", RetrievalSource::Hybrid),
                    cand("b", RetrievalSource::Hybrid),
                ],
            )
            .await
            .unwrap();
        assert!(out.iter().all(|c| c.source == RetrievalSource::Reranked));
        assert_eq!(out[0].chunk_id.as_str(), "a");
    }
}
