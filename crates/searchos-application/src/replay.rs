//! Search Replay (RFC-040), Policy Simulation (RFC-075) and Search
//! Intelligence Evaluation (RFC-086).
//!
//! A replay re-executes a recorded request through the REAL pipeline with a
//! fresh request id, OFFLINE (`max_external_queries = 0`), so it can show
//! what today's knowledge + policies would decide without spending provider
//! budget. A policy simulation applies a caller-supplied quality-policy
//! override on an isolated service (all the shared Arc state is reused).
//! Evaluation batches replays over recent traces and aggregates agreement.
//!
//! Replays ARE recorded as new tasks (honest observability), carry a
//! `replay_...` request-id prefix for attribution, and never bill a provider.

use crate::search_service::{SearchDependencies, SearchService as _, SearchServiceImpl};
use searchos_domain::decision::DecisionTrace;
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::response::{QualityStatus, SearchStatus};
use searchos_domain::SearchOsError;
use searchos_policy::QualityPolicy;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The recorded request facts needed to reconstruct a replay (RFC-040).
#[derive(Debug, Clone)]
pub struct RecordedSearchTask {
    pub task_id: String,
    pub trace_id: String,
    pub query: String,
    pub profile_id: String,
    pub mode: String,
    pub workspace_id: String,
    pub status: String,
}

/// Port for recorded-task lookup and recent-trace enumeration.
#[async_trait::async_trait]
pub trait RecordedTaskLookup: Send + Sync {
    async fn by_trace_id(
        &self,
        trace_id: &str,
    ) -> Result<Option<RecordedSearchTask>, SearchOsError>;
    async fn recent_trace_ids(
        &self,
        limit: u32,
        workspace: Option<&str>,
    ) -> Result<Vec<String>, SearchOsError>;
    async fn trace(&self, trace_id: &str) -> Result<Option<DecisionTrace>, SearchOsError>;
}

/// RFC-075: caller-supplied policy override for a what-if simulation.
/// `None` fields keep the deployed policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PolicyOverride {
    #[serde(default)]
    pub minimum_quality: Option<f32>,
    #[serde(default)]
    pub minimum_coverage: Option<f32>,
    #[serde(default)]
    pub minimum_distinct_sources: Option<usize>,
}

impl PolicyOverride {
    pub fn is_empty(&self) -> bool {
        self.minimum_quality.is_none()
            && self.minimum_coverage.is_none()
            && self.minimum_distinct_sources.is_none()
    }

    pub fn apply(&self, policy: &mut QualityPolicy) {
        if let Some(v) = self.minimum_quality {
            policy.minimum_quality = v.clamp(0.0, 1.0);
        }
        if let Some(v) = self.minimum_coverage {
            policy.minimum_coverage = v.clamp(0.0, 1.0);
        }
        if let Some(v) = self.minimum_distinct_sources {
            policy.minimum_distinct_sources = v.min(32);
        }
    }
}

/// One side of a replay comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionSnapshot {
    pub decision_path: String,
    pub decision_reason: String,
    pub final_outcome: Option<String>,
    pub estimated_cost_micros: i64,
    pub quality_status: String,
    pub status: String,
}

/// RFC-040 replay outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayOutcome {
    pub trace_id: String,
    pub task_id: String,
    pub query: String,
    pub original: DecisionSnapshot,
    pub replayed: DecisionSnapshot,
    pub decision_changed: bool,
    pub quality_changed: bool,
    /// True when a policy override was applied to the replayed run (075).
    pub policy_applied: bool,
    /// A default-policy replay of the SAME request, present when a policy
    /// override was simulated (RFC-075): the comparison baseline for
    /// "did the policy change the outcome?".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<DecisionSnapshot>,
    /// Replays are offline: no provider was ever called.
    pub offline: bool,
    /// Honest caveats (offline divergence, missing trace data, ...).
    #[serde(default)]
    pub notes: Vec<String>,
}

