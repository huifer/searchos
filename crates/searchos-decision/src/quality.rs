//! Evidence evaluation & quality gate (RFC-005).
//!
//! Hard constraints first (freshness, required domain, provenance, coverage);
//! hard constraints cannot be overridden by the weighted score (RFC-005 5.4).

use searchos_domain::evidence::Evidence;
use searchos_domain::response::{CoverageStatus, FreshnessStatus, QualityStatus};
use searchos_domain::CoverageGap;
use searchos_policy::{FreshnessPolicy, QualityPolicy, SourcePolicy};
use serde::{Deserialize, Serialize};

/// Evaluation of one evidence item against hard constraints + score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceEvaluation {
    pub evidence_id: String,
    pub accepted: bool,
    /// Hard-constraint failures (freshness_failed / required_domain_missing /
    /// provenance_missing / excluded_domain).
    pub failed_constraints: Vec<String>,
    pub relevance: f32,
    pub freshness: f32,
    pub coverage: f32,
    pub source: f32,
    pub provenance: f32,
    /// Weighted Q score (only meaningful when accepted).
    pub score: f32,
}

/// Full quality verdict for an evidence set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityVerdict {
    pub status: QualityStatus,
    pub freshness: FreshnessStatus,
    pub coverage: CoverageStatus,
    pub score: f32,
    pub failed_constraints: Vec<String>,
    pub accepted_indices: Vec<usize>,
    pub evaluations: Vec<EvidenceEvaluation>,
    pub coverage_gaps: Vec<CoverageGap>,
    /// Whether unresolved contradictions were detected (RFC-005 5.7).
    pub conflicts: Vec<String>,
}

impl QualityVerdict {
    /// Whether the set passed all hard constraints.
    pub fn passed(&self) -> bool {
        self.status == QualityStatus::Passed
    }

    /// Whether a hybrid/external follow-up is required.
    pub fn needs_more(&self) -> bool {
        matches!(self.status, QualityStatus::Partial | QualityStatus::Failed)
    }
}

/// The quality gate.
pub struct QualityGate {
    pub policy: QualityPolicy,
    pub freshness: FreshnessPolicy,
    pub source: SourcePolicy,
}

impl QualityGate {
    /// Gate with explicit policies.
    pub fn new(policy: QualityPolicy, freshness: FreshnessPolicy, source: SourcePolicy) -> Self {
        Self {
            policy,
            freshness,
            source,
        }
    }

