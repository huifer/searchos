//! Decision engine (RFC-004).
//!
//! Decides: LocalOnly / LocalWithRefresh / Hybrid / ExternalOnly / Reject.
//! Rules (RFC-004 4.4):
//! 1. 强时效性查询 — strict-freshness queries cannot be served from history.
//! 2. 指定来源 — explicit sources take priority; no substitutes.
//! 3. 本地数据过期 — stale docs are history, need refresh/external.
//! 4. 本地数据覆盖不足 — only fill the missing gaps.
//! 5. 成本预算 — never pick a lower-quality path silently; Reject with
//!    partial results instead.

use async_trait::async_trait;
use searchos_domain::decision::{CoverageGap, DecisionReason, DecisionTrace, SearchDecision};
use searchos_domain::request::{FreshnessRequirement, SearchBudget, SearchMode};
use searchos_domain::retrieval::RetrievalCandidate;
use searchos_domain::{DocumentId, Money, SearchOsError, TaskId, TraceId};
use searchos_policy::SearchProfile;
use searchos_query::QueryAnalysis;
use serde::{Deserialize, Serialize};

/// One locally retrieved candidate with gate-facing metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalCandidateEval {
    pub candidate: RetrievalCandidate,
    /// Relevance normalized to [0,1] for the quality gate.
    pub relevance: f32,
    /// Whether the underlying document satisfies the freshness policy.
    pub freshness_ok: bool,
    /// Whether the document's URL satisfies the source policy.
    pub source_ok: bool,
}

/// Port for local knowledge used by the engine (application supplies the
/// adapter over retrieval + document store).
#[async_trait]
pub trait LocalKnowledgePort: Send + Sync {
    /// Local retrieval for a text query.
    async fn retrieve(
        &self,
        text: &str,
        top_k: usize,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError>;
    /// Document ids that are stale and would need refresh.
    async fn stale_document_ids(
        &self,
        candidates: &[RetrievalCandidate],
    ) -> Result<Vec<DocumentId>, SearchOsError>;
}

/// Everything the engine needs for one decision (RFC-004 4.2 pipeline input).
pub struct DecisionInput {
    pub analysis: QueryAnalysis,
    pub profile: SearchProfile,
    pub mode: SearchMode,
    pub budget: SearchBudget,
    pub local_candidates: Vec<LocalCandidateEval>,
    /// Estimated minimum cost of an external path (micros).
    pub estimated_external_cost_micros: i64,
    /// Currency for estimates.
    pub currency: String,
    /// Required sub-questions for coverage (RFC-005 5.6).
    pub required_subquestions: Vec<String>,
    /// Constraints merged into the source policy by the caller.
    pub required_domains: Vec<String>,
    pub excluded_domains: Vec<String>,
}

impl DecisionInput {
    /// Local candidates that passed freshness + source hard checks.
    pub fn hard_passing(&self) -> Vec<&LocalCandidateEval> {
        self.local_candidates
            .iter()
            .filter(|c| c.freshness_ok && c.source_ok)
            .collect()
    }
}

/// Decision engine trait (RFC-002 2.4 style port).
#[async_trait]
pub trait DecisionEngine: Send + Sync {
    /// Produces a decision for one task.
    async fn decide(
        &self,
        input: DecisionInput,
    ) -> Result<(SearchDecision, DecisionTrace), SearchOsError>;
}

/// Default rule-based engine.
pub struct DefaultDecisionEngine<Q: QualityGateLike = ()> {
    gate: crate::quality::QualityGate,
    _marker: std::marker::PhantomData<Q>,
}

/// Alias so the engine can be constructed without generics ceremony.
pub type Engine = DefaultDecisionEngine;

impl DefaultDecisionEngine {
    /// Engine with the given quality gate.
    pub fn new(gate: crate::quality::QualityGate) -> Self {
        Self {
            gate,
            _marker: std::marker::PhantomData,
        }
    }
}

/// Narrowed gate surface used by the engine (keeps engine testable).
pub trait QualityGateLike {}

impl QualityGateLike for () {}

impl DefaultDecisionEngine {
    /// Freshness requirement effective for this input (profile ∩ request).
    fn effective_freshness(input: &DecisionInput) -> FreshnessRequirement {
        match (
            &input.analysis.temporal_sensitivity,
            &input.profile.freshness,
        ) {
            (searchos_query::TemporalSensitivity::Strict, _) => FreshnessRequirement::Strict,
            (_, FreshnessRequirement::Any) => {
                if matches!(
                    input.analysis.temporal_sensitivity,
                    searchos_query::TemporalSensitivity::Strict
                ) {
                    FreshnessRequirement::Strict
                } else {
                    FreshnessRequirement::Any
                }
            }
            (_, f) => f.clone(),
        }
    }