/// RFC-086 aggregate over a batch of replays.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub evaluated: usize,
    pub decision_changes: usize,
    pub quality_changes: usize,
    pub decision_agreement_rate: f32,
    pub policy_applied: bool,
    pub per_trace: Vec<ReplayOutcome>,
}

/// Shared replay/simulation/evaluation engine over the assembled service.
pub struct ReplayService {
    deps: Arc<SearchDependencies>,
    lookup: Arc<dyn RecordedTaskLookup>,
}

impl ReplayService {
    pub fn new(deps: Arc<SearchDependencies>, lookup: Arc<dyn RecordedTaskLookup>) -> Self {
        Self { deps, lookup }
    }

    /// Replays one recorded trace (optionally under a policy override).
    pub async fn replay(
        &self,
        trace_id: &str,
        policy_override: Option<PolicyOverride>,
    ) -> Result<ReplayOutcome, SearchOsError> {
        let task = self.lookup.by_trace_id(trace_id).await?.ok_or_else(|| {
            SearchOsError::Validation(searchos_domain::ValidationError {
                field: "trace_id".into(),
                message: "UNKNOWN_TRACE".into(),
            })
        })?;
        let trace = self.lookup.trace(trace_id).await?;
        let original = DecisionSnapshot {
            decision_path: trace
                .as_ref()
                .map(|t| t.selected_path.clone())
                .unwrap_or_default(),
            decision_reason: String::new(),
            final_outcome: trace.as_ref().and_then(|t| t.final_outcome.clone()),
            estimated_cost_micros: trace
                .as_ref()
                .map(|t| t.estimated_cost.amount_micros)
                .unwrap_or(0),
            quality_status: String::new(),
            status: task.status.clone(),
        };

        // Reconstruct the request from durable facts. A FRESH request id
        // guarantees no idempotency replay and no second bill for the
        // original id; the prefix makes replay tasks attributable.
        let mut req = SearchRequest::new(
            task.query.clone(),
            ProfileId::new(&task.profile_id),
            SearchMode::parse(&task.mode).unwrap_or(SearchMode::LocalFirst),
            WorkspaceId::new(&task.workspace_id),
        )?;
        // A unique id per RUN: idempotency replays share request ids, so a
        // simulation's baseline and overridden runs must never collide (nor
        // two replays of the same trace).
        req = req.with_request_id(format!("replay_{}", searchos_domain::new_id("rep")));
        // Offline by construction: zero external queries, but the ORIGINAL
        // mode/profile/constraints are preserved so the decision is
        // comparable. Provider calls would be a real cost — never allowed.
        req.budget.max_external_queries = 0;
        // Keep the cost budget intact: zeroing it would trigger the
        // decision engine's budget-exceeded rule for EVERY external-allowing
        // mode and make all replays incomparable. Offline is enforced by the
        // injected deps below, not by a fake budget.

        let override_applied = policy_override
            .as_ref()
            .map(|p| !p.is_empty())
            .unwrap_or(false);
        let snapshot_of = |r: &searchos_domain::response::SearchResponse| DecisionSnapshot {
            decision_path: r.decision.path.clone(),
            decision_reason: r.decision.reason.clone(),
            final_outcome: None,
            estimated_cost_micros: (r.cost.estimated * 1_000_000.0) as i64,
            quality_status: format!("{:?}", r.quality.status),
            status: format!("{:?}", r.status),
        };
        // RFC-075: a simulation needs a DEFAULT-policy baseline run of the
        // SAME request; otherwise "changed" cannot be measured (the stored
        // trace carries no quality status).
        // Offline deps: external execution is refused (honest
        // ProviderUnavailable / partial), never a provider purchase.
        let mut offline_deps = (*self.deps).clone();
        offline_deps.allow_external = false;
        // A replay must RECOMPUTE, not read a recorded answer: the exact /
        // negative response caches and singleflight retention would
        // otherwise serve the ORIGINAL run's payload (and a policy
        // simulation would never show a change). Candidate-level caches
        // are still shared (retrieval itself is policy-independent).
        offline_deps.cache = Arc::new(searchos_cache::ExactQueryCache::new(300));
        offline_deps.negative_cache = Arc::new(searchos_cache::ExactQueryCache::new(0));
        offline_deps.singleflight = Arc::new(crate::singleflight::SingleflightCoordinator::new());
        let offline_deps = Arc::new(offline_deps);
        let baseline = if override_applied {
            let base = SearchServiceImpl::new(Arc::clone(&offline_deps))
                .search(req.clone())
                .await?;
            // The simulated run must not read the baseline's cached answer
            // (or join its singleflight result) — otherwise the "what-if"
            // would report no change by construction.
            offline_deps.cache.clear();
            offline_deps.negative_cache.clear();
            offline_deps.singleflight.clear().await;
            Some(snapshot_of(&base))
        } else {
            None
        };
        // The simulated run needs its OWN idempotency identity, distinct
        // from the baseline's (which was just durably recorded).
        let mut simulated_req = req.clone();
        simulated_req =
            simulated_req.with_request_id(format!("replay_sim_{}", searchos_domain::new_id("rep")));
        let service = match policy_override {
            Some(over) if !over.is_empty() => {
                let mut deps = (*offline_deps).clone();
                over.apply(&mut deps.quality);
                SearchServiceImpl::new(Arc::new(deps))
            }
            _ => SearchServiceImpl::new(Arc::clone(&offline_deps)),
        };
        let response = service.search(simulated_req).await?;
        let replayed = snapshot_of(&response);
        let comparison = baseline.as_ref().unwrap_or(&original);
        Ok(ReplayOutcome {
            trace_id: trace_id.to_string(),
            task_id: task.task_id.clone(),
            query: task.query.clone(),
            decision_changed: comparison.decision_path != replayed.decision_path,
            quality_changed: baseline
                .as_ref()
                .map(|b| b.quality_status != replayed.quality_status)
                .unwrap_or(false),
            original,
            replayed,
            policy_applied: override_applied,
            baseline,
            offline: true,
            notes: vec![
                "offline replay: external acquisition disabled; decisions that needed external evidence are reported honestly, not simulated as successes".into(),
                "the stored trace has no quality status; quality_changed is only measurable under a policy simulation".into(),
            ],
        })
    }

