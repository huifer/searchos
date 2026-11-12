//! Dataset runner (RFC-015 15.1 trace → 15.2 metrics).

use std::future::Future;

use crate::dataset::{EvalQuestion, EvaluationDataset};
use serde::{Deserialize, Serialize};

/// Result of one question execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOutcome {
    pub question_id: String,
    pub category: crate::dataset::DatasetCategory,
    /// Quality gate status of the response.
    pub quality_status: String,
    /// Whether the execution was served (fully or partially) locally.
    pub local_resolved: bool,
    /// External calls made for this question.
    pub external_calls: u64,
    /// Estimated cost in micros.
    pub cost_micros: i64,
    /// Keyword coverage ratio in [0,1].
    pub keyword_coverage: f32,
    pub latency_ms: u64,
    /// Reciprocal rank of the first relevant URL (0 when none/qrels absent).
    pub reciprocal_rank: f64,
}

/// Aggregated run outcome.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunOutcome {
    pub dataset_id: String,
    pub total_questions: usize,
    /// Aggregated ranking quality against per-question qrels. Zeros when the
    /// dataset carries no `relevant_urls` (metrics are undefined without
    /// judgments and are never fabricated).
    pub ranking: crate::ranking::RankingReport,
    pub local_resolutions: usize,
    pub quality_pass_count: usize,
    pub quality_pass_rate: f64,
    pub keyword_coverage_avg: f64,
    pub external_call_total: u64,
    pub cost_micros_total: i64,
    pub local_resolution_rate: f64,
    pub per_question: Vec<QuestionOutcome>,
}

/// Per-question input the runner produces for the query function.
pub struct QueryRequest<'a> {
    pub question: &'a EvalQuestion,
}

/// Per-question execution result supplied by the system under test.
pub struct QueryResult {
    pub quality_status: String,
    pub local_resolved: bool,
    pub external_calls: u64,
    pub cost_micros: i64,
    pub latency_ms: u64,
    /// Returned result snippets/URLs/titles joined for keyword matching.
    pub evidence_text: String,
    /// Result URLs in rank order (for ranking metrics against qrels).
    pub ranked_urls: Vec<String>,
}

