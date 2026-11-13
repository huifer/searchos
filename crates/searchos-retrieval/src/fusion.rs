//! Reciprocal Rank Fusion (RFC-007 7.5) and its trust-weighted generalisation
//! (RFC-066, final RFC Master Index).
//!
//! RRF(d) = Σ 1 / (k + rank_i(d)), initial k = 60.
//!
//! The generalised form takes an arbitrary number of ranked lists, each with a
//! source weight:
//!
//! Score(d) = Σ_s w_s / (k + rank_s(d))
//!
//! `rrf_fuse` is kept as the two-list compatibility wrapper so every existing
//! call site and test stays valid.

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

/// One ranked contribution to a multi-source fusion (RFC-066).
///
/// `weight` scales the list's RRF contribution. Weights must come from the
/// source profile (reliability / trust policy), never from a raw score:
/// scores from different sources are not comparable (RFC-062 9.2).
#[derive(Debug, Clone)]
pub struct RankedSource<'a> {
    /// Stable source label recorded on the fused candidate.
    pub name: &'a str,
    /// Weight applied to this list's RRF contribution. Non-negative; a
    /// weight of zero removes the list from the ranking without deleting it.
    pub weight: f32,
    /// The ranked list, best first.
    pub candidates: &'a [RetrievalCandidate],
}

impl<'a> RankedSource<'a> {
    pub fn new(name: &'a str, weight: f32, candidates: &'a [RetrievalCandidate]) -> Self {
        Self {
            name,
            weight,
            candidates,
        }
    }

    /// Neutral-weight source (weight = 1.0), i.e. plain unweighted RRF.
    pub fn neutral(name: &'a str, candidates: &'a [RetrievalCandidate]) -> Self {
        Self::new(name, 1.0, candidates)
    }
}

/// Fuses an arbitrary number of weighted ranked lists (RFC-066).
///
/// Candidates are matched by chunk id. The first list that contains a chunk
/// contributes its payload (title/snippet/url), which keeps the fused record
/// byte-identical to the two-list wrapper's behaviour for the same inputs.
/// Scores are NOT comparable across lists, so only ranks are combined; the
/// per-list raw scores are preserved in `bm25_score` / `vector_score` when the
/// source name identifies those stages.
pub fn rrf_fuse_weighted(
    sources: &[RankedSource<'_>],
    config: &RrfConfig,
) -> Vec<RetrievalCandidate> {
    let mut by_chunk: HashMap<String, RetrievalCandidate> = HashMap::new();
    let mut scores: HashMap<String, f32> = HashMap::new();

    for source in sources {
        for (rank, c) in source
            .candidates
            .iter()
            .take(config.per_stage_top_k)
            .enumerate()
        {
            let contribution = source.weight / (config.k + (rank as f32 + 1.0));
            *scores.entry(c.chunk_id.to_string()).or_insert(0.0) += contribution;
            let entry = by_chunk.entry(c.chunk_id.to_string()).or_insert_with(|| {
                let mut m = c.clone();
                m.source = if sources.len() > 1 {
                    RetrievalSource::Hybrid
                } else {
                    c.source
                };
                m
            });
            record_stage_score(entry, source.name, c.score);
        }
    }

    let mut out: Vec<RetrievalCandidate> = by_chunk.into_values().collect();
    for c in &mut out {
        c.score = scores.get(c.chunk_id.as_str()).copied().unwrap_or(0.0);
    }
    sort_by_score(&mut out);
    out
}

fn record_stage_score(target: &mut RetrievalCandidate, stage: &str, score: f32) {
    match stage {
        "bm25" | "lexical" | "fulltext" => target.bm25_score = Some(score),
        "vector" | "semantic" | "knn" => target.vector_score = Some(score),
        _ => {}
    }
}

/// Descending score with a deterministic chunk-id tie-break.
///
/// The candidate map is a `HashMap`, so iteration order is arbitrary; without
/// the tie-break a fused list would not be reproducible across runs, which
/// would make the RFC-062 replay and evaluation gates flaky (P-011
/// Explainable Decision).
fn sort_by_score(list: &mut [RetrievalCandidate]) {
    list.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk_id.as_str().cmp(b.chunk_id.as_str()))
    });
}