    /// Evaluates a full evidence set (RFC-005 5.4 order: hard checks, then
    /// scoring, then coverage/status aggregation).
    pub fn evaluate_set(
        &self,
        evidence: &[Evidence],
        relevance_scores: &[f32],
        required_subquestions: &[String],
    ) -> QualityVerdict {
        let mut evaluations = Vec::new();
        let mut all_failed: Vec<String> = Vec::new();
        let mut accepted_indices = Vec::new();
        let mut any_fresh = false;
        let mut all_stale = !evidence.is_empty();

        for (i, ev) in evidence.iter().enumerate() {
            let eval = self.evaluate_one(ev, relevance_scores.get(i).copied().unwrap_or(0.5));
            if !eval.failed_constraints.is_empty() {
                for f in &eval.failed_constraints {
                    if !all_failed.contains(f) {
                        all_failed.push(f.clone());
                    }
                }
            } else {
                accepted_indices.push(i);
            }
            let is_fresh = !eval
                .failed_constraints
                .iter()
                .any(|f| f == "freshness_failed");
            any_fresh |= is_fresh;
            all_stale &= !is_fresh;
            evaluations.push(eval);
        }

        // Coverage over required sub-questions (RFC-005 5.6).
        let (coverage_ratio, gaps) =
            self.coverage(evidence, &accepted_indices, required_subquestions);

        let freshness_status = if evidence.is_empty() {
            FreshnessStatus::Unknown
        } else if all_stale {
            FreshnessStatus::Stale
        } else if any_fresh {
            FreshnessStatus::Verified
        } else {
            FreshnessStatus::Unknown
        };

        let coverage_status = if coverage_ratio >= self.policy.minimum_coverage {
            CoverageStatus::Sufficient
        } else if coverage_ratio > 0.0 {
            CoverageStatus::Partial
        } else {
            CoverageStatus::Insufficient
        };

        // Source diversity (RFC-005 5.3 Diversity — task dependent).
        let distinct_sources: std::collections::HashSet<&str> = accepted_indices
            .iter()
            .map(|&i| host_of(&evidence[i].url).unwrap_or_else(|| evidence[i].url.as_str()))
            .collect();
        let diversity_failed = distinct_sources.len() < self.policy.minimum_distinct_sources;
        if diversity_failed
            && !accepted_indices.is_empty()
            && self.policy.minimum_distinct_sources > 1
        {
            all_failed.push("diversity_below_minimum".into());
        }

        // Conflicts (RFC-005 5.7): two+ equal-reliability official sources on
        // the same sub-question with diverging content hashes → unresolved.
        let conflicts = detect_conflicts(evidence, &accepted_indices);

        // Aggregate score over accepted evidence (raw relevance-based).
        let avg = |f: &dyn Fn(&EvidenceEvaluation) -> f32| -> f32 {
            if accepted_indices.is_empty() {
                0.0
            } else {
                accepted_indices
                    .iter()
                    .map(|&i| f(&evaluations[i]))
                    .sum::<f32>()
                    / accepted_indices.len() as f32
            }
        };
        let q = self.policy.weights.score(
            avg(&|e| e.relevance),
            freshness_status_score(freshness_status),
            coverage_ratio,
            avg(&|e| e.source),
            avg(&|e| e.provenance),
        );

        let status = if !accepted_indices.is_empty()
            && !diversity_failed
            && conflicts.is_empty()
            && coverage_status == CoverageStatus::Sufficient
            && freshness_status != FreshnessStatus::Stale
            && q >= self.policy.minimum_quality
        {
            QualityStatus::Passed
        } else if conflicts.is_empty()
            && (coverage_status == CoverageStatus::Partial
                || (!accepted_indices.is_empty() && q < self.policy.minimum_quality))
        {
            QualityStatus::Partial
        } else if !conflicts.is_empty() {
            QualityStatus::Conflict
        } else if evidence.is_empty() {
            QualityStatus::Unknown
        } else {
            QualityStatus::Failed
        };

        QualityVerdict {
            status,
            freshness: freshness_status,
            coverage: coverage_status,
            score: q,
            failed_constraints: all_failed,
            accepted_indices,
            evaluations,
            coverage_gaps: gaps,
            conflicts,
        }
    }

    /// Evaluates one evidence item: hard constraints then weighted score.
    pub fn evaluate_one(&self, ev: &Evidence, relevance: f32) -> EvidenceEvaluation {
        let mut failed = Vec::new();

        // Hard check 1: freshness (RFC-005 5.5; stale data must not serve as
        // current fact — Rule 3).
        let category = searchos_policy::classify_content(&ev.url, ev.title.as_deref());
        let age = ev.age_seconds();
        if !self.freshness.is_fresh(category, age) {
            failed.push("freshness_failed".into());
        }

        // Hard check 2: source policy (required/excluded domains). The
        // failure reason names the constraint actually violated (INV-30-20).
        match self.source.violation(&ev.url) {
            Some(searchos_policy::SourceViolation::ExcludedDomain) => {
                failed.push("excluded_domain_violation".into())
            }
            Some(searchos_policy::SourceViolation::RequiredDomainMissing) => {
                failed.push("required_domain_missing".into())
            }
            None => {}
        }

        // Hard check 3: provenance present when required (ADR-021).
        if self.policy.require_provenance
            && ev.provenance.source_url.is_none()
            && ev.provenance.document_id.is_none()
        {
            failed.push("provenance_missing".into());
        }

        let freshness_score = if failed.contains(&"freshness_failed".to_string()) {
            0.2
        } else {
            match category {
                searchos_policy::ContentCategory::Realtime => 1.0,
                _ => {
                    let ttl = self.freshness.ttl_seconds(category).unwrap_or(3600) as f32;
                    (1.0 - (age as f32 / ttl).clamp(0.0, 0.8)).max(0.2)
                }
            }
        };
        let source_score = ev.source_reliability.score();
        let provenance_score =
            if ev.provenance.source_url.is_some() || ev.provenance.document_id.is_some() {
                1.0
            } else {
                0.0
            };
        let score = self.policy.weights.score(
            relevance.clamp(0.0, 1.0),
            freshness_score,
            1.0, // per-item coverage is set-level
            source_score,
            provenance_score,
        );

        EvidenceEvaluation {
            evidence_id: ev.id.to_string(),
            accepted: failed.is_empty(),
            failed_constraints: failed,
            relevance: relevance.clamp(0.0, 1.0),
            freshness: freshness_score,
            coverage: 1.0,
            source: source_score,
            provenance: provenance_score,
            score,
        }
    }

