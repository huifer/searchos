//! SearchService (RFC-012 12.1) — the shared use case behind Local Mode and
//! Gateway Mode.
//!
//! Pipeline: normalize → fingerprint → exact cache → singleflight → decision
//! → plan → execute (local / external) → persist → final quality gate →
//! response (cost + trace attached, RFC-012 12.6).

use async_trait::async_trait;
use chrono::Utc;
use searchos_cache::exact::{CacheLookup, CachedSearchResult, ExactQueryCache};
use searchos_cost::{CostEstimator, CostModelConfig};
use searchos_decision::gap::{build_external_plan, InformationNeed};
use searchos_decision::trace::TraceCollector;
use searchos_decision::{
    DecisionEngine, DecisionInput, LocalCandidateEval, LocalKnowledgePort, QualityGate,
};
use searchos_domain::cost::{CostEvent, CostOperation, Money};
use searchos_domain::decision::SearchDecision;
use searchos_domain::ids::{TaskId, TraceId};
use searchos_domain::request::{SearchIntent, SearchMode, SearchRequest};
use searchos_domain::response::{
    CostReport, DecisionReport, FreshnessStatus, QualityReport, QualityStatus, SearchResponse,
    SearchResultItem, SearchStatus,
};
use searchos_domain::retrieval::RetrievalSource;
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, SearchCacheKey};
use searchos_domain::{CoverageGap, Evidence, SearchOsError};
use searchos_planner::router::ProviderRouter;
use searchos_planner::PlanBuilder;
use searchos_policy::{
    FreshnessPolicy, ProfileRegistry, QualityPolicy, SearchProfile, SourcePolicy,
};
use searchos_provider::traits::{ExtractDepth, FetchRequest, SearchParams, SearchProvider};
use searchos_query::{
    analyze, fingerprint, QueryAnalysis, QueryFingerprintParts, TemporalSensitivity,
};
use searchos_retrieval::SearchIndex;
use std::sync::Arc;

use crate::merge::{merge_evidence, MergeReport};
use crate::singleflight::SingleflightCoordinator;

/// Everything the search service needs, injected by the runtime.
#[allow(clippy::too_many_arguments)]
pub struct SearchDependencies {
    pub profiles: ProfileRegistry,
    pub quality: QualityPolicy,
    pub freshness: FreshnessPolicy,
    pub source: SourcePolicy,
    pub cost_config: CostModelConfig,
    pub bm25: Arc<dyn SearchIndex>,
    pub router: Arc<dyn ProviderRouter>,
    pub search_providers: Vec<Arc<dyn SearchProvider>>,
    pub local_port: Arc<dyn LocalKnowledgePort>,
    pub traces: Arc<dyn TraceCollector>,
    pub cost_sink: Arc<dyn CostEventSink>,
    pub task_recorder: Arc<dyn TaskRecorder>,
    pub singleflight: Arc<SingleflightCoordinator>,
    pub cache: Arc<ExactQueryCache>,
    /// When false, external execution is refused (offline mode).
    pub allow_external: bool,
}

/// Records search task rows (Part 17 search_tasks; every task gets a row).
#[async_trait]
pub trait TaskRecorder: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn insert(
        &self,
        task_id: &searchos_domain::ids::TaskId,
        workspace_id: &searchos_domain::ids::WorkspaceId,
        agent_id: Option<&searchos_domain::ids::AgentId>,
        query: &str,
        normalized_query: &str,
        fingerprint: &str,
        profile_id: &str,
        profile_version: &str,
        mode: &str,
        trace_id: &searchos_domain::ids::TraceId,
    ) -> Result<(), SearchOsError>;

    async fn finish(
        &self,
        task_id: &searchos_domain::ids::TaskId,
        status: &str,
    ) -> Result<(), SearchOsError>;

    // --- RFC-014 14.7: request-level idempotency ---------------------
    /// Returns a completed response payload for a replayed request id.
    async fn idempotency_get(
        &self,
        _request_id: &str,
    ) -> Result<Option<serde_json::Value>, SearchOsError> {
        Ok(None)
    }
    /// Claims a request id before execution starts.
    async fn idempotency_begin(
        &self,
        _request_id: &str,
        _task_id: &str,
        _workspace_id: &str,
    ) -> Result<(), SearchOsError> {
        Ok(())
    }
    /// Stores the recoverable task state / response.
    async fn idempotency_complete(
        &self,
        _request_id: &str,
        _payload: &serde_json::Value,
    ) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn idempotency_fail(&self, _request_id: &str) -> Result<(), SearchOsError> {
        Ok(())
    }
    /// RFC-026 2.3: Analytics is best-effort. A failed event write must never
    /// fail the search request.
    async fn record_search_completed(
        &self,
        _response: &serde_json::Value,
    ) -> Result<(), SearchOsError> {
        Ok(())
    }
    /// Enqueues refresh of stale documents (RFC-004 v1.1 10.1). Returns the
    /// number of enqueued URLs.
    async fn enqueue_refresh(
        &self,
        _urls: &[String],
        _workspace_id: &str,
    ) -> Result<usize, SearchOsError> {
        Ok(0)
    }
}

/// Receives cost events (append-only ledger).
#[async_trait]
pub trait CostEventSink: Send + Sync {
    async fn append(&self, ev: CostEvent) -> Result<(), SearchOsError>;
    /// Quality-passing task ids for cost-per-successful-query.
    async fn record_quality_pass(&self, task_id: &str) -> Result<(), SearchOsError>;
    async fn singleflight_saved_micros(&self) -> Result<i64, SearchOsError>;
    async fn record_singleflight(
        &self,
        fingerprint: &str,
        leader: &str,
        followers: &[String],
        saved: i64,
    ) -> Result<(), SearchOsError>;

