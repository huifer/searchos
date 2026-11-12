//! Ranking-quality metrics (RFC-015 15.2 extension).
//!
//! Keyword coverage alone cannot tell whether the right chunk ranked FIRST.
//! These metrics grade the returned URL order against relevance judgments
//! (qrels) attached to each dataset question:
//!
//! - [`mrr`] — Mean Reciprocal Rank of the first relevant URL.
//! - [`ndcg_at_k`] — normalized discounted cumulative gain (binary gains).
//! - [`recall_at_k`] — fraction of relevant URLs present in the top k.
//! - [`precision_at_k`] — fraction of the top k that is relevant.
//!
//! All take the RANKED URL list and the relevant URL set; k bounds the cut.

/// Rank of the first relevant URL (1-based); `None` when none appear.
pub fn first_relevant_rank(ranked: &[String], relevant: &[String]) -> Option<usize> {
    ranked
        .iter()
        .position(|u| relevant.contains(u))
        .map(|i| i + 1)
}

/// Mean Reciprocal Rank over a question set.
pub fn mrr(ranked_per_question: &[&[String]], relevant_per_question: &[&[String]]) -> f64 {
    let n = ranked_per_question.len();
    if n == 0 {
        return 0.0;
    }
    let sum: f64 = ranked_per_question
        .iter()
        .zip(relevant_per_question)
        .map(|(ranked, relevant)| {
            first_relevant_rank(ranked, relevant)
                .map(|r| 1.0 / r as f64)
                .unwrap_or(0.0)
        })
        .sum();
    sum / n as f64
}

/// Binary-gain DCG@k.
fn dcg(ranked: &[String], relevant: &[String], k: usize) -> f64 {
    ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, u)| {
            let gain = if relevant.contains(u) { 1.0 } else { 0.0 };
            gain / (i as f64 + 2.0).log2()
        })
        .sum()
}

/// Graded DCG@k: gain is `2^grade - 1` so a canonical page (grade 3, gain 7)
/// outweighs a merely-related page (grade 1, gain 1).
pub fn graded_dcg_at_k(
    ranked: &[String],
    grades: &[crate::dataset::RelevanceGrade],
    k: usize,
) -> f64 {
    ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, u)| {
            let gain = grades
                .iter()
                .find(|g| &g.url == u)
                .map(|g| g.gain())
                .unwrap_or(0.0);
            gain / (i as f64 + 2.0).log2()
        })
        .sum()
}

/// Graded nDCG@k: 1.0 only when documents are ordered by descending grade.
pub fn graded_ndcg_at_k(
    ranked: &[String],
    grades: &[crate::dataset::RelevanceGrade],
    k: usize,
) -> f64 {
    let mut sorted: Vec<crate::dataset::RelevanceGrade> = grades.to_vec();
    sorted.sort_by_key(|g| std::cmp::Reverse(g.grade));
    // Ideal ordering is the graded docs themselves (their URLs), in order.
    let ideal: Vec<String> = sorted.iter().map(|g| g.url.clone()).collect();
    let idcg = graded_dcg_at_k(&ideal, grades, k);
    if idcg <= 0.0 {
        return 0.0;
    }
    graded_dcg_at_k(ranked, grades, k) / idcg
}

/// URLs that count as a CORRECT answer (grade >= 2), for MRR/Recall on graded
/// qrels. Grade 1 is "related but not an answer": it still contributes gain to
/// nDCG, but must not inflate recall or let MRR reward landing on a merely
/// related page instead of the canonical one.
pub fn graded_relevant_urls(grades: &[crate::dataset::RelevanceGrade]) -> Vec<String> {
    grades
        .iter()
        .filter(|g| g.grade >= 2)
        .map(|g| g.url.clone())
        .collect()
}

/// Normalized DCG@k (1.0 = perfect ordering of all relevant URLs).
pub fn ndcg_at_k(ranked: &[String], relevant: &[String], k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let ideal_count = relevant.len().min(k);
    let ideal: Vec<String> = relevant[..ideal_count].to_vec();
    let idcg = dcg(&ideal, relevant, k);
    if idcg <= 0.0 {
        return 0.0;
    }
    dcg(ranked, relevant, k) / idcg
}

/// Recall@k: fraction of relevant URLs present within the top k.
pub fn recall_at_k(ranked: &[String], relevant: &[String], k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|u| relevant.contains(u))
        .count();
    hits as f64 / relevant.len() as f64
}

/// Precision@k: fraction of the top k that is relevant.
pub fn precision_at_k(ranked: &[String], relevant: &[String], k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|u| relevant.contains(u))
        .count();
    hits as f64 / k as f64
}

