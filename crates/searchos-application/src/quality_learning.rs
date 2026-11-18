//! RFC-080 result-level quality learning.
//!
//! The v1 cost-adaptive router scored a source by how reliably it ANSWERED
//! (success rate) and how much its own metadata could be trusted. That is a
//! statement about the SOURCE, not about the RESULTS it produced. This module
//! closes that gap: a source that always answers but always answers with
//! irrelevant results must not keep winning on price.
//!
//! The signal is deliberately conservative:
//! * only JUDGED results count (a result nobody labelled carries no signal);
//! * an unjudged source is UNKNOWN, never bad — the planner ignores the
//!   result-level score until a minimum number of judged samples exists;
//! * the score is an exponential moving average, so one bad query cannot
//!   erase a track record;
//! * relevance comes from human judgement (RFC-033 feedback) and/or an
//!   evaluated dataset (RFC-015) — never from a model's opinion of itself.

use std::collections::BTreeMap;
use std::sync::Arc;

use searchos_federated::InMemorySourceRegistry;
use serde::{Deserialize, Serialize};

use crate::feedback::{aggregation_key, FeedbackQrels};

/// A relevance judge: given a query and a URL, what is the graded relevance?
///
/// `None` means "nobody judged this" — NEVER "irrelevant". A judge that
/// cannot answer must not push a source's quality down.
pub trait RelevanceJudge: Send + Sync {
    fn grade(&self, query: &str, url: &str) -> Option<u8>;
}

/// An in-memory qrel index: normalized query → canonical URL → grade.
///
/// Built from human feedback (RFC-033) and/or an evaluated dataset
/// (RFC-015). Lookup is deterministic and O(1) per candidate.
#[derive(Debug, Clone, Default)]
pub struct QrelIndex {
    entries: BTreeMap<String, BTreeMap<String, u8>>,
}

impl QrelIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one judgement. A higher grade wins, so a canonical answer is not
    /// downgraded by a later "related" vote for the same URL.
    pub fn insert(&mut self, query: &str, url: &str, grade: u8) {
        let (query, url) = aggregation_key(query, url);
        let bucket = self.entries.entry(query).or_default();
        let entry = bucket.entry(url).or_insert(grade);
        if grade > *entry {
            *entry = grade;
        }
    }

    /// Adds every accepted qrel of an aggregate.
    pub fn extend_from_qrels(&mut self, qrels: &FeedbackQrels) {
        for qrel in &qrels.accepted {
            self.insert(&qrel.query, &qrel.url, qrel.grade);
        }
    }

    /// Adds every graded judgement of an evaluation dataset.
    pub fn extend_from_dataset(
        &mut self,
        dataset: &searchos_evaluation::dataset::EvaluationDataset,
    ) {
        for question in &dataset.questions {
            for grade in &question.relevance_grades {
                self.insert(&question.question, &grade.url, grade.grade);
            }
            for url in &question.relevant_urls {
                self.insert(&question.question, url, 2);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.values().map(|m| m.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl RelevanceJudge for QrelIndex {
    fn grade(&self, query: &str, url: &str) -> Option<u8> {
        let (query, url) = aggregation_key(query, url);
        self.entries.get(&query)?.get(&url).copied()
    }
}

/// Durable storage for learned source quality. Implemented over SQLite so
/// the learning survives a restart (a router that forgets every day cannot
/// learn anything).
#[async_trait::async_trait]
pub trait SourceQualityStore: Send + Sync {
    async fn load_all(&self) -> Result<Vec<(String, f32, u32)>, String>;
    async fn save(&self, source_id: &str, quality: f32, samples: u32) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityLearningConfig {
    /// Whether result-level learning runs at all.
    pub enabled: bool,
    /// The grade at which a judged result counts as relevant (0..=3).
    pub min_relevance_grade: u8,
    /// Optional evaluated dataset (JSONL) used as an additional qrel source.
    pub qrels_path: Option<String>,
}

impl Default for QualityLearningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_relevance_grade: 2,
            qrels_path: None,
        }
    }
}

impl QualityLearningConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.min_relevance_grade > 3 {
            return Err("quality_learning.min_relevance_grade must be 0..=3".into());
        }
        Ok(())
    }
}