    // --- RFC-004 v1.1 15 / RFC-006 6.11: reserve -> execute -> settle --
    async fn reserve_budget(
        &self,
        _task_id: &str,
        _trace_id: &str,
        _estimated_micros: i64,
        _currency: &str,
    ) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn settle_budget(
        &self,
        _task_id: &str,
        _actual_micros: i64,
    ) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn exceed_budget(&self, _task_id: &str) -> Result<(), SearchOsError> {
        Ok(())
    }
}

/// Search use case (RFC-002 2.4 SearchService).
#[async_trait]
pub trait SearchService: Send + Sync {
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, SearchOsError>;
}

/// The concrete orchestrator.
pub struct SearchServiceImpl {
    deps: Arc<SearchDependencies>,
}

impl SearchServiceImpl {
    /// Builds the service over injected dependencies.
    pub fn new(deps: Arc<SearchDependencies>) -> Self {
        Self { deps }
    }
}

/// Required sub-questions for coverage analysis (RFC-005 5.6):
/// comparison queries decompose per entity; others stay whole.
pub fn required_subquestions(analysis: &searchos_query::QueryAnalysis) -> Vec<String> {
    if analysis.intents.contains(&SearchIntent::Comparison) && analysis.entities.len() >= 2 {
        let topic = analysis
            .normalized
            .split_whitespace()
            .filter(|w| {
                !analysis
                    .entities
                    .iter()
                    .any(|e| w.eq_ignore_ascii_case(&e.to_lowercase()))
                    && !["compare", "vs", "and", "the", "of", "difference", "between"].contains(w)
            })
            .map(|w| w.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let topic = if topic.is_empty() {
            "overall".to_string()
        } else {
            topic
        };
        analysis
            .entities
            .iter()
            .map(|e| format!("{e} {topic}"))
            .collect()
    } else {
        vec![analysis
            .normalized
            .trim_end_matches('?')
            .trim_end_matches('！')
            .trim_end_matches('!')
            .to_string()]
    }
}

/// Heuristic source reliability from domain (RFC-005 5.3).
fn source_reliability_of(url: &str) -> searchos_domain::evidence::ReliabilityLevel {
    let official_markers = [
        "docs.rs",
        "rust-lang.org",
        "github.com",
        "developer.mozilla.org",
        "openai.com",
        "anthropic.com",
        "python.org",
        "kernel.org",
    ];
    if official_markers.iter().any(|m| url.contains(m)) {
        searchos_domain::evidence::ReliabilityLevel::Official
    } else {
        searchos_domain::evidence::ReliabilityLevel::Community
    }
}

/// Whether a request must bypass the exact search cache.
///
/// RFC-003 3.4: a query containing "latest" cannot be satisfied by an old
/// result merely because its semantic similarity is high. RFC-004 v1.1 9.5:
/// ExternalOnly must not be silently intercepted by Exact Cache / Canonical
/// Cache. ExternalFirst keeps external evidence primary.
pub fn should_bypass_exact_cache(
    req: &SearchRequest,
    analysis: &QueryAnalysis,
    profile: &SearchProfile,
) -> bool {
    matches!(
        req.mode,
        SearchMode::ExternalOnly | SearchMode::ExternalFirst
    ) || matches!(analysis.temporal_sensitivity, TemporalSensitivity::Strict)
        || analysis.intents.iter().any(|i| i.is_time_sensitive())
        || matches!(
            req.constraints.freshness,
            searchos_domain::request::FreshnessRequirement::Strict
                | searchos_domain::request::FreshnessRequirement::MaximumAgeSeconds(_)
        )
        || matches!(
            profile.freshness,
            searchos_domain::request::FreshnessRequirement::Strict
                | searchos_domain::request::FreshnessRequirement::MaximumAgeSeconds(_)
        )
}

/// Explicit task status; partial success is never reported as completed
/// (RFC-002 2.8, RFC-004 v1.1 6.12).
pub fn derive_status(
    decision: &searchos_domain::decision::SearchDecision,
    quality: QualityStatus,
    evidence_count: usize,
    warnings: &[String],
) -> SearchStatus {
    use searchos_domain::decision::{DecisionReason, SearchDecision};
    match decision {
        SearchDecision::Reject {
            reason: DecisionReason::BudgetExceeded,
        } => SearchStatus::BudgetExhausted,
        SearchDecision::Reject {
            reason:
                DecisionReason::ProviderUnavailable
                | DecisionReason::StrictFreshnessRequired
                | DecisionReason::WorkspacePolicyDenied,
        } => SearchStatus::InsufficientEvidence,
        SearchDecision::Reject { .. } => SearchStatus::Failed,
        _ => match quality {
            QualityStatus::Passed if !warnings.is_empty() => SearchStatus::CompletedWithWarnings,
            QualityStatus::Passed => SearchStatus::Completed,
            QualityStatus::Partial | QualityStatus::Conflict => SearchStatus::Partial,
            QualityStatus::Unknown | QualityStatus::Failed if evidence_count == 0 => {
                SearchStatus::InsufficientEvidence
            }
            _ => SearchStatus::Failed,
        },
    }
}

/// Serves local candidates as results + provenance-tracked evidence.
fn local_results_and_evidence(
    candidates: &[RetrievalCandidate],
    limit: usize,
) -> (Vec<SearchResultItem>, Vec<Evidence>, Vec<f32>) {
    let mut items = Vec::new();
    let mut evidence = Vec::new();
    let mut relevances = Vec::new();
    for c in candidates.iter().take(limit) {
        items.push(SearchResultItem {
            title: c.title.clone(),
            url: c.url.clone(),
            content: c.snippet.clone(),
            score: c.score,
            published_at: None,
            source: "local".into(),
        });
        evidence.push(Evidence {
            id: searchos_domain::ids::EvidenceId::generate(),
            source_id: searchos_domain::ids::SourceId::generate(),
            document_version_id: Some(c.version_id.clone()),
            title: c.title.clone(),
            url: c.url.clone(),
            content_ref: None,
            snippet: Some(c.snippet.clone()),
            published_at: None,
            fetched_at: c.fetched_at,
            source_reliability: source_reliability_of(&c.url),
            provenance: searchos_domain::evidence::Provenance {
                origin: searchos_domain::evidence::ProvenanceOrigin::Local,
                provider_id: None,
                source_url: Some(c.url.clone()),
                document_id: Some(c.document_id.clone()),
                retrieved_at: c.fetched_at,
            },
        });
        relevances.push(normalize_candidate_score(c));
    }
    (items, evidence, relevances)
}

#[async_trait]
impl SearchService for SearchServiceImpl {
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse, SearchOsError> {
        let trace_id = TraceId::generate();
        let task_id = TaskId::generate();
        let started = std::time::Instant::now();
        let metrics = searchos_observability::MetricsRegistry::global();

        // ---- Request idempotency (RFC-014 14.7, RFC-004 v1.1 14.2) ----
        // A repeated request_id must not re-run external acquisition,
        // re-bill, or re-write the same document version.
        if let Some(payload) = self
            .deps
            .task_recorder
            .idempotency_get(req.request_id.as_str())
            .await?
        {
            if let Ok(mut replayed) = serde_json::from_value::<SearchResponse>(payload) {
                replayed.replayed = true;
                metrics.record_search(started.elapsed().as_millis() as f64, "idempotent_replay");
                return Ok(replayed);
            }
        }

        // ---- Query intelligence (RFC-003) ------------------------------
        let analysis = analyze(&req.query);
        let fp = fingerprint(&QueryFingerprintParts {
            normalized_query: analysis.normalized.clone(),
            language: analysis.language.clone(),
            profile_version: self
                .deps
                .profiles
                .get(req.profile.as_str())
                .map(|p| p.version.clone())
                .unwrap_or_else(|| searchos_policy::POLICY_SUITE_VERSION.into()),
            freshness_policy: self.deps.freshness.version.clone(),
            source_policy: self.deps.source.version.clone(),
            workspace_scope: req.workspace_id.to_string(),
        });
        let cache_key = SearchCacheKey {
            query_fingerprint: fp.clone(),
            profile_version: self
                .deps
                .profiles
                .get(req.profile.as_str())
                .map(|p| p.version.clone())
                .unwrap_or_default(),
            freshness_policy_version: self.deps.freshness.version.clone(),
            source_policy_version: self.deps.source.version.clone(),
            language: analysis.language.clone(),
            workspace_scope: req.workspace_id.to_string(),
        };

        // ---- Task row + attribution -----------------------------------
        let profile = self
            .deps
            .profiles
            .get(req.profile.as_str())
            .cloned()
            .ok_or_else(|| {
                SearchOsError::validation("profile", format!("unknown profile {}", req.profile))
            })?;

        // ---- Exact cache check (RFC-007 7.2) ---------------------------
        // Hard quality constraint: a strict-freshness query, ExternalOnly or
        // ExternalFirst must never be intercepted by a cache hit
        // (RFC-003 3.4, RFC-004 v1.1 9.5, acceptance SDE-007).
        let bypass_cache = should_bypass_exact_cache(&req, &analysis, &profile);
        if !bypass_cache {
            if let CacheLookup::Hit(hit) = self.deps.cache.get(&cache_key, &profile.version) {
                metrics.inc_cache_hit();
                if let Ok(mut resp) =
                    serde_json::from_value::<SearchResponse>(hit.value.payload.clone())
                {
                    // Keep the original trace id: it is the persisted,
                    // explainable decision for this evidence.
                    resp.replayed = true;
                    return Ok(resp);
                }
            }
        }

        self.deps
            .task_recorder
            .idempotency_begin(
                req.request_id.as_str(),
                task_id.as_str(),
                req.workspace_id.as_str(),
            )
            .await?;

        // ---- Singleflight merge (RFC-009 9.8) --------------------------
        let deps = Arc::clone(&self.deps);
        let req2 = req.clone();
        let fp2 = fp.clone();
        let trace2 = trace_id.clone();
        let task2 = task_id.clone();
        let sf = Arc::clone(&deps.singleflight);
        let executed = sf
            .execute(&fp, move || {
                let deps = Arc::clone(&deps);
                let req = req2;
                let trace_id = trace2;
                let task_id = task2;
                let fp = fp2;
                async move {
                    run_pipeline(deps, req, fp, trace_id, task_id)
                        .await
                        .map_err(|e| e.to_string())
                }
            })
            .await;
        let (payload, is_leader) = match executed {
            Ok(v) => v,
            Err(e) => {
                let _ = self
                    .deps
                    .task_recorder
                    .idempotency_fail(req.request_id.as_str())
                    .await;
                return Err(SearchOsError::Internal(e));
            }
        };

        // Persist the recoverable task state for idempotent replay.
        self.deps
            .task_recorder
            .idempotency_complete(req.request_id.as_str(), &payload)
            .await?;
        // Analytics events are observational only (RFC-026 2.3).
        let mut analytics_payload = payload.clone();
        analytics_payload["workspace_id"] = serde_json::Value::String(req.workspace_id.to_string());
        analytics_payload["request_id"] = serde_json::Value::String(req.request_id.to_string());
        let _ = self
            .deps
            .task_recorder
            .record_search_completed(&analytics_payload)
            .await;

        if !is_leader {
            // Shared execution: external cost recorded once by the leader;
            // this caller records a shared (non-billed) benefit event.
            metrics.inc_singleflight();
            let shared_ev = CostEvent {
                id: searchos_domain::ids::CostEventId::generate(),
                task_id: task_id.clone(),
                workspace_id: req.workspace_id.clone(),
                agent_id: req.agent_id.clone(),
                operation: CostOperation::WebSearch,
                provider_id: None,
                input_units: 0,
                output_units: 0,
                estimated_cost: Money::zero(&req.budget.currency),
                actual_cost: None,
                duration_ms: started.elapsed().as_millis() as u64,
                occurred_at: Utc::now(),
                trace_id: Some(trace_id.clone()),
                shared: true,
                pricing_model_version: "1".into(),
            };
            self.deps.cost_sink.append(shared_ev).await?;
            self.deps
                .cost_sink
                .record_singleflight(&fp, "", &[task_id.to_string()], 0)
                .await?;
        }

        let response: SearchResponse =
            serde_json::from_value(payload).map_err(|e| SearchOsError::Internal(e.to_string()))?;

        // Cache the passing responses only (RFC-007: stale/failed never
        // reused as passing). Freshness-forced paths are never cacheable as
        // a future valid hit.
        if !bypass_cache && response.quality.status == QualityStatus::Passed {
            self.deps.cache.put(
                &cache_key,
                CachedSearchResult {
                    payload: serde_json::to_value(&response)
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?,
                    quality_status: "passed".into(),
                },
                &profile.version,
            );
        }

        metrics.record_search(
            started.elapsed().as_millis() as f64,
            &response.decision.path,
        );
        metrics.record_quality(response.quality.status.as_str());
        metrics.record_status(response.status.as_str());
        // Appendix C: only a quality-passing local resolution counts as a
        // valid cost optimisation; "not searching" with stale evidence does
        // not.
        if response.quality.status == QualityStatus::Passed {
            if response.decision.path == "local" || response.decision.path == "local_with_refresh" {
                metrics.record_valid_local_resolution();
                metrics.record_external_avoided();
            } else {
                metrics.record_external_call();
            }
        }
        Ok(response)
    }
}

/// Full pipeline run (singleflight leader path).
async fn run_pipeline(
    deps: Arc<SearchDependencies>,
    req: SearchRequest,
    fp: String,
    trace_id: TraceId,
    task_id: TaskId,
) -> Result<serde_json::Value, SearchOsError> {
    let metrics = searchos_observability::MetricsRegistry::global();
    let estimator = CostEstimator::from_config(&deps.cost_config);
    let mut warnings: Vec<String> = Vec::new();
    let analysis = analyze(&req.query);
    let profile = deps
        .profiles
        .get(req.profile.as_str())
        .cloned()
        .ok_or_else(|| {
            SearchOsError::validation("profile", format!("unknown profile {}", req.profile))
        })?;

    // Task row (Part 17 search_tasks) — every task is recorded.
    deps.task_recorder
        .insert(
            &task_id,
            &req.workspace_id,
            req.agent_id.as_ref(),
            &req.query,
            &analysis.normalized,
            &fp,
            req.profile.as_str(),
            &profile.version,
            req.mode.as_str(),
            &trace_id,
        )
        .await?;

    // Local availability evaluation (RFC-004 4.2 step).
    let local_hits = deps.local_port.retrieve(&analysis.normalized, 10).await?;
    let freshness_policy = deps.freshness.clone();
    let source_policy = deps.source.clone().with_constraints(
        &req.constraints.required_domains,
        &req.constraints.excluded_domains,
    );
    let local_candidates: Vec<LocalCandidateEval> = local_hits
        .iter()
        .map(|c| {
            let category = searchos_policy::classify_content(&c.url, c.title.as_deref());
            let age = (Utc::now() - c.fetched_at).num_seconds().max(0);
            LocalCandidateEval {
                candidate: c.clone(),
                relevance: normalize_candidate_score(c),
                freshness_ok: freshness_policy.is_fresh(category, age),
                source_ok: source_policy.allows(&c.url),
            }
        })
        .collect();
    // Stale local documents drive RefreshExistingSource (RFC-004 v1.1 10.1).
    let stale_document_ids = deps
        .local_port
        .stale_document_ids(&local_hits)
        .await
        .unwrap_or_default();

    // ---- Decision (RFC-004) ------------------------------------------
    let gate = QualityGate::new(
        deps.quality.clone(),
        deps.freshness.clone(),
        deps.source.clone(),
    );
    let engine = searchos_decision::DefaultDecisionEngine::new(gate);
    let estimated_external = estimator
        .estimate_request(
            deps.router
                .route(true, req.budget.max_total_cost_micros)
                .await?
                .first()
                .map(|e| e.id.as_str())
                .unwrap_or("tavily"),
        )
        .amount_micros
        .saturating_mul(req.budget.max_external_queries as i64);
    let decision_input = DecisionInput {
        analysis: analysis.clone(),
        profile,
        mode: req.mode,
        budget: req.budget.clone(),
        local_candidates,
        estimated_external_cost_micros: estimated_external,
        currency: req.budget.currency.clone(),
        required_subquestions: required_subquestions(&analysis),
        required_domains: req.constraints.required_domains.clone(),
        excluded_domains: req.constraints.excluded_domains.clone(),
        stale_document_ids: stale_document_ids.clone(),
    };
    let (mut decision, mut trace) = engine.decide(decision_input).await?;
    // Offline / provider-unavailable must never be disguised as a local
    // success (RFC-004 v1.1 13.2, RFC-002 2.8). The original mode is kept
    // for the decision; only the executable path is rejected explicitly.
    if !deps.allow_external
        && (req.mode == SearchMode::ExternalOnly || decision.requires_external())
    {
        decision = searchos_domain::decision::SearchDecision::Reject {
            reason: searchos_domain::decision::DecisionReason::ProviderUnavailable,
        };
        warnings.push("provider_unavailable:offline_external_execution_disabled".into());
    }
    trace.task_id = task_id.clone();
    trace.trace_id = trace_id.clone();
    deps.traces.record(trace.clone()).await?;
    metrics.record_search(0.0, decision.path_name()); // latency recorded by caller

    // ---- Plan (RFC-009 9.2) -------------------------------------------
    let builder = PlanBuilder::new(&estimator);
    let provider_id = deps
        .router
        .route(true, req.budget.max_total_cost_micros)
        .await?
        .first()
        .map(|e| e.id.clone());
    let plan = builder.build(
        task_id.clone(),
        &decision,
        &req.budget,
        provider_id.as_deref(),
    )?;
    // Budget enforcement BEFORE execution (RFC-009 9.6).
    let budget_check = searchos_policy::BudgetEnforcer::new(None, req.budget.currency.clone())
        .check(&req.budget, plan.estimated_cost.amount_micros);
    if !budget_check.is_allowed() {
        let _ = deps.cost_sink.exceed_budget(task_id.as_str()).await;
        return Err(SearchOsError::Budget(
            budget_check.violation().unwrap_or("budget").to_string(),
        ));
    }
    // Budget is a decision constraint, not事后统计: reserve before
    // execution, settle with actual spend afterwards (RFC-004 v1.1 15).
    deps.cost_sink
        .reserve_budget(
            task_id.as_str(),
            trace_id.as_str(),
            plan.estimated_cost.amount_micros,
            &req.budget.currency,
        )
        .await?;

    // ---- Execute -------------------------------------------------------
    let mut results: Vec<SearchResultItem> = Vec::new();
    let mut evidence: Vec<Evidence> = Vec::new();
    let mut cost_spent = Money::zero(&req.budget.currency);

    match &decision {
        SearchDecision::LocalOnly { .. } => {
            // Serve from local candidates (already retrieved above).
            let (l_items, l_ev, _rel) = local_results_and_evidence(&local_hits, 5);
            results.extend(l_items);
            evidence.extend(l_ev);
            // Local compute cost (RFC-008: 不能把本地成本记为零).
            let local_cost = estimator.estimate_cpu_millis(30);
            let local_cost_out = local_cost.clone();
            cost_spent = cost_spent
                .checked_add(&local_cost)
                .map_err(SearchOsError::Internal)?;
            let ev = CostEvent {
                id: searchos_domain::ids::CostEventId::generate(),
                task_id: task_id.clone(),
                workspace_id: req.workspace_id.clone(),
                agent_id: req.agent_id.clone(),
                operation: CostOperation::LocalBm25,
                provider_id: None,
                input_units: 30,
                output_units: 0,
                estimated_cost: local_cost_out,
                actual_cost: None,
                duration_ms: 30,
                occurred_at: Utc::now(),
                trace_id: Some(trace_id.clone()),
                shared: false,
                pricing_model_version: "1".into(),
            };
            deps.cost_sink.append(ev).await?;
            metrics.add_cost_micros(local_cost.amount_micros);
        }
        SearchDecision::Reject { reason } => {
            // Provide partial local results + explicit reason (RFC-004 Rule 5).
            for c in deps
                .local_port
                .retrieve(&analysis.normalized, 3)
                .await?
                .into_iter()
                .take(3)
            {
                results.push(SearchResultItem {
                    title: c.title.clone(),
                    url: c.url.clone(),
                    content: c.snippet.clone(),
                    score: c.score,
                    published_at: None,
                    source: "local_partial".into(),
                });
            }
            warnings.push(format!("rejected: {}", reason.code()));
            let status = derive_status(&decision, QualityStatus::Failed, 0, &warnings);
            if *reason == searchos_domain::decision::DecisionReason::BudgetExceeded {
                deps.cost_sink.exceed_budget(task_id.as_str()).await?;
            }
            deps.task_recorder
                .finish(
                    &task_id,
                    if status == SearchStatus::InsufficientEvidence {
                        "partial"
                    } else {
                        "failed"
                    },
                )
                .await?;
            let response = build_response(
                &req,
                &decision,
                results,
                QualityStatus::Failed,
                FreshnessStatus::Unknown,
                searchos_domain::response::CoverageStatus::Insufficient,
                vec![format!("rejected: {}", reason.code())],
                cost_spent.clone(),
                trace_id.clone(),
                evidence,
                warnings,
                &task_id,
                status,
            );
            return serde_json::to_value(&response)
                .map_err(|e| SearchOsError::Internal(e.to_string()));
        }
        SearchDecision::Hybrid { .. } | SearchDecision::ExternalOnly { .. } => {
            let gaps: Vec<CoverageGap> = match &decision {
                SearchDecision::Hybrid {
                    missing_requirements,
                    ..
                } => missing_requirements.clone(),
                _ => Vec::new(),
            };
            // Gap-Based Search: each external query is linked to one open
            // information requirement, and covered needs are never re-bought
            // (RFC-004 v1.1 6.8/10).
            let needs: Vec<InformationNeed> = gaps
                .iter()
                .map(|g| InformationNeed {
                    id: g.requirement_id.clone(),
                    description: g.question.clone(),
                    status: g.status,
                    priority: g.priority,
                    strategy: g.strategy,
                })
                .collect();
            let per_query_cost = estimator
                .estimate_request(provider_id.as_deref().unwrap_or("tavily"))
                .amount_micros;
            let external_plan = build_external_plan(
                &needs,
                req.request_id.as_str(),
                req.budget.max_external_queries,
                req.budget.max_total_cost_micros,
                per_query_cost,
                &req.constraints.required_domains,
                &req.constraints.excluded_domains,
            );
            let mut queries: Vec<String> = external_plan
                .queries
                .iter()
                .map(|q| q.query.clone())
                .collect();
            if queries.is_empty() {
                queries.push(req.query.clone());
            }

            // Local evidence supplements external evidence in Hybrid and
            // ExternalFirst (RFC-004 v1.1 9.3/9.4), never replaces it.
            if matches!(req.mode, SearchMode::Hybrid | SearchMode::ExternalFirst) {
                let (l_items, l_ev, _rel) = local_results_and_evidence(&local_hits, 5);
                results.extend(l_items);
                evidence.extend(l_ev);
            }

            let svc = SearchServiceImplStub {
                deps: Arc::clone(&deps),
                estimator: searchos_cost::CostEstimator::from_config(&deps.cost_config),
            };
            let execution = svc
                .execute_external(&req, &queries, &trace_id, &task_id)
                .await;
            let external_items = match execution {
                Ok((items, evs, _calls, spent)) => {
                    results.extend(items.clone());
                    evidence.extend(evs);
                    cost_spent = Money {
                        amount_micros: spent,
                        currency: req.budget.currency.clone(),
                    };
                    items
                }
                Err(e) => {
                    tracing::warn!("external execution failed, partial results: {e}");
                    metrics.inc_provider_failure();
                    warnings.push(format!("external_execution_failed:{}", e));
                    Vec::new()
                }
            };

            // Persist only newly acquired external pages for reuse
            // (Incremental Reuse P-007).
            for item in external_items.iter().take(2) {
                let payload = IndexedDocument {
                    chunk_id: searchos_domain::ids::ChunkId::generate(),
                    document_id: searchos_domain::ids::DocumentId::generate(),
                    version_id: searchos_domain::ids::DocumentVersionId::generate(),
                    title: item.title.clone(),
                    body: item.content.clone(),
                    url: item.url.clone(),
                    language: req.constraints.language.clone(),
                    workspace_id: req.workspace_id.to_string(),
                    fetched_at: Utc::now(),
                };
                deps.bm25.index(payload).await.ok(); // best-effort persistence
            }
            deps.bm25.count().await.ok();
        }
        SearchDecision::LocalWithRefresh { documents, .. } => {
            // RefreshExistingSource: serve valid local evidence AND enqueue
            // re-fetch of stale URLs. Never delete authoritative knowledge
            // (RFC-006 9.5, RFC-018 18.2).
            let (l_items, l_ev, _rel) = local_results_and_evidence(&local_hits, 5);
            results.extend(l_items);
            evidence.extend(l_ev);
            let urls: Vec<String> = local_hits
                .iter()
                .filter(|c| documents.contains(&c.document_id))
                .map(|c| c.url.clone())
                .collect();
            let enqueued = deps
                .task_recorder
                .enqueue_refresh(&urls, req.workspace_id.as_str())
                .await?;
            warnings.push(format!("refresh_enqueued:{}", enqueued));
            let refresh_cost = estimator.estimate_request("web_fetch");
            cost_spent = cost_spent
                .checked_add(&refresh_cost)
                .map_err(SearchOsError::Internal)?;
            deps.cost_sink
                .append(CostEvent {
                    id: searchos_domain::ids::CostEventId::generate(),
                    task_id: task_id.clone(),
                    workspace_id: req.workspace_id.clone(),
                    agent_id: req.agent_id.clone(),
                    operation: CostOperation::WebFetch,
                    provider_id: None,
                    input_units: enqueued as u64,
                    output_units: 0,
                    estimated_cost: refresh_cost.clone(),
                    actual_cost: None,
                    duration_ms: 0,
                    occurred_at: Utc::now(),
                    trace_id: Some(trace_id.clone()),
                    shared: false,
                    pricing_model_version: "1".into(),
                })
                .await?;
            metrics.add_cost_micros(refresh_cost.amount_micros);
        }
    }

    // ---- Evidence merge (RFC-004 v1.1 6.11) ---------------------------
    let (merged_evidence, merge_report): (Vec<Evidence>, MergeReport) =
        merge_evidence(evidence, Vec::new());
    for c in &merge_report.conflicts {
        warnings.push(c.clone());
    }
    if merge_report.duplicates_removed > 0 {
        warnings.push(format!(
            "duplicate_canonical_documents:{}",
            merge_report.duplicates_removed
        ));
    }

    // ---- Final quality gate over the full evidence set (RFC-005) --------
    let gate = QualityGate::new(
        deps.quality.clone(),
        deps.freshness.clone(),
        deps.source.clone(),
    );
    // Relevance comes from the actual retrieval / provider scores, not a
    // constant placeholder (RFC-005 5.4: measured features only).
    let relevances: Vec<f32> = merged_evidence
        .iter()
        .map(|ev| {
            results
                .iter()
                .find(|r| r.url == ev.url)
                .map(|r| (r.score / 3.0).clamp(0.05, 1.0))
                .unwrap_or(0.6)
        })
        .collect();
    let mut verdict = gate.evaluate_set(
        &merged_evidence,
        &relevances,
        &required_subquestions(&analysis),
    );
    if !merge_report.conflicts.is_empty() {
        verdict.status = QualityStatus::Conflict;
        verdict
            .failed_constraints
            .push("unresolved_evidence_conflict".into());
    }
    if verdict.status == QualityStatus::Passed {
        deps.cost_sink.record_quality_pass(task_id.as_str()).await?;
        metrics.inc_coverage_pass();
    }
    if verdict.freshness == FreshnessStatus::Stale {
        metrics.inc_freshness_violation();
    }

    let status = derive_status(&decision, verdict.status, merged_evidence.len(), &warnings);
    deps.task_recorder
        .finish(
            &task_id,
            match status {
                SearchStatus::Failed | SearchStatus::BudgetExhausted => "failed",
                SearchStatus::Partial | SearchStatus::InsufficientEvidence => "partial",
                _ => "completed",
            },
        )
        .await?;
    deps.cost_sink
        .settle_budget(task_id.as_str(), cost_spent.amount_micros)
        .await?;

    let response = build_response(
        &req,
        &decision,
        results,
        verdict.status,
        verdict.freshness,
        verdict.coverage,
        verdict.failed_constraints.clone(),
        cost_spent,
        trace_id.clone(),
        merged_evidence,
        warnings,
        &task_id,
        status,
    );

    // Cache payload keyed under fp via caller.
    let _ = fp;
    serde_json::to_value(&response).map_err(|e| SearchOsError::Internal(e.to_string()))
}

fn normalize_candidate_score(c: &RetrievalCandidate) -> f32 {
    match c.source {
        RetrievalSource::Hybrid => (c.score * 61.0 / 2.0).clamp(0.05, 1.0), // RRF ~2/61 → 1.0
        _ => (c.score / 3.0).clamp(0.05, 1.0),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_response(
    req: &SearchRequest,
    decision: &SearchDecision,
    results: Vec<SearchResultItem>,
    quality_status: QualityStatus,
    freshness: FreshnessStatus,
    coverage: searchos_domain::response::CoverageStatus,
    failed_constraints: Vec<String>,
    cost: Money,
    trace_id: TraceId,
    provenance: Vec<Evidence>,
    warnings: Vec<String>,
    task_id: &TaskId,
    search_status: SearchStatus,
) -> SearchResponse {
    SearchResponse {
        query: req.query.clone(),
        decision: DecisionReport {
            path: decision.path_name().into(),
            reason: decision.reason().code().into(),
        },
        results,
        quality: QualityReport {
            status: quality_status,
            freshness,
            coverage,
            score: 0.0,
            failed_constraints,
        },
        cost: CostReport {
            estimated: cost.amount_micros as f64 / 1_000_000.0,
            actual: None, // no real bill yet (RFC-012 12.6)
            currency: cost.currency,
        },
        provenance,
        trace_id,
        status: search_status,
        warnings,
        task_id: Some(task_id.to_string()),
        replayed: false,
    }
}

/// Thin internal handle to reuse execute_external from the pipeline.
struct SearchServiceImplStub {
    deps: Arc<SearchDependencies>,
    estimator: CostEstimator,
}

#[allow(clippy::unused_self)]
impl SearchServiceImplStub {
    async fn execute_external(
        &self,
        req: &SearchRequest,
        planned_queries: &[String],
        trace_id: &TraceId,
        task_id: &TaskId,
    ) -> Result<(Vec<SearchResultItem>, Vec<Evidence>, u64, i64), SearchOsError> {
        if !self.deps.allow_external {
            return Err(SearchOsError::Policy(
                "external execution disabled (offline)".into(),
            ));
        }
        let selection = self
            .deps
            .router
            .route(true, req.budget.max_total_cost_micros)
            .await?;
        let Some(entry) = selection.first().cloned() else {
            return Err(SearchOsError::Provider(
                "no healthy provider satisfies quality/budget constraints".into(),
            ));
        };
        let provider = self
            .deps
            .search_providers
            .iter()
            .find(|p| p.id() == entry.id)
            .ok_or_else(|| SearchOsError::Provider("router picked unregistered provider".into()))?;

        let mut queries: Vec<String> = planned_queries
            .iter()
            .map(|q| q.trim().to_string())
            .filter(|q| !q.is_empty())
            .take(req.budget.max_external_queries.max(1) as usize)
            .collect();
        if queries.is_empty() {
            queries.push(req.query.clone());
        }

        let mut items = Vec::new();
        let mut evidence = Vec::new();
        let mut calls = 0u64;
        let mut spent = Money::zero(&req.budget.currency);

        for q in &queries {
            if calls >= req.budget.max_external_queries as u64 {
                break;
            }
            if spent.amount_micros + self.estimator.estimate_request(provider.id()).amount_micros
                > req.budget.max_total_cost_micros
            {
                break;
            }
            let params = SearchParams {
                query: q.clone(),
                max_results: 5,
                required_domains: req.constraints.required_domains.clone(),
                excluded_domains: req.constraints.excluded_domains.clone(),
                language: req.constraints.language.clone(),
                freshness_days: match &req.constraints.freshness {
                    searchos_domain::request::FreshnessRequirement::Strict => Some(1),
                    searchos_domain::request::FreshnessRequirement::Recent => Some(30),
                    searchos_domain::request::FreshnessRequirement::MaximumAgeSeconds(s) => {
                        Some((s / 86400).max(1) as u32)
                    }
                    searchos_domain::request::FreshnessRequirement::Any => None,
                },
                include_raw_content: false,
            };
            let result = provider
                .search(&params)
                .await
                .map_err(SearchOsError::from)?;
            calls += 1;
            let price = self.estimator.estimate_request(provider.id());
            spent = spent.checked_add(&price).map_err(SearchOsError::Internal)?;
            let ev = CostEvent {
                id: searchos_domain::ids::CostEventId::generate(),
                task_id: task_id.clone(),
                workspace_id: req.workspace_id.clone(),
                agent_id: req.agent_id.clone(),
                operation: CostOperation::WebSearch,
                provider_id: Some(provider.id().to_string()),
                input_units: 1,
                output_units: 0,
                estimated_cost: price.clone(),
                actual_cost: None,
                duration_ms: result.duration_ms,
                occurred_at: Utc::now(),
                trace_id: Some(trace_id.clone()),
                shared: false,
                pricing_model_version: "1".into(),
            };
            self.deps.cost_sink.append(ev).await?;
            searchos_observability::MetricsRegistry::global().add_cost_micros(price.amount_micros);

            for entry in result.entries.iter().take(5) {
                items.push(SearchResultItem {
                    title: entry.title.clone(),
                    url: entry.url.clone(),
                    content: entry.content.clone(),
                    score: entry.score,
                    published_at: entry.published_at,
                    source: provider.id().to_string(),
                });
                evidence.push(Evidence {
                    id: searchos_domain::ids::EvidenceId::generate(),
                    source_id: searchos_domain::ids::SourceId::generate(),
                    document_version_id: None,
                    title: entry.title.clone(),
                    url: entry.url.clone(),
                    content_ref: None,
                    snippet: Some(entry.content.clone()),
                    published_at: entry.published_at,
                    fetched_at: Utc::now(),
                    source_reliability: source_reliability_of(&entry.url),
                    provenance: searchos_domain::evidence::Provenance {
                        origin: searchos_domain::evidence::ProvenanceOrigin::External,
                        provider_id: Some(provider.id().to_string()),
                        source_url: Some(entry.url.clone()),
                        document_id: None,
                        retrieved_at: Utc::now(),
                    },
                });
            }
        }
        Ok((items, evidence, calls, spent.amount_micros))
    }
}

/// Unused import suppression: FetchRequest/ExtractDepth referenced by
/// provider plumbing in sibling services.
#[allow(dead_code)]
fn _unused(_r: FetchRequest, _d: ExtractDepth) {}

#[cfg(test)]
mod completion_tests {
    use super::*;
    use searchos_domain::decision::{DecisionReason, SearchDecision};
    use searchos_domain::ids::ProfileId;
    use searchos_domain::request::FreshnessRequirement;
    use searchos_domain::WorkspaceId;
    use searchos_query::analyze;

    fn request(query: &str, mode: SearchMode) -> SearchRequest {
        SearchRequest::new(query, ProfileId::general(), mode, WorkspaceId::new("ws")).unwrap()
    }

    fn profile(id: &str) -> SearchProfile {
        searchos_policy::ProfileRegistry::builtin()
            .get(id)
            .unwrap()
            .clone()
    }

    #[test]
    fn cache_bypass_for_external_only_and_strict_freshness() {
        let profile = profile("general");
        let plain = request("what is tokio", SearchMode::LocalFirst);
        assert!(!should_bypass_exact_cache(
            &plain,
            &analyze(&plain.query),
            &profile
        ));

        let external = request("what is tokio", SearchMode::ExternalOnly);
        assert!(should_bypass_exact_cache(
            &external,
            &analyze(&external.query),
            &profile
        ));
        let external_first = request("what is tokio", SearchMode::ExternalFirst);
        assert!(should_bypass_exact_cache(
            &external_first,
            &analyze(&external_first.query),
            &profile
        ));

        let latest = request("latest tokio version", SearchMode::LocalFirst);
        assert!(should_bypass_exact_cache(
            &latest,
            &analyze(&latest.query),
            &profile
        ));

        let mut strict = request("what is tokio", SearchMode::LocalFirst);
        strict.constraints.freshness = FreshnessRequirement::Strict;
        assert!(should_bypass_exact_cache(
            &strict,
            &analyze(&strict.query),
            &profile
        ));
    }

    #[test]
    fn status_never_disguises_partial_as_completed() {
        let local = SearchDecision::LocalOnly {
            reason: DecisionReason::LocalEvidencePassedQualityGate,
        };
        assert_eq!(
            derive_status(&local, QualityStatus::Passed, 3, &[]),
            SearchStatus::Completed
        );
        assert_eq!(
            derive_status(
                &local,
                QualityStatus::Passed,
                3,
                &["refresh_enqueued:1".into()]
            ),
            SearchStatus::CompletedWithWarnings
        );
        assert_eq!(
            derive_status(&local, QualityStatus::Unknown, 0, &[]),
            SearchStatus::InsufficientEvidence
        );
        assert_eq!(
            derive_status(&local, QualityStatus::Partial, 2, &[]),
            SearchStatus::Partial
        );
        let reject = SearchDecision::Reject {
            reason: DecisionReason::BudgetExceeded,
        };
        assert_eq!(
            derive_status(&reject, QualityStatus::Failed, 0, &[]),
            SearchStatus::BudgetExhausted
        );
    }

    #[test]
    fn request_id_can_be_set_by_agent_clients() {
        let req = request("q", SearchMode::LocalFirst).with_request_id("agent-req-42");
        assert_eq!(req.request_id.as_str(), "agent-req-42");
    }
}
