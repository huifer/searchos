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
use searchos_domain::decision::{
    CoverageGap, DecisionReason, DecisionTrace, GapPriority, GapResolutionStrategy,
    RequirementStatus, SearchDecision,
};
use searchos_domain::request::{FreshnessRequirement, SearchBudget, SearchMode};
use searchos_domain::response::CoverageStatus;
use searchos_domain::retrieval::RetrievalCandidate;
use searchos_domain::{DocumentId, Money, SearchOsError, TaskId, TraceId};
use searchos_policy::SearchProfile;
use searchos_query::QueryAnalysis;
use serde::{Deserialize, Serialize};

use crate::gap::GapAnalyzer;
use crate::state::{DecisionStage, DecisionState, DecisionStateMachine, StageStatus};

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
    /// Local retrieval for a text query, scoped to the requesting workspace
    /// plus the global `*` namespace (RFC-013 8.7).
    async fn retrieve(
        &self,
        text: &str,
        top_k: usize,
        workspace_id: Option<&str>,
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
    /// Local documents known to be stale (RFC-004 v1.1 10.1: prefer
    /// RefreshExistingSource instead of deleting or re-buying knowledge).
    pub stale_document_ids: Vec<DocumentId>,
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

    /// Coverage gap computation (Rule 4) with structured requirement states
    /// and minimal acquisition strategies (RFC-004 v1.1 6.7/10.1).
    fn coverage_gaps(input: &DecisionInput) -> Vec<CoverageGap> {
        let covered_texts: Vec<String> = input
            .hard_passing()
            .iter()
            .map(|c| {
                format!(
                    "{} {} {}",
                    c.candidate.title.as_deref().unwrap_or(""),
                    c.candidate.url,
                    c.candidate.snippet
                )
            })
            .collect();
        let needs = GapAnalyzer::analyze(
            &input.required_subquestions,
            &covered_texts,
            &input.required_domains,
            Self::query_strict_freshness(input) || Self::profile_strict_freshness(input),
            !input.stale_document_ids.is_empty(),
        );
        needs
            .into_iter()
            .filter(|n| n.is_open())
            .map(|n| CoverageGap {
                question: n.description,
                missing_kind: if n.status == RequirementStatus::PartiallyCovered {
                    "partial_evidence".into()
                } else {
                    "evidence".into()
                },
                status: n.status,
                priority: n.priority,
                strategy: n.strategy,
                requirement_id: n.id,
            })
            .collect()
    }

    /// Strategy label attached to a decision trace (RFC-004 v1.1 8.2).
    fn strategy_for(decision: &SearchDecision, mode: SearchMode) -> &'static str {
        match decision {
            SearchDecision::LocalOnly { .. } => "local_resolve",
            SearchDecision::LocalWithRefresh { .. } => "local_then_refresh",
            SearchDecision::Hybrid { .. } if mode == SearchMode::ExternalFirst => {
                "external_then_local"
            }
            SearchDecision::Hybrid { .. } => "local_and_external_parallel",
            SearchDecision::ExternalOnly { reason } => {
                if *reason == DecisionReason::ExternalFirstRequested {
                    "external_then_local"
                } else {
                    "external_only"
                }
            }
            SearchDecision::Reject { .. } => "reject",
        }
    }

    /// Walks the canonical state machine, marking unused stages `skipped`,
    /// and returns the immutable stage records (RFC-004 v1.1 7/11).
    ///
    /// Every transition is legal by construction; the state machine still
    /// validates each jump so a future edit cannot silently add an illegal
    /// one.
    fn trace_stages(
        decision: &SearchDecision,
        mode: SearchMode,
    ) -> Vec<searchos_domain::decision::DecisionStageTrace> {
        let mut m = DecisionStateMachine::new();
        let local_used = mode != SearchMode::ExternalOnly;
        let external_used = decision.requires_external();
        let budget_rejected = matches!(
            decision,
            SearchDecision::Reject {
                reason: DecisionReason::BudgetExceeded,
                ..
            }
        );
        let step = |m: &mut DecisionStateMachine,
                    to: DecisionState,
                    stage: DecisionStage,
                    used: bool,
                    status: StageStatus,
                    output: &str,
                    reasons: Vec<String>| {
            // A leader panic must not be manufactured by an `.expect` on a
            // state-machine jump; record the illegal transition as skipped.
            if m.transition(to).is_err() {
                m.record(
                    stage,
                    StageStatus::Skipped,
                    0,
                    stage.as_str(),
                    "illegal_transition",
                    vec!["illegal_transition".into()],
                );
                return;
            }
            m.record(
                stage,
                if used { status } else { StageStatus::Skipped },
                0,
                stage.as_str(),
                output,
                reasons,
            );
        };
        step(
            &mut m,
            DecisionState::Validating,
            DecisionStage::Validation,
            true,
            StageStatus::Completed,
            "request validated",
            vec![],
        );
        step(
            &mut m,
            DecisionState::PolicyResolved,
            DecisionStage::PolicyResolution,
            true,
            StageStatus::Completed,
            "profile resolved",
            vec![],
        );
        step(
            &mut m,
            DecisionState::QueryUnderstood,
            DecisionStage::QueryUnderstanding,
            true,
            StageStatus::Completed,
            "intent classified",
            vec![],
        );
        step(
            &mut m,
            DecisionState::LocalRetrieving,
            DecisionStage::LocalRetrieval,
            local_used,
            StageStatus::Completed,
            "local candidates retrieved",
            vec![],
        );
        step(
            &mut m,
            DecisionState::LocalEvaluating,
            DecisionStage::EvidenceEvaluation,
            local_used,
            StageStatus::Completed,
            "evidence evaluated",
            vec![],
        );
        step(
            &mut m,
            DecisionState::QualityChecking,
            DecisionStage::QualityGate,
            local_used,
            if external_used {
                StageStatus::Partial
            } else {
                StageStatus::Completed
            },
            "quality gate",
            vec![decision.reason().code().into()],
        );
        step(
            &mut m,
            DecisionState::GapAnalyzing,
            DecisionStage::GapAnalysis,
            external_used,
            StageStatus::Completed,
            "missing requirements identified",
            vec![],
        );
        step(
            &mut m,
            DecisionState::PlanningExternal,
            DecisionStage::ExternalPlanning,
            external_used,
            if budget_rejected {
                StageStatus::Failed
            } else {
                StageStatus::Completed
            },
            "targeted queries generated",
            if budget_rejected {
                vec!["budget_exceeded".into()]
            } else {
                vec![]
            },
        );
        if budget_rejected {
            let _ = m.transition(DecisionState::BudgetExceeded);
            return m.into_stages();
        }
        step(
            &mut m,
            DecisionState::SelectingProvider,
            DecisionStage::ProviderSelection,
            external_used,
            StageStatus::Completed,
            "provider selected",
            vec![],
        );
        step(
            &mut m,
            DecisionState::ExecutingExternal,
            DecisionStage::ExternalExecution,
            external_used,
            StageStatus::Completed,
            "external acquisition",
            vec![],
        );
        step(
            &mut m,
            DecisionState::MergingEvidence,
            DecisionStage::EvidenceMerge,
            external_used,
            StageStatus::Completed,
            "evidence merged",
            vec![],
        );
        step(
            &mut m,
            DecisionState::FinalQualityChecking,
            DecisionStage::FinalQualityGate,
            external_used,
            StageStatus::Completed,
            "final quality gate",
            vec![],
        );
        step(
            &mut m,
            DecisionState::PersistingKnowledge,
            DecisionStage::Persistence,
            external_used,
            StageStatus::Completed,
            "knowledge persisted",
            vec![],
        );
        let _ = m.transition(DecisionState::Completed);
        m.into_stages()
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

        // ExternalOnly: never intercepted by local cache, and no local
        // retrieval by default (RFC-004 v1.1 9.5).
        if input.mode == SearchMode::ExternalOnly {
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
        // ExternalFirst: external evidence is primary, local knowledge is a
        // supplement in the merge stage (RFC-004 v1.1 9.4). The pipeline
        // performs the local retrieval + merge using the original mode.
        if input.mode == SearchMode::ExternalFirst {
            let decision = SearchDecision::ExternalOnly {
                reason: DecisionReason::ExternalFirstRequested,
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
                gaps.push(
                    CoverageGap::new(input.analysis.normalized.clone(), "fresh_evidence")
                        .with_strategy(GapResolutionStrategy::RefreshExistingSource)
                        .with_priority(GapPriority::High),
                );
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

        // Rule 3 / RFC-004 v1.1 10.1: when the corpus contains stale
        // documents, prefer RefreshExistingSource (re-fetch the same URL)
        // over buying a fresh full external search. Strict-freshness queries
        // were already escalated above and never take this path.
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

        // Refresh over re-buying (RFC-004 v1.1 10.1).
        if !input.stale_document_ids.is_empty()
            && !Self::query_strict_freshness(&input)
            && !Self::profile_strict_freshness(&input)
        {
            let decision = SearchDecision::LocalWithRefresh {
                documents: input.stale_document_ids.clone(),
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
                    missing_requirements: vec![CoverageGap::new(
                        input.analysis.normalized.clone(),
                        "fresh_evidence",
                    )],
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

        // Nothing usable locally (Rule 3 / fail): external acquisition.
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
    /// Coverage is a status, never the numeric count of sub-questions.
    fn trace_coverage_status(input: &DecisionInput) -> CoverageStatus {
        if input.local_candidates.is_empty() {
            CoverageStatus::Insufficient
        } else if input.hard_passing().len() >= input.required_subquestions.len().max(1) {
            CoverageStatus::Sufficient
        } else {
            CoverageStatus::Partial
        }
    }

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
            coverage_status: Self::trace_coverage_status(input).as_str().into(),
            source_status: if Self::explicit_source(input) {
                "explicit".into()
            } else {
                "open".into()
            },
            selected_path: decision.path_name().into(),
            rejection_reasons: match decision {
                SearchDecision::Reject { reason } => vec![reason.code().into()],
                _ => Vec::new(),
            },
            estimated_cost: estimated_cost.clone(),
            actual_cost: None,
            decision_at: chrono::Utc::now(),
            stages: Self::trace_stages(decision, input.mode),
            final_strategy: Some(Self::strategy_for(decision, input.mode).into()),
            final_outcome: Some(match decision {
                SearchDecision::Reject { reason } => {
                    if *reason == DecisionReason::BudgetExceeded {
                        "budget_exceeded".into()
                    } else {
                        "failed".into()
                    }
                }
                SearchDecision::Hybrid { .. } => "partial_then_external".into(),
                _ => "completed".into(),
            }),
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
                workspace_id: None,
                language: None,
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
            stale_document_ids: vec![],
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
        assert!(
            t.rejection_reasons.is_empty(),
            "successful decisions carry no rejection reasons"
        );
        assert_eq!(t.selected_path, "external");
    }

    /// Part 30 §30.11: coverage_status is an enum wire name, not a number.
    #[test]
    fn coverage_status_is_a_status_not_a_number() {
        let inp = input(
            "what is tokio async runtime",
            "general",
            SearchMode::LocalOnly,
            vec![candidate("https://tokio.rs", "Tokio", true)],
        );
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (_, t) = rt.block_on(engine().decide(inp)).unwrap();
        assert!(matches!(
            t.coverage_status.as_str(),
            "sufficient" | "partial" | "insufficient"
        ));
        assert!(t.coverage_status.parse::<usize>().is_err());
    }

    /// Part 30 §30.11: rejection_reasons only on Reject.
    #[test]
    fn successful_decision_has_no_rejection_reasons() {
        let inp = input(
            "what is tokio async runtime",
            "general",
            SearchMode::LocalOnly,
            vec![candidate("https://tokio.rs", "Tokio", true)],
        );
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (decision, trace) = rt.block_on(engine().decide(inp)).unwrap();
        assert!(matches!(decision, SearchDecision::LocalOnly { .. }));
        assert!(trace.rejection_reasons.is_empty());
        let mut rejected = input("x", "general", SearchMode::LocalFirst, vec![]);
        rejected.estimated_external_cost_micros = 500_000;
        let (_, reject_trace) = rt.block_on(engine().decide(rejected)).unwrap();
        assert_eq!(reject_trace.rejection_reasons, vec!["budget_exceeded"]);
    }

    #[test]
    fn normalize_still_exported_for_callers() {
        assert!(!normalize_query("A  B").is_empty());
    }

    #[tokio::test]
    async fn external_first_is_distinct_from_external_only() {
        // RFC-004 v1.1 9.4: external evidence primary, local supplements.
        let inp = input(
            "What is Tokio?",
            "general",
            SearchMode::ExternalFirst,
            vec![candidate("https://tokio.rs", "Tokio", true)],
        );
        let (d, t) = engine().decide(inp).await.unwrap();
        assert!(matches!(
            d,
            SearchDecision::ExternalOnly {
                reason: DecisionReason::ExternalFirstRequested,
                ..
            }
        ));
        assert_eq!(t.final_strategy.as_deref(), Some("external_then_local"));
    }

    #[tokio::test]
    async fn stale_documents_trigger_refresh_not_deletion() {
        // RFC-004 v1.1 10.1: RefreshExistingSource is preferred.
        let mut inp = input(
            "what is raft consensus algorithm",
            "general",
            SearchMode::LocalFirst,
            vec![candidate("https://raft.github.io", "Raft", false)],
        );
        inp.stale_document_ids = vec![inp.local_candidates[0].candidate.document_id.clone()];
        let (d, _) = engine().decide(inp).await.unwrap();
        assert!(
            matches!(d, SearchDecision::LocalWithRefresh { .. }),
            "expected LocalWithRefresh, got {d:?}"
        );
    }

    #[tokio::test]
    async fn trace_contains_structured_stages() {
        let inp = input(
            "What is Tokio?",
            "general",
            SearchMode::ExternalOnly,
            vec![],
        );
        let (_, t) = engine().decide(inp).await.unwrap();
        assert!(!t.stages.is_empty(), "every decision records stage traces");
        assert_eq!(t.stages[0].stage, "validation");
        assert_eq!(t.final_strategy.as_deref(), Some("external_only"));
        assert!(t
            .stages
            .iter()
            .any(|s| s.stage == "local_retrieval" && s.status == "skipped"));
    }

    #[tokio::test]
    async fn hybrid_gaps_carry_status_priority_and_strategy() {
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
                assert!(missing_requirements
                    .iter()
                    .all(|g| !g.requirement_id.is_empty()));
                assert!(missing_requirements
                    .iter()
                    .any(|g| g.question.to_lowercase().contains("axum")));
            }
            other => panic!("expected hybrid, got {other:?}"),
        }
    }
}