/// Executes every question through `f` and aggregates the outcome.
pub async fn run_dataset<Fut, F>(
    dataset: &EvaluationDataset,
    mut f: F,
) -> Result<RunOutcome, String>
where
    F: FnMut(QueryRequest<'_>) -> Fut,
    Fut: Future<Output = Result<QueryResult, String>>,
{
    let mut outcome = RunOutcome {
        dataset_id: dataset.dataset_id.clone(),
        ranking: crate::ranking::RankingReport::default(),
        ..Default::default()
    };
    let mut ranking_questions = 0usize;
    let mut ranking_mrr = 0.0f64;
    let mut ranking_ndcg = 0.0f64;
    let mut ranking_recall = 0.0f64;
    let mut ranking_precision = 0.0f64;
    for q in &dataset.questions {
        let result = f(QueryRequest { question: q }).await?;
        let coverage = keyword_coverage(&result.evidence_text, &q.expected_keywords);
        let passed = result.quality_status == "passed";
        if !q.relevant_urls.is_empty() {
            ranking_questions += 1;
            ranking_mrr +=
                crate::ranking::first_relevant_rank(&result.ranked_urls, &q.relevant_urls)
                    .map(|r| 1.0 / r as f64)
                    .unwrap_or(0.0);
            ranking_ndcg += crate::ranking::ndcg_at_k(
                &result.ranked_urls,
                &q.relevant_urls,
                crate::ranking::RANKING_K,
            );
            ranking_recall += crate::ranking::recall_at_k(
                &result.ranked_urls,
                &q.relevant_urls,
                crate::ranking::RANKING_K,
            );
            ranking_precision += crate::ranking::precision_at_k(
                &result.ranked_urls,
                &q.relevant_urls,
                crate::ranking::RANKING_K,
            );
        }
        outcome.total_questions += 1;
        if passed {
            outcome.quality_pass_count += 1;
        }
        outcome.external_call_total += result.external_calls;
        outcome.cost_micros_total += result.cost_micros;
        outcome.keyword_coverage_avg += coverage as f64;
        if result.local_resolved {
            outcome.local_resolutions += 1;
        }
        outcome.per_question.push(QuestionOutcome {
            question_id: q.id.clone(),
            category: q.category,
            quality_status: result.quality_status,
            local_resolved: result.local_resolved,
            external_calls: result.external_calls,
            cost_micros: result.cost_micros,
            keyword_coverage: coverage,
            latency_ms: result.latency_ms,
            reciprocal_rank: crate::ranking::first_relevant_rank(
                &result.ranked_urls,
                &q.relevant_urls,
            )
            .map(|r| 1.0 / r as f64)
            .unwrap_or(0.0),
        });
    }
    if ranking_questions > 0 {
        outcome.ranking = crate::ranking::RankingReport {
            questions_with_qrels: ranking_questions,
            mrr: ranking_mrr / ranking_questions as f64,
            ndcg_at_k: ranking_ndcg / ranking_questions as f64,
            recall_at_k: ranking_recall / ranking_questions as f64,
            precision_at_k: ranking_precision / ranking_questions as f64,
        };
    }
    if outcome.total_questions > 0 {
        outcome.quality_pass_rate =
            outcome.quality_pass_count as f64 / outcome.total_questions as f64;
        outcome.keyword_coverage_avg /= outcome.total_questions as f64;
        outcome.local_resolution_rate =
            outcome.local_resolutions as f64 / outcome.total_questions as f64;
    }
    Ok(outcome)
}

/// Keyword coverage: fraction of expected keywords present in the evidence
/// text (case-insensitive).
pub fn keyword_coverage(evidence_text: &str, keywords: &[String]) -> f32 {
    if keywords.is_empty() {
        return 1.0;
    }
    let lower = evidence_text.to_lowercase();
    keywords
        .iter()
        .filter(|k| lower.contains(&k.to_lowercase()))
        .count() as f32
        / keywords.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::DatasetCategory;

    #[test]
    fn coverage_counts_keywords() {
        let c = keyword_coverage(
            "tokio is an async runtime for rust",
            &["tokio".into(), "runtime".into(), "go".into()],
        );
        assert!((c - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(keyword_coverage("anything", &[]), 1.0);
    }

    #[tokio::test]
    async fn run_aggregates() {
        let ds = EvaluationDataset {
            dataset_id: "t".into(),
            created_at: String::new(),
            questions: vec![
                EvalQuestion {
                    id: "q1".into(),
                    question: "q1".into(),
                    category: DatasetCategory::StaticTechKnowledge,
                    expected_keywords: vec!["tokio".into()],
                    time_sensitive: false,
                    profile: None,
                    relevant_urls: vec![],
                },
                EvalQuestion {
                    id: "q2".into(),
                    question: "q2".into(),
                    category: DatasetCategory::NewsLatest,
                    expected_keywords: vec![],
                    time_sensitive: true,
                    profile: None,
                    relevant_urls: vec![],
                },
            ],
        };
        let mut n = 0;
        let outcome = run_dataset(&ds, |_req| {
            n += 1;
            async move {
                Ok(QueryResult {
                    quality_status: if n == 1 { "passed" } else { "partial" }.into(),
                    local_resolved: n == 1,
                    external_calls: if n == 1 { 0 } else { 1 },
                    cost_micros: if n == 1 { 100 } else { 5000 },
                    latency_ms: 10,
                    evidence_text: "tokio runtime".into(),
                    ranked_urls: vec![],
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(outcome.total_questions, 2);
        assert_eq!(outcome.quality_pass_count, 1);
        assert!((outcome.quality_pass_rate - 0.5).abs() < 1e-9);
        assert_eq!(outcome.external_call_total, 1);
        assert_eq!(outcome.cost_micros_total, 5100);
        assert!((outcome.local_resolution_rate - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn ranking_metrics_are_aggregated_from_qrels() {
        let ds = EvaluationDataset {
            dataset_id: "rank".into(),
            created_at: String::new(),
            questions: vec![
                EvalQuestion {
                    id: "q1".into(),
                    question: "q1".into(),
                    category: DatasetCategory::StaticTechKnowledge,
                    expected_keywords: vec![],
                    time_sensitive: false,
                    profile: None,
                    relevant_urls: vec!["https://a".into()],
                },
                EvalQuestion {
                    id: "q2".into(),
                    question: "q2".into(),
                    category: DatasetCategory::StaticTechKnowledge,
                    expected_keywords: vec![],
                    time_sensitive: false,
                    profile: None,
                    relevant_urls: vec!["https://b".into()],
                },
            ],
        };
        let outcome = run_dataset(&ds, |req| {
            let id = req.question.id.clone();
            async move {
                let ranked_urls = if id == "q1" {
                    vec!["https://a".into(), "https://x".into()]
                } else {
                    vec!["https://x".into(), "https://b".into()]
                };
                Ok(QueryResult {
                    quality_status: "passed".into(),
                    local_resolved: true,
                    external_calls: 0,
                    cost_micros: 0,
                    latency_ms: 1,
                    evidence_text: String::new(),
                    ranked_urls,
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(outcome.ranking.questions_with_qrels, 2);
        // q1 rank 1 (RR=1), q2 rank 2 (RR=0.5).
        assert!((outcome.ranking.mrr - 0.75).abs() < 1e-9);
        assert!((outcome.ranking.recall_at_k - 1.0).abs() < 1e-9);
        assert!(outcome.ranking.ndcg_at_k > 0.0 && outcome.ranking.ndcg_at_k < 1.0);
    }
}
