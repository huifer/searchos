//! RFC-033 Online Feedback — turn human judgements into reusable qrels.
//!
//! This is the LEARNING side of the loop: a human says which results actually
//! answered a query, the system keeps every judgement, and an aggregate only
//! becomes a relevance label when it is reproducible (enough agreement, no
//! unresolved conflict). Nothing here silently rewrites a search result,
//! retrains a model, or invents a label.
//!
//! Load-bearing rules:
//! * **Append-only** — feedback is a human observation, never edited. A later
//!   judgement is another row; the audit trail keeps both.
//! * **Fail-closed aggregation** — a disputed `(query, url)` pair is NOT a
//!   qrel. Averaging contradictory judgements into "grade 1.5" would be a
//!   fabricated label, exactly the failure mode this RFC exists to prevent.
//! * **Deterministic** — verdicts map to fixed grades; ties and thin samples
//!   are reported, not guessed away.
//! * **Workspace-scoped** — feedback never crosses a workspace boundary, and
//!   notes are never sent to an external provider.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use searchos_domain::SearchOsError;
use serde::{Deserialize, Serialize};

/// Human judgement on one result. Grades are FIXED so an aggregate can never
/// depend on who typed the feedback or when it arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackVerdict {
    /// The canonical / authoritative answer.
    Canonical,
    /// Relevant, answers the question.
    Helpful,
    /// Related but does not answer the question.
    Related,
    /// Not relevant to the query.
    Irrelevant,
    /// Actively wrong or misleading.
    Wrong,
}