    /// Coverage ratio + uncovered gaps (RFC-005 5.6).
    fn coverage(
        &self,
        evidence: &[Evidence],
        accepted: &[usize],
        required_subquestions: &[String],
    ) -> (f32, Vec<CoverageGap>) {
        if required_subquestions.is_empty() {
            // Single-question task: covered iff any accepted evidence exists.
            return (if accepted.is_empty() { 0.0 } else { 1.0 }, Vec::new());
        }
        let mut covered = vec![false; required_subquestions.len()];
        let mut gaps = Vec::new();
        for (i, sq) in required_subquestions.iter().enumerate() {
            let tokens = content_tokens(sq);
            let hit = accepted.iter().any(|&ai| {
                let ev = &evidence[ai];
                let text = format!(
                    "{} {} {}",
                    ev.title.as_deref().unwrap_or("").to_lowercase(),
                    ev.url.to_lowercase(),
                    ev.snippet.as_deref().unwrap_or("").to_lowercase(),
                );
                // Sub-question covered when the majority of its content
                // tokens (stopwords stripped) appear in title/URL/snippet.
                let hits = tokens.iter().filter(|t| text.contains(t.as_str())).count();
                tokens.is_empty() || hits * 2 > tokens.len()
            });
            covered[i] = hit;
            if !hit {
                gaps.push(CoverageGap::new(sq.clone(), "evidence"));
            }
        }
        let ratio =
            covered.iter().filter(|c| **c).count() as f32 / required_subquestions.len() as f32;
        (ratio, gaps)
    }
}