    /// RFC-086: aggregate replays over the most recent recorded traces.
    pub async fn evaluate(
        &self,
        limit: u32,
        workspace: Option<&str>,
        policy_override: Option<PolicyOverride>,
    ) -> Result<EvaluationReport, SearchOsError> {
        let ids = self.lookup.recent_trace_ids(limit, workspace).await?;
        let mut per_trace = Vec::with_capacity(ids.len());
        for id in &ids {
            // A missing/corrupt trace row is reported as a skip, not a batch
            // failure: evaluation must be resilient over historical data.
            match self.replay(id, policy_override.clone()).await {
                Ok(outcome) => per_trace.push(outcome),
                Err(error) => {
                    tracing::warn!(trace_id = %id, %error, "replay skipped in evaluation");
                }
            }
        }
        let decision_changes = per_trace.iter().filter(|o| o.decision_changed).count();
        let quality_changes = per_trace.iter().filter(|o| o.quality_changed).count();
        let agreement = if per_trace.is_empty() {
            1.0
        } else {
            (per_trace.len() - decision_changes) as f32 / per_trace.len() as f32
        };
        Ok(EvaluationReport {
            evaluated: per_trace.len(),
            decision_changes,
            quality_changes,
            decision_agreement_rate: agreement,
            policy_applied: policy_override
                .as_ref()
                .map(|p| !p.is_empty())
                .unwrap_or(false),
            per_trace,
        })
    }
}

/// Marker used by HTTP transports to render a quality status label.
pub fn quality_label(status: &QualityStatus) -> &'static str {
    match status {
        QualityStatus::Passed => "passed",
        QualityStatus::Partial => "partial",
        QualityStatus::Conflict => "conflict",
        QualityStatus::Failed => "failed",
        QualityStatus::Unknown => "unknown",
    }
}

/// Status label for the evaluation surface.
pub fn status_label(status: &SearchStatus) -> &'static str {
    status.as_str()
}
