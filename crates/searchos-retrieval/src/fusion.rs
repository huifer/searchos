//! Reciprocal Rank Fusion (RFC-007 7.5).
//!
//! RRF(d) = Σ 1 / (k + rank_i(d)), initial k = 60.

use searchos_domain::retrieval::{RetrievalCandidate, RetrievalSource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// RRF parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RrfConfig {
    /// Rank constant k (RFC default 60).
    pub k: f32,
    /// How many candidates to keep from each stage before fusion.
    pub per_stage_top_k: usize,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            k: 60.0,
            per_stage_top_k: 50,
        }
    }
}

/// Fuses two ranked candidate lists with RRF.
///
/// Candidates are matched by chunk id; scores from each source list are
/// preserved (`bm25_score` / `vector_score`) for rerank decisions.
pub fn rrf_fuse(
    bm25: &[RetrievalCandidate],
    vector: &[RetrievalCandidate],
    config: &RrfConfig,
) -> Vec<RetrievalCandidate> {
    let mut by_chunk: HashMap<String, RetrievalCandidate> = HashMap::new();
    let mut rrf_scores: HashMap<String, f32> = HashMap::new();

    for (rank, c) in bm25.iter().take(config.per_stage_top_k).enumerate() {
        let contribution = 1.0 / (config.k + (rank as f32 + 1.0));
        *rrf_scores.entry(c.chunk_id.to_string()).or_insert(0.0) += contribution;
        by_chunk
            .entry(c.chunk_id.to_string())
            .or_insert_with(|| {
                let mut m = c.clone();
                m.source = RetrievalSource::Hybrid;
                m
            })
            .bm25_score = Some(c.score);
    }
    for (rank, c) in vector.iter().take(config.per_stage_top_k).enumerate() {
        let contribution = 1.0 / (config.k + (rank as f32 + 1.0));
        *rrf_scores.entry(c.chunk_id.to_string()).or_insert(0.0) += contribution;
        by_chunk
            .entry(c.chunk_id.to_string())
            .or_insert_with(|| {
                let mut m = c.clone();
                m.source = RetrievalSource::Hybrid;
                m
            })
            .vector_score = Some(c.score);
    }

    let mut out: Vec<RetrievalCandidate> = by_chunk.into_values().collect();
    for c in &mut out {
        c.score = rrf_scores.get(c.chunk_id.as_str()).copied().unwrap_or(0.0);
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};

    fn cand(chunk: &str, score: f32, source: RetrievalSource) -> RetrievalCandidate {
        RetrievalCandidate {
            chunk_id: ChunkId::new(chunk),
            document_id: DocumentId::new("doc_1"),
            version_id: DocumentVersionId::new("docv_1"),
            title: None,
            snippet: "snippet".into(),
            url: "https://example.com".into(),
            score,
            source,
            fetched_at: Utc::now(),
            vector_score: None,
            bm25_score: None,
        }
    }

    #[test]
    fn consensus_candidate_ranks_first() {
        let bm25 = vec![
            cand("a", 10.0, RetrievalSource::Bm25),
            cand("b", 5.0, RetrievalSource::Bm25),
        ];
        let vector = vec![cand("a", 0.9, RetrievalSource::Vector)];
        let fused = rrf_fuse(&bm25, &vector, &RrfConfig::default());
        assert_eq!(fused[0].chunk_id.as_str(), "a");
        // score = 1/(60+1) [bm25 rank1] + 1/(60+1) [vector rank1]
        let expected = 2.0 / 61.0;
        assert!((fused[0].score - expected).abs() < 1e-5);
        assert!(fused[0].bm25_score.is_some() && fused[0].vector_score.is_some());
    }

    #[test]
    fn bm25_only_input_preserved() {
        let bm25 = vec![
            cand("a", 10.0, RetrievalSource::Bm25),
            cand("b", 5.0, RetrievalSource::Bm25),
        ];
        let fused = rrf_fuse(&bm25, &[], &RrfConfig::default());
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].chunk_id.as_str(), "a");
    }
}