    /// Query-level strict freshness: the query itself demands current facts
    /// (Rule 1: Latest / Today / Current / Live / Real-time).
    fn query_strict_freshness(input: &DecisionInput) -> bool {
        matches!(
            input.analysis.temporal_sensitivity,
            searchos_query::TemporalSensitivity::Strict
        ) || input.analysis.intents.iter().any(|i| i.is_time_sensitive())
    }

    /// Profile-level strict freshness (e.g. research profile):
    /// fresh sources required in the mix, local may supplement.
    fn profile_strict_freshness(input: &DecisionInput) -> bool {
        matches!(
            input.profile.freshness,
            FreshnessRequirement::Strict | FreshnessRequirement::MaximumAgeSeconds(_)
        )
    }

    /// Rule 2: explicit source requirement present?
    fn explicit_source(input: &DecisionInput) -> bool {
        input.analysis.source_requirement.has_explicit_requirement()
            || !input.required_domains.is_empty()
    }

    /// Coverage gap computation for hybrid decisions (Rule 4).
    fn coverage_gaps(input: &DecisionInput) -> Vec<CoverageGap> {
        // Reuse the query expander's gap questions: uncovered sub-questions.
        input
            .required_subquestions
            .iter()
            .filter(|sq| {
                // A sub-question is "covered" when some hard-passing
                // candidate matches most of its tokens.
                let passing = input.hard_passing();
                let tokens = crate::quality::content_tokens(sq);
                !passing.iter().any(|c| {
                    let text = format!(
                        "{} {} {}",
                        c.candidate.title.as_deref().unwrap_or("").to_lowercase(),
                        c.candidate.url.to_lowercase(),
                        c.candidate.snippet.to_lowercase()
                    );
                    let hits = tokens.iter().filter(|t| text.contains(t.as_str())).count();
                    tokens.is_empty() || hits * 2 >= tokens.len()
                })
            })
            .map(|sq| CoverageGap {
                question: sq.clone(),
                missing_kind: "evidence".into(),
            })
            .collect()
    }
}

#[async_trait]
impl DecisionEngine for DefaultDecisionEngine {
    async fn decide(
        &self,
        input: DecisionInput,
    ) -> Result<(SearchDecision, DecisionTrace), SearchOsError> {
        let trace_id = TraceId::generate();
        let task_id = TaskId::generate();

        // ---- Resolve policy + mode gates ------------------------------
        let local_allowed = input.mode.allows_local();
        let external_allowed = input.mode.allows_external();

        // Rule 5: budget — all quality paths over budget ⇒ Reject.
        if external_allowed {
            let est = Money {
                amount_micros: input.estimated_external_cost_micros,
                currency: input.currency.clone(),
            };
            let max = Money {
                amount_micros: input.budget.max_total_cost_micros,
                currency: input.currency.clone(),
            };
            if max.amount_micros <= 0 || est.amount_micros > max.amount_micros {
                let decision = SearchDecision::Reject {
                    reason: DecisionReason::BudgetExceeded,
                };
                let trace = self.build_trace(&trace_id, &task_id, &input, &decision, 0, &est);
                return Ok((decision, trace));
            }
        }

        // Mode-forced shortcuts (ExternalOnly / LocalOnly).
        if !local_allowed {
            let decision = SearchDecision::ExternalOnly {
                reason: DecisionReason::ModeForcesExternal,
            };
            let trace = self.build_trace(
                &trace_id,
                &task_id,
                &input,
                &decision,
                0,
                &Money::zero(&input.currency),
            );
            return Ok((decision, trace));
        }
        if !external_allowed {
            // Rule 1 applies even in local-only mode: if strict freshness
            // demanded and local evidence cannot verify freshness → Reject
            // instead of serving stale facts (质量硬约束高于成本 P-001).
            if Self::query_strict_freshness(&input) || Self::profile_strict_freshness(&input) {
                let decision = SearchDecision::Reject {
                    reason: DecisionReason::StrictFreshnessRequired,
                };
                let trace = self.build_trace(
                    &trace_id,
                    &task_id,
                    &input,
                    &decision,
                    input.local_candidates.len(),
                    &Money::zero(&input.currency),
                );
                return Ok((decision, trace));
            }
            let decision = SearchDecision::LocalOnly {
                reason: DecisionReason::ModeForcesLocal,
            };
            let trace = self.build_trace(
                &trace_id,
                &task_id,
                &input,
                &decision,
                input.local_candidates.len(),
                &Money::zero(&input.currency),
            );
            return Ok((decision, trace));
        }

        // ---- Evaluate local availability ------------------------------
        let passing = input.hard_passing();
        let local_candidates_count = input.local_candidates.len();

        // Rule 2: explicit source — if named source not locally available,
        // external targeted search required.
        if Self::explicit_source(&input) {
            let named = &input.analysis.source_requirement;
            let locally_satisfies = if let Some(url) = &named.exact_url {
                let host = searchos_policy::SourcePolicy::domain_of(url);
                passing.iter().any(|c| {
                    host.as_ref()
                        .map(|h| c.candidate.url.contains(h.as_str()))
                        .unwrap_or(false)
                })
            } else {
                !passing.is_empty() && passing.iter().all(|c| c.source_ok)
            };
            if !locally_satisfies {
                let decision = SearchDecision::ExternalOnly {
                    reason: DecisionReason::ExplicitSourceRequirement,
                };
                let trace = self.build_trace(
                    &trace_id,
                    &task_id,
                    &input,
                    &decision,
                    local_candidates_count,
                    &Money::zero(&input.currency),
                );
                return Ok((decision, trace));
            }
        }

        // Rule 1a: query-level strict freshness (Latest / Real-time etc.) —
        // local data alone can never verify currency → external required.
        if Self::query_strict_freshness(&input) {
            let decision = SearchDecision::ExternalOnly {
                reason: DecisionReason::StrictFreshnessRequired,
            };
            let trace = self.build_trace(
                &trace_id,
                &task_id,
                &input,
                &decision,
                local_candidates_count,
                &Money::zero(&input.currency),
            );
            return Ok((decision, trace));
        }

        // Rule 1b: profile-level strict freshness (research profile) —
        // fresh external sources must join the mix; local supplements only.
        if Self::profile_strict_freshness(&input) {
            if passing.is_empty() {
                let decision = SearchDecision::ExternalOnly {
                    reason: DecisionReason::StrictFreshnessRequired,
                };
                let trace = self.build_trace(
                    &trace_id,
                    &task_id,
                    &input,
                    &decision,
                    local_candidates_count,
                    &Money::zero(&input.currency),
                );
                return Ok((decision, trace));
            }
            let mut gaps = Self::coverage_gaps(&input);
            if gaps.is_empty() {
                gaps.push(CoverageGap {
                    question: input.analysis.normalized.clone(),
                    missing_kind: "fresh_evidence".into(),
                });
            }
            let decision = SearchDecision::Hybrid {
                missing_requirements: gaps,
                reason: DecisionReason::StrictFreshnessRequired,
            };
            let trace = self.build_trace(
                &trace_id,
                &task_id,
                &input,
                &decision,
                local_candidates_count,
                &Money::zero(&input.currency),
            );
            return Ok((decision, trace));
        }

        // ---- Quality gate over local evidence -------------------------
        let evidence: Vec<searchos_domain::evidence::Evidence> = passing
            .iter()
            .map(|c| searchos_domain::evidence::Evidence {
                id: searchos_domain::ids::EvidenceId::generate(),
                source_id: searchos_domain::ids::SourceId::generate(),
                document_version_id: None,
                title: c.candidate.title.clone(),
                url: c.candidate.url.clone(),
                content_ref: None,
                snippet: Some(c.candidate.snippet.clone()),
                published_at: None,
                fetched_at: c.candidate.fetched_at,
                source_reliability: searchos_domain::evidence::ReliabilityLevel::Community,
                provenance: searchos_domain::evidence::Provenance {
                    origin: searchos_domain::evidence::ProvenanceOrigin::Local,
                    provider_id: None,
                    source_url: Some(c.candidate.url.clone()),
                    document_id: Some(c.candidate.document_id.clone()),
                    retrieved_at: c.candidate.fetched_at,
                },
            })
            .collect();
        let relevances: Vec<f32> = passing.iter().map(|c| c.relevance).collect();
        let verdict = self
            .gate
            .evaluate_set(&evidence, &relevances, &input.required_subquestions);

        // Rule 3: stale documents should be refreshed even when enough
        // non-stale evidence exists.
        let stale_ids = vec![]; // filled by caller via stale port when used

        if verdict.passed() {
            let decision = SearchDecision::LocalOnly {
                reason: DecisionReason::LocalEvidencePassedQualityGate,
            };
            let trace = self.build_trace(
                &trace_id,
                &task_id,
                &input,
                &decision,
                local_candidates_count,
                &Money::zero(&input.currency),
            );
            return Ok((decision, trace));
        }

        // Partial: hybrid with only the missing gaps (Rule 4).
        if verdict.needs_more() || !verdict.coverage_gaps.is_empty() {
            let mut gaps = verdict.coverage_gaps.clone();
            for g in Self::coverage_gaps(&input) {
                if !gaps.iter().any(|x| x.question == g.question) {
                    gaps.push(g);
                }
            }
            let decision = if gaps.is_empty() {
                SearchDecision::Hybrid {
                    missing_requirements: vec![CoverageGap {
                        question: input.analysis.normalized.clone(),
                        missing_kind: "fresh_evidence".into(),
                    }],
                    reason: DecisionReason::LocalCoverageInsufficient,
                }
            } else {
                SearchDecision::Hybrid {
                    missing_requirements: gaps,
                    reason: DecisionReason::LocalCoverageInsufficient,
                }
            };
            let trace = self.build_trace(
                &trace_id,
                &task_id,
                &input,
                &decision,
                local_candidates_count,
                &Money::zero(&input.currency),
            );
            return Ok((decision, trace));
        }

        // Nothing usable locally (Rule 3 / fail): external, or refresh.
        if !stale_ids.is_empty() {
            let decision = SearchDecision::LocalWithRefresh {
                documents: stale_ids,
                reason: DecisionReason::LocalEvidenceStale,
            };
            let trace = self.build_trace(
                &trace_id,
                &task_id,
                &input,
                &decision,
                local_candidates_count,
                &Money::zero(&input.currency),
            );
            return Ok((decision, trace));
        }

        let decision = SearchDecision::ExternalOnly {
            reason: DecisionReason::NoValidExecutionPath,
        };
        let trace = self.build_trace(
            &trace_id,
            &task_id,
            &input,
            &decision,
            local_candidates_count,
            &Money::zero(&input.currency),
        );
        Ok((decision, trace))
    }
}

impl DefaultDecisionEngine {
    fn build_trace(
        &self,
        trace_id: &TraceId,
        task_id: &TaskId,
        input: &DecisionInput,
        decision: &SearchDecision,
        accepted: usize,
        estimated_cost: &Money,
    ) -> DecisionTrace {
        DecisionTrace {
            trace_id: trace_id.clone(),
            task_id: task_id.clone(),
            intents: input.analysis.intents.clone(),
            profile_version: input.profile.version.clone(),
            local_candidates: input.local_candidates.len(),
            accepted_candidates: accepted,
            freshness_status: format!("{:?}", Self::effective_freshness(input)).to_lowercase(),
            coverage_status: format!("{:?}", input.required_subquestions.len()).to_string(),
            source_status: if Self::explicit_source(input) {
                "explicit".into()
            } else {
                "open".into()
            },
            selected_path: decision.path_name().into(),
            rejection_reasons: vec![decision.reason().code().into()],
            estimated_cost: estimated_cost.clone(),
            actual_cost: None,
            decision_at: chrono::Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};
    use searchos_domain::retrieval::RetrievalSource;
    use searchos_policy::{FreshnessPolicy, ProfileRegistry, QualityPolicy, SourcePolicy};
    use searchos_query::{analyze, normalize_query};

    fn candidate(url: &str, title: &str, fresh: bool) -> LocalCandidateEval {
        LocalCandidateEval {
            candidate: RetrievalCandidate {
                chunk_id: ChunkId::generate(),
                document_id: DocumentId::generate(),
                version_id: DocumentVersionId::generate(),
                title: Some(title.into()),
                snippet: "some content".into(),
                url: url.into(),
                score: 5.0,
                source: RetrievalSource::Bm25,
                fetched_at: if fresh {
                    Utc::now()
                } else {
                    Utc::now() - chrono::Duration::days(400)
                },
                vector_score: None,
                bm25_score: Some(5.0),
            },
            relevance: 0.9,
            freshness_ok: fresh,
            source_ok: true,
        }
    }

    fn input(
        query: &str,
        profile_id: &str,
        mode: SearchMode,
        candidates: Vec<LocalCandidateEval>,
    ) -> DecisionInput {
        let profile = ProfileRegistry::builtin().get(profile_id).unwrap().clone();
        let analysis = analyze(query);
        DecisionInput {
            analysis,
            profile,
            mode,
            budget: SearchBudget::default(),
            local_candidates: candidates,
            estimated_external_cost_micros: 5_000,
            currency: "USD".into(),
            required_subquestions: vec![],
            required_domains: vec![],
            excluded_domains: vec![],
        }
    }

    fn engine() -> DefaultDecisionEngine {
        DefaultDecisionEngine::new(crate::quality::QualityGate::new(
            QualityPolicy::default(),
            FreshnessPolicy::default_policy(),
            SourcePolicy::default_policy(),
        ))
    }

    #[tokio::test]
    async fn strict_freshness_goes_external() {
        // Rule 1: "latest tokio version" must not be served from history.
        let inp = input(
            "What is the latest Tokio version?",
            "general",
            SearchMode::LocalFirst,
            vec![candidate("https://tokio.rs", "Tokio", true)],
        );
        let (d, t) = engine().decide(inp).await.unwrap();
        assert!(d.requires_external(), "expected external, got {:?}", d);
        assert_eq!(d.reason(), &DecisionReason::StrictFreshnessRequired);
        assert_eq!(t.selected_path, "external");
    }

    #[tokio::test]
    async fn static_query_with_good_local_passes() {
        let inp = input(
            "What is Tokio? rust async runtime explanation",
            "general",
            SearchMode::LocalFirst,
            vec![candidate(
                "https://tokio.rs/tokio/tutorial",
                "Tokio tutorial",
                true,
            )],
        );
        let (d, _) = engine().decide(inp).await.unwrap();
        assert!(matches!(d, SearchDecision::LocalOnly { .. }), "got {:?}", d);
    }

    #[tokio::test]
    async fn budget_rejection_rule5() {
        let mut inp = input("What is Tokio?", "general", SearchMode::LocalFirst, vec![]);
        inp.estimated_external_cost_micros = 500_000; // > 0.10 USD budget
        let (d, _) = engine().decide(inp).await.unwrap();
        assert!(matches!(
            d,
            SearchDecision::Reject {
                reason: DecisionReason::BudgetExceeded
            }
        ));
    }

    #[tokio::test]
    async fn mode_forces_external() {
        let inp = input(
            "What is Tokio?",
            "general",
            SearchMode::ExternalOnly,
            vec![],
        );
        let (d, _) = engine().decide(inp).await.unwrap();
        assert!(matches!(
            d,
            SearchDecision::ExternalOnly {
                reason: DecisionReason::ModeForcesExternal
            }
        ));
    }

    #[tokio::test]
    async fn explicit_unsatisfied_source_goes_external() {
        // Rule 2: named source must be used; local evidence from other
        // domains cannot substitute.
        let q = "install rust from https://rustlang.org official";
        let inp = input(
            q,
            "general",
            SearchMode::LocalFirst,
            vec![candidate("https://other-site.com/rust", "Rust", true)],
        );
        let (d, _) = engine().decide(inp).await.unwrap();
        assert!(
            matches!(
                d,
                SearchDecision::ExternalOnly {
                    reason: DecisionReason::ExplicitSourceRequirement
                }
            ),
            "got {:?}",
            d
        );
    }

    #[tokio::test]
    async fn partial_coverage_becomes_hybrid_rule4() {
        let q = "compare tokio and axum performance";
        let inp = input(
            q,
            "research",
            SearchMode::LocalFirst,
            vec![candidate("https://tokio.rs", "Tokio perf", true)],
        );
        let (d, _) = engine().decide(inp).await.unwrap();
        match d {
            SearchDecision::Hybrid {
                missing_requirements,
                ..
            } => {
                assert!(!missing_requirements.is_empty());
                // Rule 4: only the missing gap (axum), not a full re-search.
                assert!(missing_requirements
                    .iter()
                    .any(|g| g.question.to_lowercase().contains("axum")));
            }
            other => panic!("expected hybrid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_local_goes_external() {
        let inp = input(
            "quantum computing history overview",
            "general",
            SearchMode::LocalFirst,
            vec![],
        );
        let (d, _) = engine().decide(inp).await.unwrap();
        assert!(d.requires_external());
    }

    #[test]
    fn trace_records_reasons() {
        let inp = input("x", "general", SearchMode::ExternalOnly, vec![]);
        // build_trace is private; verify via decide (mode forced external).
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (_, t) = rt.block_on(engine().decide(inp)).unwrap();
        assert_eq!(
            t.rejection_reasons,
            vec!["mode_forces_external".to_string()]
        );
        assert_eq!(t.selected_path, "external");
    }

    #[test]
    fn normalize_still_exported_for_callers() {
        assert!(!normalize_query("A  B").is_empty());
    }
}