impl FeedbackVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::Helpful => "helpful",
            Self::Related => "related",
            Self::Irrelevant => "irrelevant",
            Self::Wrong => "wrong",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "canonical" => Ok(Self::Canonical),
            "helpful" => Ok(Self::Helpful),
            "related" => Ok(Self::Related),
            "irrelevant" => Ok(Self::Irrelevant),
            "wrong" => Ok(Self::Wrong),
            other => Err(format!(
                "unknown feedback verdict {other}; use canonical|helpful|related|irrelevant|wrong"
            )),
        }
    }

    /// Graded relevance (RFC-015 15.6): 0 irrelevant, 1 related, 2 relevant,
    /// 3 highly relevant. This is the ONLY source of a grade from feedback.
    pub fn grade(&self) -> u8 {
        match self {
            Self::Canonical => 3,
            Self::Helpful => 2,
            Self::Related => 1,
            Self::Irrelevant | Self::Wrong => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackEvent {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub workspace_id: String,
    /// The search task whose results are being judged, when known.
    pub task_id: Option<String>,
    pub query: String,
    pub url: String,
    pub verdict: FeedbackVerdict,
    /// Optional free-text note. Local only — never exported to a provider.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewFeedback {
    pub workspace_id: String,
    pub task_id: Option<String>,
    pub query: String,
    pub url: String,
    pub verdict: FeedbackVerdict,
    pub note: Option<String>,
}

impl NewFeedback {
    pub fn validate(&self) -> Result<(), SearchOsError> {
        if self.query.trim().is_empty() {
            return Err(SearchOsError::validation("query", "must not be empty"));
        }
        if self.url.trim().is_empty() {
            return Err(SearchOsError::validation("url", "must not be empty"));
        }
        if self.workspace_id.trim().is_empty() {
            return Err(SearchOsError::validation(
                "workspace_id",
                "must not be empty",
            ));
        }
        Ok(())
    }
}

/// Append-only feedback store. Implemented by the runtime over SQLite.
#[async_trait]
pub trait FeedbackStore: Send + Sync {
    async fn append(&self, event: &FeedbackEvent) -> Result<(), String>;
    async fn list(&self, workspace_id: &str, limit: u32) -> Result<Vec<FeedbackEvent>, String>;
}

/// The aggregation key. Query text is normalized deterministically; URLs go
/// through the SAME canonicalizer the evidence merge uses (Part 30 INV-30-15),
/// so `https://x/` and `https://X` do not split one judgement in two.
pub fn aggregation_key(query: &str, url: &str) -> (String, String) {
    let normalized_query = query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (normalized_query, crate::merge::canonical_url(url))
}

/// Why a `(query, url)` pair did NOT become a qrel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// Two or more judgements disagree by >= 2 grades.
    Disputed,
    /// No verdict reached `min_agreement`, or the top verdicts tied.
    InsufficientAgreement,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptedQrel {
    pub query: String,
    pub url: String,
    pub grade: u8,
    pub agreement: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExcludedPair {
    pub query: String,
    pub url: String,
    pub reason: ExclusionReason,
    /// All observed grades, in submission order (audit trail).
    pub grades: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackQrels {
    pub workspace_id: String,
    pub min_agreement: usize,
    pub accepted: Vec<AcceptedQrel>,
    pub excluded: Vec<ExcludedPair>,
}

impl FeedbackQrels {
    /// Qrels grouped by query, ready to become an evaluation dataset.
    pub fn dataset(&self) -> searchos_evaluation::dataset::EvaluationDataset {
        let mut by_query: BTreeMap<String, Vec<searchos_evaluation::dataset::RelevanceGrade>> =
            BTreeMap::new();
        for qrel in &self.accepted {
            by_query.entry(qrel.query.clone()).or_default().push(
                searchos_evaluation::dataset::RelevanceGrade {
                    url: qrel.url.clone(),
                    grade: qrel.grade,
                },
            );
        }
        let questions = by_query
            .into_iter()
            .enumerate()
            .map(|(idx, (question, mut grades))| {
                grades.sort_by(|a, b| a.url.cmp(&b.url));
                grades.dedup();
                let relevant_urls = grades
                    .iter()
                    .filter(|g| g.grade >= 2)
                    .map(|g| g.url.clone())
                    .collect();
                searchos_evaluation::dataset::EvalQuestion {
                    id: format!("feedback-{}", idx + 1),
                    question,
                    // The RFC-015 schema needs a category; feedback-derived
                    // questions are not part of the 500-question category
                    // counts, so they are all marked mixed_zh_en and the
                    // dataset id states their provenance.
                    category: searchos_evaluation::dataset::DatasetCategory::MixedZhEn,
                    // Keyword coverage is a different signal; never invent
                    // expected keywords from the query text.
                    expected_keywords: Vec::new(),
                    time_sensitive: false,
                    profile: None,
                    relevant_urls,
                    relevance_grades: grades,
                }
            })
            .collect();
        searchos_evaluation::dataset::EvaluationDataset {
            dataset_id: format!("feedback-{}", self.workspace_id),
            created_at: Utc::now().to_rfc3339(),
            questions,
        }
    }
}

/// Pure, reproducible aggregation. No I/O, no clock, no randomness.
pub fn aggregate_feedback(
    events: &[FeedbackEvent],
    workspace_id: &str,
    min_agreement: usize,
) -> FeedbackQrels {
    let min_agreement = min_agreement.max(1);
    let mut groups: BTreeMap<(String, String), Vec<u8>> = BTreeMap::new();
    for event in events {
        if event.workspace_id != workspace_id {
            continue;
        }
        groups
            .entry(aggregation_key(&event.query, &event.url))
            .or_default()
            .push(event.verdict.grade());
    }
    let mut accepted = Vec::new();
    let mut excluded = Vec::new();
    for ((query, url), grades) in groups {
        let min = grades.iter().copied().min().unwrap_or(0);
        let max = grades.iter().copied().max().unwrap_or(0);
        if max.saturating_sub(min) >= 2 {
            excluded.push(ExcludedPair {
                query,
                url,
                reason: ExclusionReason::Disputed,
                grades,
            });
            continue;
        }
        let mut counts: BTreeMap<u8, usize> = BTreeMap::new();
        for grade in &grades {
            *counts.entry(*grade).or_default() += 1;
        }
        let top = counts
            .iter()
            .max_by_key(|(grade, count)| (**count, **grade));
        let (grade, agreement) = match top {
            Some((grade, count)) => (*grade, *count),
            None => (0, 0),
        };
        // A TIE between grades is not an agreement. `max_by_key` breaks ties
        // by grade, so check explicitly that no other grade has the same
        // count at the top.
        let tied = counts.values().filter(|c| **c == agreement).count() > 1;
        if agreement < min_agreement || tied {
            excluded.push(ExcludedPair {
                query,
                url,
                reason: ExclusionReason::InsufficientAgreement,
                grades,
            });
            continue;
        }
        accepted.push(AcceptedQrel {
            query,
            url,
            grade,
            agreement,
        });
    }
    FeedbackQrels {
        workspace_id: workspace_id.to_string(),
        min_agreement,
        accepted,
        excluded,
    }
}

/// The application surface: submit a judgement, read the audit trail, and
/// derive qrels. It never mutates retrieval state.
pub struct FeedbackService {
    store: Arc<dyn FeedbackStore>,
}

impl FeedbackService {
    pub fn new(store: Arc<dyn FeedbackStore>) -> Self {
        Self { store }
    }

    pub async fn submit(&self, input: NewFeedback) -> Result<FeedbackEvent, SearchOsError> {
        input.validate()?;
        let event = FeedbackEvent {
            id: searchos_domain::new_id("fb"),
            created_at: Utc::now(),
            workspace_id: input.workspace_id,
            task_id: input.task_id,
            query: input.query,
            url: input.url,
            verdict: input.verdict,
            note: input.note,
        };
        self.store
            .append(&event)
            .await
            .map_err(SearchOsError::Internal)?;
        Ok(event)
    }

    pub async fn list(
        &self,
        workspace_id: &str,
        limit: u32,
    ) -> Result<Vec<FeedbackEvent>, SearchOsError> {
        self.store
            .list(workspace_id, limit)
            .await
            .map_err(SearchOsError::Internal)
    }

    pub async fn qrels(
        &self,
        workspace_id: &str,
        min_agreement: usize,
    ) -> Result<FeedbackQrels, SearchOsError> {
        let events = self.list(workspace_id, 10_000).await?;
        Ok(aggregate_feedback(&events, workspace_id, min_agreement))
    }

    pub async fn dataset(
        &self,
        workspace_id: &str,
        min_agreement: usize,
    ) -> Result<searchos_evaluation::dataset::EvaluationDataset, SearchOsError> {
        Ok(self.qrels(workspace_id, min_agreement).await?.dataset())
    }
}

/// Distinct workspaces present in a set of events — used by tests and by
/// operators who want to see that isolation actually held.
pub fn workspaces(events: &[FeedbackEvent]) -> BTreeSet<String> {
    events.iter().map(|e| e.workspace_id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(query: &str, url: &str, verdict: FeedbackVerdict, ws: &str) -> FeedbackEvent {
        FeedbackEvent {
            id: format!("fb-{}", query.len()),
            created_at: Utc::now(),
            workspace_id: ws.to_string(),
            task_id: None,
            query: query.into(),
            url: url.into(),
            verdict,
            note: None,
        }
    }

    #[test]
    fn verdicts_map_to_fixed_grades() {
        assert_eq!(FeedbackVerdict::Canonical.grade(), 3);
        assert_eq!(FeedbackVerdict::Helpful.grade(), 2);
        assert_eq!(FeedbackVerdict::Related.grade(), 1);
        assert_eq!(FeedbackVerdict::Irrelevant.grade(), 0);
        assert_eq!(FeedbackVerdict::Wrong.grade(), 0);
    }

    #[test]
    fn agreement_produces_a_graded_qrel() {
        let events = vec![
            event(
                "What is BM25?",
                "https://a.test/bm25",
                FeedbackVerdict::Canonical,
                "ws",
            ),
            event(
                " what is bm25? ",
                "https://A.TEST/bm25/",
                FeedbackVerdict::Canonical,
                "ws",
            ),
        ];
        let q = aggregate_feedback(&events, "ws", 2);
        assert_eq!(
            q.accepted.len(),
            1,
            "query + URL normalization must merge the pair"
        );
        assert_eq!(q.accepted[0].grade, 3);
        assert_eq!(q.accepted[0].agreement, 2);
        assert!(q.excluded.is_empty());
    }

    #[test]
    fn contradictory_judgements_are_not_averaged() {
        let events = vec![
            event("q", "https://a.test/x", FeedbackVerdict::Canonical, "ws"),
            event("q", "https://a.test/x", FeedbackVerdict::Wrong, "ws"),
        ];
        let q = aggregate_feedback(&events, "ws", 1);
        assert!(
            q.accepted.is_empty(),
            "a disputed pair must not become a qrel"
        );
        assert_eq!(q.excluded[0].reason, ExclusionReason::Disputed);
        assert_eq!(q.excluded[0].grades, vec![3, 0]);
    }

    #[test]
    fn a_tie_or_a_thin_sample_is_not_an_agreement() {
        let tie = vec![
            event("q", "https://a.test/x", FeedbackVerdict::Canonical, "ws"),
            event("q", "https://a.test/x", FeedbackVerdict::Helpful, "ws"),
        ];
        let q = aggregate_feedback(&tie, "ws", 1);
        // grade difference is 1, but the two grades tie at one vote each.
        assert!(q.accepted.is_empty());
        assert_eq!(q.excluded[0].reason, ExclusionReason::InsufficientAgreement);

        let thin = vec![event(
            "q",
            "https://a.test/x",
            FeedbackVerdict::Helpful,
            "ws",
        )];
        let q = aggregate_feedback(&thin, "ws", 2);
        assert!(q.accepted.is_empty());
        assert_eq!(q.excluded[0].reason, ExclusionReason::InsufficientAgreement);
    }

    #[test]
    fn feedback_never_leaks_across_workspaces() {
        let events = vec![
            event("q", "https://a.test/x", FeedbackVerdict::Helpful, "ws_a"),
            event("q", "https://a.test/x", FeedbackVerdict::Wrong, "ws_b"),
        ];
        let q = aggregate_feedback(&events, "ws_a", 1);
        assert_eq!(
            q.accepted.len(),
            1,
            "ws_b's contradictory vote must not veto ws_a"
        );
        assert_eq!(q.accepted[0].grade, 2);
        assert_eq!(workspaces(&events).len(), 2);
    }

    #[test]
    fn dataset_exposes_graded_qrels_not_fabricated_keywords() {
        let events = vec![
            event("q", "https://a.test/x", FeedbackVerdict::Canonical, "ws"),
            event("q", "https://a.test/x", FeedbackVerdict::Canonical, "ws"),
            event("q", "https://a.test/y", FeedbackVerdict::Related, "ws"),
            event("q", "https://a.test/y", FeedbackVerdict::Related, "ws"),
        ];
        let ds = aggregate_feedback(&events, "ws", 2).dataset();
        assert_eq!(ds.questions.len(), 1);
        assert_eq!(ds.questions[0].relevance_grades.len(), 2);
        assert!(ds.questions[0].expected_keywords.is_empty());
        assert_eq!(
            ds.questions[0].relevant_urls,
            vec!["https://a.test/x".to_string()],
            "only grade >= 2 counts as relevant for MRR/Recall"
        );
    }

    #[test]
    fn validation_rejects_empty_input() {
        let bad = NewFeedback {
            workspace_id: "ws".into(),
            task_id: None,
            query: "   ".into(),
            url: "https://a.test".into(),
            verdict: FeedbackVerdict::Helpful,
            note: None,
        };
        assert!(bad.validate().is_err());
    }
}
