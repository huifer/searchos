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
    SearchResultItem,
};
use searchos_domain::retrieval::RetrievalSource;
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, SearchCacheKey};
use searchos_domain::{CoverageGap, Evidence, SearchOsError};
use searchos_planner::router::ProviderRouter;
use searchos_planner::PlanBuilder;
use searchos_policy::{FreshnessPolicy, ProfileRegistry, QualityPolicy, SourcePolicy};
use searchos_provider::traits::{ExtractDepth, FetchRequest, SearchParams, SearchProvider};
use searchos_query::{analyze, fingerprint, QueryFingerprintParts};
use searchos_retrieval::SearchIndex;
use std::sync::Arc;

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

#[async_trait]
impl SearchService for SearchServiceImpl {
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse, SearchOsError> {
        let trace_id = TraceId::generate();
        let task_id = TaskId::generate();
        let started = std::time::Instant::now();
        let metrics = searchos_observability::MetricsRegistry::global();

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
        if let CacheLookup::Hit(hit) = self.deps.cache.get(&cache_key, &profile.version) {
            metrics.inc_cache_hit();
            if let Ok(mut resp) =
                serde_json::from_value::<SearchResponse>(hit.value.payload.clone())
            {
                resp.trace_id = trace_id.clone();
                return Ok(resp);
            }
        }

        // ---- Singleflight merge (RFC-009 9.8) --------------------------
        let deps = Arc::clone(&self.deps);
        let req2 = req.clone();
        let fp2 = fp.clone();
        let trace2 = trace_id.clone();
        let task2 = task_id.clone();
        let sf = Arc::clone(&deps.singleflight);
        let (payload, is_leader) = sf
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
            .await
            .map_err(SearchOsError::Internal)?;

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

        let mut response: SearchResponse =
            serde_json::from_value(payload).map_err(|e| SearchOsError::Internal(e.to_string()))?;
        response.trace_id = trace_id.clone();

        // Cache the passing responses only (RFC-007: stale/failed never
        // reused as passing).
        if response.quality.status == QualityStatus::Passed {
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
        mode: if !deps.allow_external && req.mode.allows_external() {
            SearchMode::LocalOnly
        } else {
            req.mode
        },
        budget: req.budget.clone(),
        local_candidates,
        estimated_external_cost_micros: estimated_external,
        currency: req.budget.currency.clone(),
        required_subquestions: required_subquestions(&analysis),
        required_domains: req.constraints.required_domains.clone(),
        excluded_domains: req.constraints.excluded_domains.clone(),
    };
    let (decision, mut trace) = engine.decide(decision_input).await?;
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
        return Err(SearchOsError::Budget(
            budget_check.violation().unwrap_or("budget").to_string(),
        ));
    }

    // ---- Execute -------------------------------------------------------
    let mut results: Vec<SearchResultItem> = Vec::new();
    let mut evidence: Vec<Evidence> = Vec::new();
    let mut cost_spent = Money::zero(&req.budget.currency);

    match &decision {
        SearchDecision::LocalOnly { .. } => {
            // Serve from local candidates (already retrieved above).
            for c in deps
                .local_port
                .retrieve(&analysis.normalized, 10)
                .await?
                .into_iter()
                .take(5)
            {
                results.push(SearchResultItem {
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
            }
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
            );
            return serde_json::to_value(&response)
                .map_err(|e| SearchOsError::Internal(e.to_string()));
        }
        SearchDecision::Hybrid { .. } | SearchDecision::ExternalOnly { .. } => {
            let gaps = match &decision {
                SearchDecision::Hybrid {
                    missing_requirements,
                    ..
                } => missing_requirements.clone(),
                _ => Vec::new(),
            };
            let svc = SearchServiceImplStub {
                deps: Arc::clone(&deps),
                estimator: searchos_cost::CostEstimator::from_config(&deps.cost_config),
            };
            let (items, evs, _calls, spent) = svc
                .execute_external(&req, &gaps, &trace_id, &task_id)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("external execution failed, partial results: {e}");
                    metrics.inc_provider_failure();
                    (Vec::new(), Vec::new(), 0, 0)
                });
            results.extend(items);
            evidence.extend(evs);
            cost_spent = Money {
                amount_micros: spent,
                currency: req.budget.currency.clone(),
            };

            // Persist fetched pages into local knowledge for reuse
            // (Incremental Reuse P-007): fetch top-2 results into the
            // document store via the local port's persist hook.
            for item in results.iter().take(2) {
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
            for d in documents {
                deps.bm25.delete(d).await.ok();
            }
        }
    }

    // ---- Final quality gate over the full evidence set (RFC-005) --------
    let gate = QualityGate::new(
        deps.quality.clone(),
        deps.freshness.clone(),
        deps.source.clone(),
    );
    let relevances: Vec<f32> = evidence.iter().map(|_e| 0.8).collect();
    let verdict = gate.evaluate_set(&evidence, &relevances, &required_subquestions(&analysis));
    if verdict.status == QualityStatus::Passed {
        deps.cost_sink.record_quality_pass(task_id.as_str()).await?;
        metrics.inc_coverage_pass();
    }
    if verdict.freshness == FreshnessStatus::Stale {
        metrics.inc_freshness_violation();
    }

    deps.task_recorder
        .finish(
            &task_id,
            if verdict.status == QualityStatus::Failed {
                "failed"
            } else {
                "completed"
            },
        )
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
        evidence,
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
    status: QualityStatus,
    freshness: FreshnessStatus,
    coverage: searchos_domain::response::CoverageStatus,
    failed_constraints: Vec<String>,
    cost: Money,
    trace_id: TraceId,
    provenance: Vec<Evidence>,
) -> SearchResponse {
    SearchResponse {
        query: req.query.clone(),
        decision: DecisionReport {
            path: decision.path_name().into(),
            reason: decision.reason().code().into(),
        },
        results,
        quality: QualityReport {
            status,
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
        gaps: &[CoverageGap],
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

        let mut queries: Vec<String> = gaps
            .iter()
            .map(|g| g.question.clone())
            .filter(|q| !q.trim().is_empty())
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