/// Content tokens of a sub-question: lowercased, stopwords + punctuation
/// stripped, so "What is raft consensus?" → ["raft", "consensus"].
pub fn content_tokens(q: &str) -> Vec<String> {
    const STOPWORDS: [&str; 12] = [
        "what", "is", "the", "a", "an", "are", "of", "in", "on", "for", "how", "does",
    ];
    q.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty() && !STOPWORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

fn freshness_status_score(s: FreshnessStatus) -> f32 {
    match s {
        FreshnessStatus::Verified => 1.0,
        FreshnessStatus::Unknown => 0.5,
        FreshnessStatus::Stale => 0.1,
    }
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split(['/', '?', '#']).next()
}

/// Detects unresolved contradictions: the same subject/claim covered by
/// official/primary sources with contradictory values (RFC-005 5.7).
/// Two ordinary pages on one host are NOT a conflict (INV-30-20).
fn detect_conflicts(evidence: &[Evidence], accepted: &[usize]) -> Vec<String> {
    use std::collections::HashMap;
    let mut by_claim: HashMap<String, Vec<usize>> = HashMap::new();
    for &i in accepted {
        let ev = &evidence[i];
        if matches!(
            ev.source_reliability,
            searchos_domain::evidence::ReliabilityLevel::Official
                | searchos_domain::evidence::ReliabilityLevel::Primary
        ) {
            by_claim.entry(claim_key(ev)).or_default().push(i);
        }
    }
    let mut conflicts = Vec::new();
    for (claim, idxs) in by_claim {
        if idxs.len() < 2 {
            continue;
        }
        let mut contradiction = false;
        for i in 0..idxs.len() {
            for j in (i + 1)..idxs.len() {
                if contradictory_values(
                    evidence[idxs[i]].snippet.as_deref(),
                    evidence[idxs[j]].snippet.as_deref(),
                ) {
                    contradiction = true;
                }
            }
        }
        if contradiction {
            conflicts.push(format!("conflicting_official_sources:{claim}"));
        }
    }
    conflicts
}

/// A subject/claim key derived from the canonical page path (or title when
/// the URL has no meaningful path segment). Paths are case-sensitive.
fn claim_key(ev: &Evidence) -> String {
    let without_scheme = ev.url.split("://").nth(1).unwrap_or(ev.url.as_str());
    let path = without_scheme.split(['?', '#']).next().unwrap_or("");
    let last = path.split('/').rfind(|s| !s.is_empty()).unwrap_or("");
    if !last.is_empty() && last != "index.html" {
        last.to_string()
    } else {
        ev.title.as_deref().unwrap_or("").to_lowercase()
    }
}

/// Numeric claims are the contradiction signal: same subject/claim but
/// disjoint numeric values (e.g. version 1.20 vs 1.21, $10 vs $12).
fn contradictory_values(a: Option<&str>, b: Option<&str>) -> bool {
    fn numbers(s: &str) -> Vec<String> {
        s.split(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-')
            .filter(|t| t.chars().any(|c| c.is_ascii_digit()) && t.len() <= 20)
            .map(|t| t.to_ascii_lowercase())
            .collect()
    }
    let (a, b) = (a.unwrap_or(""), b.unwrap_or(""));
    let na = numbers(a);
    let nb = numbers(b);
    !na.is_empty() && !nb.is_empty() && na.iter().all(|n| !nb.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::evidence::{Provenance, ProvenanceOrigin, ReliabilityLevel};
    use searchos_domain::ids::{EvidenceId, SourceId};

    fn evidence(
        url: &str,
        title: &str,
        age_seconds: i64,
        rel: ReliabilityLevel,
        published: Option<chrono::DateTime<Utc>>,
    ) -> Evidence {
        Evidence {
            id: EvidenceId::generate(),
            source_id: SourceId::generate(),
            document_version_id: None,
            title: Some(title.into()),
            url: url.into(),
            content_ref: None,
            snippet: Some("content".into()),
            published_at: published,
            fetched_at: Utc::now() - chrono::Duration::seconds(age_seconds),
            source_reliability: rel,
            provenance: Provenance {
                origin: ProvenanceOrigin::Local,
                provider_id: None,
                source_url: Some(url.into()),
                document_id: None,
                retrieved_at: Utc::now(),
            },
        }
    }

    fn gate() -> QualityGate {
        QualityGate::new(
            Default::default(),
            FreshnessPolicy::default_policy(),
            SourcePolicy::default_policy(),
        )
    }

    #[test]
    fn fresh_official_evidence_passes() {
        let g = gate();
        let ev = vec![evidence(
            "https://doc.rust-lang.org/book",
            "The Rust Book",
            3600,
            ReliabilityLevel::Official,
            None,
        )];
        let v = g.evaluate_set(&ev, &[0.9], &[]);
        assert_eq!(v.status, QualityStatus::Passed);
        assert_eq!(v.freshness, FreshnessStatus::Verified);
        assert!(v.coverage_gaps.is_empty());
    }

    #[test]
    fn stale_news_fails_freshness_hard_constraint() {
        let g = gate();
        // News TTL 60min; this is 5 hours old.
        let ev = vec![evidence(
            "https://news.example.com/article",
            "News",
            5 * 3600,
            ReliabilityLevel::Professional,
            None,
        )];
        let v = g.evaluate_set(&ev, &[0.9], &[]);
        assert_ne!(v.status, QualityStatus::Passed);
        assert!(v.failed_constraints.iter().any(|f| f == "freshness_failed"));
        assert_eq!(v.freshness, FreshnessStatus::Stale);
    }

    #[test]
    fn excluded_domain_rejected() {
        let g = QualityGate::new(
            Default::default(),
            FreshnessPolicy::default_policy(),
            SourcePolicy::default_policy().with_constraints(&[], &["pinterest.com".to_string()]),
        );
        let ev = vec![evidence(
            "https://pinterest.com/pin",
            "Pin",
            10,
            ReliabilityLevel::Community,
            None,
        )];
        let v = g.evaluate_set(&ev, &[0.9], &[]);
        assert!(v
            .failed_constraints
            .iter()
            .any(|f| f == "excluded_domain_violation"));
        assert!(!v
            .failed_constraints
            .iter()
            .any(|f| f == "required_domain_missing"));
        assert_eq!(v.status, QualityStatus::Failed);
    }

    /// Part 30 §30.11: an excluded domain must be reported as an exclusion
    /// violation, not as a missing required domain.
    #[test]
    fn excluded_domain_reason_is_correct() {
        let g = QualityGate::new(
            Default::default(),
            FreshnessPolicy::default_policy(),
            SourcePolicy::default_policy().with_constraints(&[], &["blocked.com".to_string()]),
        );
        let ev = evidence(
            "https://blocked.com/x",
            "Blocked",
            10,
            ReliabilityLevel::Official,
            None,
        );
        let eval = g.evaluate_one(&ev, 0.9);
        assert!(eval
            .failed_constraints
            .iter()
            .any(|f| f == "excluded_domain_violation"));
        assert!(!eval
            .failed_constraints
            .iter()
            .any(|f| f == "required_domain_missing"));
    }

    /// Part 30 §30.11: two official pages on one host are not a conflict.
    #[test]
    fn two_pages_on_same_official_host_are_not_a_conflict() {
        let g = gate();
        let ev = vec![
            evidence(
                "https://docs.example.com/page-one",
                "Tokio guide",
                3600,
                ReliabilityLevel::Official,
                None,
            ),
            evidence(
                "https://docs.example.com/page-two",
                "Axum guide",
                3600,
                ReliabilityLevel::Official,
                None,
            ),
        ];
        let v = g.evaluate_set(&ev, &[0.9, 0.9], &[]);
        assert!(v.conflicts.is_empty(), "same host alone is not a conflict");
        assert_eq!(v.status, QualityStatus::Passed);
    }

    #[test]
    fn coverage_gap_only_for_missing_subquestion() {
        let g = gate();
        let ev = vec![evidence(
            "https://tokio.rs/tokio",
            "Tokio features overview",
            100,
            ReliabilityLevel::Official,
            None,
        )];
        let subs = vec!["tokio features".to_string(), "axum features".to_string()];
        let v = g.evaluate_set(&ev, &[0.9], &subs);
        assert_eq!(v.status, QualityStatus::Partial);
        assert_eq!(v.coverage_gaps.len(), 1);
        assert_eq!(v.coverage_gaps[0].question, "axum features");
        assert_eq!(v.coverage, CoverageStatus::Partial);
    }

    #[test]
    fn missing_provenance_rejected_when_required() {
        let mut ev = evidence(
            "https://example.com/a",
            "A",
            10,
            ReliabilityLevel::Community,
            None,
        );
        ev.provenance.source_url = None;
        ev.provenance.document_id = None;
        let g = gate();
        let v = g.evaluate_set(&[ev], &[0.9], &[]);
        assert!(v
            .failed_constraints
            .iter()
            .any(|f| f == "provenance_missing"));
    }
}