/// Aggregated ranking metrics over a question set.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RankingReport {
    /// Questions that carried relevance judgments.
    pub questions_with_qrels: usize,
    pub mrr: f64,
    pub ndcg_at_k: f64,
    pub recall_at_k: f64,
    pub precision_at_k: f64,
}

/// The evaluation cut for ranking metrics.
pub const RANKING_K: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn first_relevant_rank_is_one_based() {
        let ranked = urls(&["x", "rel1", "rel2"]);
        assert_eq!(first_relevant_rank(&ranked, &urls(&["rel1"])), Some(2));
        assert_eq!(first_relevant_rank(&ranked, &urls(&["missing"])), None);
    }

    #[test]
    fn mrr_averages_reciprocal_ranks() {
        let r1 = urls(&["a", "rel"]);
        let r2 = urls(&["rel", "b"]);
        let r3 = urls(&["x", "y"]);
        let rel = urls(&["rel"]);
        let m = mrr(&[&r1, &r2, &r3], &[&rel, &rel, &rel]);
        assert!((m - (0.5 + 1.0 + 0.0) / 3.0).abs() < 1e-9);
    }

    #[test]
    fn perfect_ordering_scores_one() {
        let ranked = urls(&["rel1", "rel2", "x"]);
        let relevant = urls(&["rel1", "rel2"]);
        assert!((ndcg_at_k(&ranked, &relevant, 5) - 1.0).abs() < 1e-9);
        assert!((recall_at_k(&ranked, &relevant, 5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn wrong_order_scores_below_one() {
        let ranked = urls(&["x", "rel2", "rel1"]);
        let relevant = urls(&["rel1", "rel2"]);
        let n = ndcg_at_k(&ranked, &relevant, 5);
        assert!(n > 0.0 && n < 1.0, "nDCG {n}");
    }

    #[test]
    fn k_bounds_the_cut() {
        let ranked = urls(&["x", "y", "rel"]);
        let relevant = urls(&["rel"]);
        assert_eq!(recall_at_k(&ranked, &relevant, 2), 0.0);
        assert_eq!(recall_at_k(&ranked, &relevant, 3), 1.0);
        assert_eq!(precision_at_k(&ranked, &relevant, 3), 1.0 / 3.0);
    }

    #[test]
    fn empty_inputs_are_zero_not_nan() {
        assert_eq!(mrr(&[], &[]), 0.0);
        assert_eq!(ndcg_at_k(&[], &[], 5), 0.0);
        assert_eq!(recall_at_k(&urls(&["a"]), &[], 5), 0.0);
    }

    #[test]
    fn graded_gain_distinguishes_canonical_from_related() {
        use crate::dataset::RelevanceGrade;
        let grades = vec![
            RelevanceGrade {
                url: "https://canonical".into(),
                grade: 3,
            },
            RelevanceGrade {
                url: "https://related".into(),
                grade: 1,
            },
        ];
        let perfect = urls(&["https://canonical", "https://related", "x"]);
        let swapped = urls(&["https://related", "https://canonical", "x"]);
        assert!((graded_ndcg_at_k(&perfect, &grades, 5) - 1.0).abs() < 1e-9);
        let worse = graded_ndcg_at_k(&swapped, &grades, 5);
        assert!(
            worse < 0.75,
            "putting a related page first must score lower: {worse}"
        );
        // Binary nDCG cannot tell these two orderings apart: both have one
        // relevant doc at rank 1 and one at rank 2 with equal gain.
        let binary = urls(&["https://canonical", "https://related"]);
        let binary_perfect = ndcg_at_k(&perfect, &binary, 5);
        let binary_swapped = ndcg_at_k(&swapped, &binary, 5);
        assert!((binary_perfect - binary_swapped).abs() < 1e-9);
    }

    #[test]
    fn grade_zero_is_not_relevant() {
        use crate::dataset::RelevanceGrade;
        let grades = vec![
            RelevanceGrade {
                url: "https://a".into(),
                grade: 0,
            },
            RelevanceGrade {
                url: "https://b".into(),
                grade: 2,
            },
        ];
        assert_eq!(graded_relevant_urls(&grades), urls(&["https://b"]));
        assert!(
            graded_relevant_urls(&[RelevanceGrade {
                url: "https://c".into(),
                grade: 1,
            }])
            .is_empty(),
            "grade 1 is related, not a correct answer"
        );
        assert_eq!(graded_ndcg_at_k(&urls(&["https://a"]), &grades, 5), 0.0);
    }
}