/// Learns per-source result quality and keeps the router's picture of it.
pub struct QualityLearner {
    judge: Arc<dyn RelevanceJudge>,
    registry: Arc<InMemorySourceRegistry>,
    store: Option<Arc<dyn SourceQualityStore>>,
    min_relevance_grade: u8,
}

impl QualityLearner {
    pub fn new(
        judge: Arc<dyn RelevanceJudge>,
        registry: Arc<InMemorySourceRegistry>,
        store: Option<Arc<dyn SourceQualityStore>>,
        min_relevance_grade: u8,
    ) -> Self {
        Self {
            judge,
            registry,
            store,
            min_relevance_grade,
        }
    }

    /// Loads the persisted scores into the live registry.
    pub async fn seed(&self) -> Result<usize, String> {
        let Some(store) = self.store.as_ref() else {
            return Ok(0);
        };
        let rows = store.load_all().await?;
        let count = rows.len();
        for (source_id, quality, samples) in rows {
            self.registry.seed_result_quality(
                &searchos_domain::ids::SourceId::new(source_id),
                quality,
                samples,
            );
        }
        Ok(count)
    }

    /// Learns from one search: the URLs a source contributed for a query.
    ///
    /// Returns `None` when nothing could be judged — an unjudged result is
    /// not a bad result, so the source's score stays exactly as it was.
    pub async fn learn(&self, source_id: &str, query: &str, urls: &[String]) -> Option<f64> {
        let mut judged = 0usize;
        let mut relevant = 0usize;
        for url in urls {
            if let Some(grade) = self.judge.grade(query, url) {
                judged += 1;
                if grade >= self.min_relevance_grade {
                    relevant += 1;
                }
            }
        }
        if judged == 0 {
            return None;
        }
        let ratio = relevant as f64 / judged as f64;
        let id = searchos_domain::ids::SourceId::new(source_id);
        self.registry.record_result_quality(&id, ratio as f32);
        if let Some(store) = self.store.as_ref() {
            let (quality, samples) = self
                .registry
                .learned_quality(&id)
                .unwrap_or((ratio as f32, 1));
            if let Err(err) = store.save(source_id, quality, samples).await {
                tracing::warn!(%err, source_id, "could not persist learned source quality");
            }
        }
        Some(ratio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{FeedbackEvent, FeedbackVerdict};
    use chrono::Utc;

    fn event(query: &str, url: &str, verdict: FeedbackVerdict) -> FeedbackEvent {
        FeedbackEvent {
            id: "fb".into(),
            created_at: Utc::now(),
            workspace_id: "ws".into(),
            task_id: None,
            query: query.into(),
            url: url.into(),
            verdict,
            note: None,
        }
    }

    #[test]
    fn judge_answers_only_for_judged_pairs() {
        let events = vec![
            event(
                "What is BM25?",
                "https://a.test/bm25",
                FeedbackVerdict::Canonical,
            ),
            event(
                "What is BM25?",
                "https://a.test/bm25",
                FeedbackVerdict::Canonical,
            ),
            event(
                "What is BM25?",
                "https://a.test/junk",
                FeedbackVerdict::Wrong,
            ),
            event(
                "What is BM25?",
                "https://a.test/junk",
                FeedbackVerdict::Wrong,
            ),
        ];
        let qrels = crate::feedback::aggregate_feedback(&events, "ws", 2);
        let mut index = QrelIndex::new();
        index.extend_from_qrels(&qrels);

        assert_eq!(
            index.grade("what is bm25?", "https://A.TEST/bm25/"),
            Some(3)
        );
        assert_eq!(index.grade("What is BM25?", "https://a.test/junk"), Some(0));
        assert_eq!(
            index.grade("what is bm25?", "https://a.test/never-judged"),
            None,
            "an unjudged URL is UNKNOWN, never irrelevant"
        );
        assert_eq!(index.grade("some other query", "https://a.test/bm25"), None);
    }

    #[test]
    fn dataset_qrels_are_also_usable() {
        let dataset = searchos_evaluation::dataset::EvaluationDataset {
            dataset_id: "d".into(),
            created_at: Utc::now().to_rfc3339(),
            questions: vec![searchos_evaluation::dataset::EvalQuestion {
                id: "q1".into(),
                question: "bm25 ranking".into(),
                category: searchos_evaluation::dataset::DatasetCategory::MixedZhEn,
                expected_keywords: Vec::new(),
                time_sensitive: false,
                profile: None,
                relevant_urls: vec!["https://a.test/bm25".into()],
                relevance_grades: vec![searchos_evaluation::dataset::RelevanceGrade {
                    url: "https://a.test/bm25".into(),
                    grade: 3,
                }],
            }],
        };
        let mut index = QrelIndex::new();
        index.extend_from_dataset(&dataset);
        assert_eq!(index.grade("bm25 ranking", "https://a.test/bm25"), Some(3));
    }

    #[test]
    fn unjudged_source_keeps_its_score_and_judged_samples_move_it() {
        use searchos_domain::ids::{SourceId, WorkspaceId};
        use searchos_federated::{InMemorySourceRegistry, SearchSourceDescriptor, SourceScope};

        let registry = InMemorySourceRegistry::new();
        // Register a minimal descriptor so the registry has an entry to update.
        struct Stub(SearchSourceDescriptor);
        #[async_trait::async_trait]
        impl searchos_federated::SearchSource for Stub {
            fn descriptor(&self) -> SearchSourceDescriptor {
                self.0.clone()
            }
            async fn search(
                &self,
                _r: searchos_federated::SourceSearchRequest,
                _c: searchos_federated::SourceExecutionContext,
            ) -> Result<searchos_federated::SourceSearchResponse, searchos_federated::SourceError>
            {
                Ok(searchos_federated::SourceSearchResponse::success(
                    "s",
                    vec![],
                    0,
                ))
            }
        }
        let descriptor = SearchSourceDescriptor::new(
            SourceId::new("s"),
            "s",
            searchos_federated::SourceCategory::WebSearch,
            SourceScope::Global,
        );
        registry
            .register(std::sync::Arc::new(Stub(descriptor)))
            .unwrap();

        let id = SourceId::new("s");
        let ws = WorkspaceId::new("*");
        registry.seed_result_quality(&id, 0.0, 0);
        assert_eq!(
            registry
                .descriptor_for(&id, &ws)
                .unwrap()
                .reliability
                .result_samples,
            0
        );

        registry.record_result_quality(&id, 1.0);
        registry.record_result_quality(&id, 1.0);
        let profile = registry.descriptor_for(&id, &ws).unwrap().reliability;
        assert_eq!(profile.result_samples, 2);
        assert!((profile.result_quality - 1.0).abs() < 1e-6);

        // A poor sample pulls the EMA down but cannot erase the history.
        registry.record_result_quality(&id, 0.0);
        let profile = registry.descriptor_for(&id, &ws).unwrap().reliability;
        assert_eq!(profile.result_samples, 3);
        assert!(profile.result_quality < 1.0 && profile.result_quality > 0.5);
    }

    #[test]
    fn planner_ignores_result_quality_until_the_sample_floor() {
        use searchos_federated::planner::{expected_quality, MIN_RESULT_QUALITY_SAMPLES};
        use searchos_federated::{SearchSourceDescriptor, SourceCategory, SourceScope};

        let mut d = SearchSourceDescriptor::new(
            searchos_domain::ids::SourceId::new("s"),
            "s",
            SourceCategory::WebSearch,
            SourceScope::Global,
        );
        let baseline = expected_quality(&d);

        // Below the floor: a terrible learned score must NOT change anything.
        d.reliability.result_quality = 0.0;
        d.reliability.result_samples = MIN_RESULT_QUALITY_SAMPLES - 1;
        assert!((expected_quality(&d) - baseline).abs() < 1e-6);

        // At the floor: perfect results keep the score, poor results halve it.
        d.reliability.result_samples = MIN_RESULT_QUALITY_SAMPLES;
        d.reliability.result_quality = 1.0;
        assert!((expected_quality(&d) - baseline).abs() < 1e-6);
        d.reliability.result_quality = 0.0;
        assert!(expected_quality(&d) < baseline * 0.6);
    }

    #[test]
    fn config_validation_rejects_an_impossible_grade() {
        let mut cfg = QualityLearningConfig::default();
        assert!(cfg.validate().is_ok());
        cfg.min_relevance_grade = 4;
        assert!(cfg.validate().is_err());
    }
}