/// Fuses two ranked candidate lists with RRF.
///
/// Retained as the compatibility wrapper for [`rrf_fuse_weighted`]: the BM25
/// list keeps its raw score in `bm25_score` and the vector list in
/// `vector_score`, exactly as before the generalisation.
pub fn rrf_fuse(
    bm25: &[RetrievalCandidate],
    vector: &[RetrievalCandidate],
    config: &RrfConfig,
) -> Vec<RetrievalCandidate> {
    rrf_fuse_weighted(
        &[
            RankedSource::new("bm25", 1.0, bm25),
            RankedSource::new("vector", 1.0, vector),
        ],
        config,
    )
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
            workspace_id: None,
            language: None,
            highlight: None,
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

    #[test]
    fn wrapper_matches_neutral_weighted_three_way_split() {
        // A third empty source must not change the two-list result: the
        // wrapper and an explicit neutral list agree exactly.
        let bm25 = vec![cand("a", 10.0, RetrievalSource::Bm25)];
        let vector = vec![cand("b", 0.9, RetrievalSource::Vector)];
        let config = RrfConfig::default();
        let via_wrapper = rrf_fuse(&bm25, &vector, &config);
        let via_weighted = rrf_fuse_weighted(
            &[
                RankedSource::neutral("bm25", &bm25),
                RankedSource::neutral("vector", &vector),
            ],
            &config,
        );
        let mut via_weighted = via_weighted;
        sort_by_score(&mut via_weighted);
        let mut via_wrapper = via_wrapper;
        sort_by_score(&mut via_wrapper);
        assert_eq!(via_wrapper.len(), via_weighted.len());
        for (x, y) in via_wrapper.iter().zip(via_weighted.iter()) {
            assert_eq!(x.chunk_id, y.chunk_id);
            assert!((x.score - y.score).abs() < 1e-9);
        }
    }

    #[test]
    fn higher_source_weight_lifts_a_candidate() {
        let low = vec![cand("shared", 1.0, RetrievalSource::Bm25)];
        let high = vec![cand("only_high", 1.0, RetrievalSource::Bm25)];
        let config = RrfConfig::default();
        // With equal weights, "shared" (rank 1 in list A) and "only_high"
        // (rank 1 in list B) tie. Weighting list B doubles its contribution
        // and breaks the tie in its favour.
        let fused = rrf_fuse_weighted(
            &[
                RankedSource::new("a", 1.0, &low),
                RankedSource::new("b", 2.0, &high),
            ],
            &config,
        );
        assert_eq!(fused[0].chunk_id.as_str(), "only_high");
        let expected = 2.0 / 61.0;
        assert!((fused[0].score - expected).abs() < 1e-6);
        assert!((fused[1].score - 1.0 / 61.0).abs() < 1e-6);
    }

    #[test]
    fn zero_weight_removes_a_source_from_the_ranking() {
        let kept = vec![cand("kept", 5.0, RetrievalSource::Bm25)];
        let ignored = vec![cand("ignored", 9.0, RetrievalSource::Vector)];
        let fused = rrf_fuse_weighted(
            &[
                RankedSource::new("bm25", 1.0, &kept),
                RankedSource::new("vector", 0.0, &ignored),
            ],
            &RrfConfig::default(),
        );
        assert_eq!(fused.len(), 2, "a zero-weight source is not deleted");
        assert_eq!(fused[0].chunk_id.as_str(), "kept");
        assert_eq!(fused[1].chunk_id.as_str(), "ignored");
        assert_eq!(fused[1].score, 0.0);
    }

    #[test]
    fn consensus_across_three_trusted_sources_beats_a_single_rank() {
        // RFC-066: an item confirmed by multiple independent sources must
        // outrank an item that is rank-1 in exactly one list.
        let local = vec![
            cand("x", 9.0, RetrievalSource::Bm25),
            cand("y", 8.0, RetrievalSource::Bm25),
        ];
        let web = vec![cand("y", 9.0, RetrievalSource::Bm25)];
        let memory = vec![cand("y", 9.0, RetrievalSource::Bm25)];
        let fused = rrf_fuse_weighted(
            &[
                RankedSource::new("local", 1.0, &local),
                RankedSource::new("web", 1.0, &web),
                RankedSource::new("memory", 1.0, &memory),
            ],
            &RrfConfig::default(),
        );
        assert_eq!(fused[0].chunk_id.as_str(), "y");
        // y is rank 1 in web and memory (1/61 each) and rank 2 in local (1/62).
        let expected_y = 2.0 / 61.0 + 1.0 / 62.0;
        assert!((fused[0].score - expected_y).abs() < 1e-6);
        assert_eq!(fused[1].chunk_id.as_str(), "x");
        assert!((fused[1].score - 1.0 / 61.0).abs() < 1e-6);
    }

    #[test]
    fn stage_scores_map_to_their_fields_by_source_name() {
        let bm25 = vec![cand("a", 12.5, RetrievalSource::Bm25)];
        let vector = vec![cand("a", 0.81, RetrievalSource::Vector)];
        let fused = rrf_fuse_weighted(
            &[
                RankedSource::new("bm25", 1.0, &bm25),
                RankedSource::new("vector", 1.0, &vector),
            ],
            &RrfConfig::default(),
        );
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].bm25_score, Some(12.5));
        assert_eq!(fused[0].vector_score, Some(0.81));
    }

    #[test]
    fn per_stage_top_k_bounds_each_list() {
        let many: Vec<RetrievalCandidate> = (0..10)
            .map(|i| cand(&format!("c{i}"), i as f32, RetrievalSource::Bm25))
            .collect();
        let config = RrfConfig {
            k: 60.0,
            per_stage_top_k: 3,
        };
        let fused = rrf_fuse_weighted(&[RankedSource::new("bm25", 1.0, &many)], &config);
        assert_eq!(fused.len(), 3);
    }

    #[test]
    fn fusion_order_is_deterministic_for_equal_scores() {
        let a = vec![cand("zeta", 5.0, RetrievalSource::Bm25)];
        let b = vec![cand("alpha", 5.0, RetrievalSource::Vector)];
        let config = RrfConfig::default();
        let first = rrf_fuse(&a, &b, &config);
        for _ in 0..8 {
            let again = rrf_fuse(&a, &b, &config);
            assert_eq!(
                first
                    .iter()
                    .map(|c| c.chunk_id.as_str())
                    .collect::<Vec<_>>(),
                again
                    .iter()
                    .map(|c| c.chunk_id.as_str())
                    .collect::<Vec<_>>()
            );
        }
        // Both are rank 1 in their own list, so the id tie-break decides.
        assert_eq!(first[0].chunk_id.as_str(), "alpha");
        assert_eq!(first[1].chunk_id.as_str(), "zeta");
    }
}
