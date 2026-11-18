//! SearchService (RFC-012 12.1) — the shared use case behind Local Mode and
//! Gateway Mode.
//!
//! Pipeline: normalize → fingerprint → exact cache → singleflight → decision
//! → plan → execute (local / external) → persist → final quality gate →
//! response (cost + trace attached, RFC-012 12.6).

use async_trait::async_trait;
use chrono::Utc;
use searchos_cache::exact::{CacheLookup, CachedSearchResult, ExactQueryCache};
use searchos_cache::{
    CanonicalQueryCache, EmbeddingCache, RerankCache, RetrievalCache, SemanticCache,
    SemanticCompatibility,
};
use searchos_cost::{CostEstimator, CostModelConfig};
use searchos_decision::gap::{build_external_plan, InformationNeed};
use searchos_decision::trace::TraceCollector;
use searchos_decision::{
    DecisionEngine, DecisionInput, LocalCandidateEval, LocalKnowledgePort, QualityGate,
};
use searchos_document::content_hash;
use searchos_domain::cost::{CostAttemptStatus, CostEvent, CostOperation, Money};
use searchos_domain::decision::{DecisionReason, SearchDecision};
use searchos_domain::ids::{TaskId, TraceId};
use searchos_domain::promotion::{EvidenceItem, PromotionState, QueryOrigin};
use searchos_domain::request::{
    clamp_max_results, FreshnessRequirement, SearchIntent, SearchMode, SearchRequest,
    DEFAULT_MAX_RESULTS,
};
use searchos_domain::response::{
    CostReport, CoverageStatus, DecisionReport, FreshnessStatus, QualityReport, QualityStatus,
    SearchResponse, SearchResultItem, SearchStatus,
};
use searchos_domain::retrieval::{
    RetrievalCandidate, RetrievalQuery, RetrievalSource, SearchCacheKey, CACHE_KEY_VERSION,
};
use searchos_domain::{CoverageGap, Evidence, SearchOsError};
use searchos_federated::source::{SourceExecutionStatus, SourceSearchRequest};
use searchos_planner::router::ProviderRouter;
use searchos_planner::PlanBuilder;
use searchos_policy::{
    FreshnessPolicy, ProfileRegistry, QualityPolicy, SearchProfile, SourcePolicy,
};
use searchos_provider::embedding::EmbeddingProvider;
use searchos_provider::traits::{ExtractDepth, FetchRequest, SearchParams, SearchProvider};
use searchos_query::{
    analyze, fingerprint, ExpansionConfig, QueryAnalysis, QueryExpander, QueryFingerprintParts,
    TemporalSensitivity,
};
use searchos_retrieval::{HybridRetriever, SearchIndex, VectorIndex};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::merge::{merge_evidence, MergeReport};
use crate::promotion_service::{build_acquisition, EvidencePromotionService};
use crate::singleflight::SingleflightCoordinator;

/// Result of atomically claiming a request id (Part 30 §30.5).
#[derive(Debug, Clone)]
pub enum IdempotencyClaim {
    Claimed,
    AlreadyRunning { task_id: String },
    Completed { response: serde_json::Value },
    Failed,
}

/// Synchronous + async settlement handle behind the RAII budget guard.
#[async_trait]
pub trait BudgetSettler: Send + Sync {
    async fn settle(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError>;
    fn settle_sync(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError>;
    async fn exceed(&self, task_id: &str) -> Result<(), SearchOsError>;
    fn exceed_sync(&self, task_id: &str) -> Result<(), SearchOsError>;
    async fn release(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError>;
    fn release_sync(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError>;
}

/// Default no-op handle for tests and in-memory adapters.
#[derive(Debug, Default)]
pub struct NoopBudgetSettler;

#[async_trait]
impl BudgetSettler for NoopBudgetSettler {
    async fn settle(&self, _: &str, _: i64) -> Result<(), SearchOsError> {
        Ok(())
    }
    fn settle_sync(&self, _: &str, _: i64) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn exceed(&self, _: &str) -> Result<(), SearchOsError> {
        Ok(())
    }
    fn exceed_sync(&self, _: &str) -> Result<(), SearchOsError> {
        Ok(())
    }
    async fn release(&self, _: &str, _: i64) -> Result<(), SearchOsError> {
        Ok(())
    }
    fn release_sync(&self, _: &str, _: i64) -> Result<(), SearchOsError> {
        Ok(())
    }
}

/// RAII guard: every reserve reaches settled / exceeded / released, including
/// `?`, early return, cancellation and panic (Part 30 INV-30-03).
#[must_use = "budget reservations must be settled"]
pub struct BudgetGuard {
    handle: Arc<dyn BudgetSettler>,
    task_id: String,
    spent_micros: i64,
    terminal: bool,
}

impl BudgetGuard {
    pub fn new(handle: Arc<dyn BudgetSettler>, task_id: &str) -> Self {
        Self {
            handle,
            task_id: task_id.to_string(),
            spent_micros: 0,
            terminal: false,
        }
    }

    pub fn set_spent(&mut self, spent_micros: i64) {
        self.spent_micros = spent_micros;
    }

    pub async fn settle(mut self, spent_micros: i64) -> Result<(), SearchOsError> {
        self.terminal = true;
        let result = self.handle.settle(&self.task_id, spent_micros).await;
        if result.is_err() {
            let _ = self.handle.settle_sync(&self.task_id, spent_micros);
        }
        result
    }

    pub async fn exceed(mut self) -> Result<(), SearchOsError> {
        self.terminal = true;
        let result = self.handle.exceed(&self.task_id).await;
        if result.is_err() {
            let _ = self.handle.exceed_sync(&self.task_id);
        }
        result
    }

    pub async fn release(mut self) -> Result<(), SearchOsError> {
        self.terminal = true;
        let result = self.handle.release(&self.task_id, self.spent_micros).await;
        if result.is_err() {
            let _ = self.handle.release_sync(&self.task_id, self.spent_micros);
        }
        result
    }
}

impl Drop for BudgetGuard {
    fn drop(&mut self) {
        if !self.terminal {
            let _ = self.handle.settle_sync(&self.task_id, self.spent_micros);
        }
    }
}

/// External execution error that carries already-incurred spend
/// (Part 30 INV-30-06).
#[derive(Debug, Clone)]
pub struct ExternalExecutionError {
    pub spent_micros: i64,
    pub error_code: String,
    pub message: String,
}

impl std::fmt::Display for ExternalExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.error_code, self.message)
    }
}

impl From<SearchOsError> for ExternalExecutionError {
    fn from(e: SearchOsError) -> Self {
        Self {
            spent_micros: 0,
            error_code: e.class().code.to_ascii_lowercase(),
            message: e.to_string(),
        }
    }
}

/// Everything the search service needs, injected by the runtime.
/// `Clone` lets RFC-075 policy simulation build an isolated service with one
/// overridden policy while sharing all the (Arc) state behind it.
#[allow(clippy::too_many_arguments)]
#[derive(Clone)]
pub struct SearchDependencies {
    pub profiles: ProfileRegistry,
    pub quality: QualityPolicy,
    pub freshness: FreshnessPolicy,
    pub source: SourcePolicy,
    pub cost_config: CostModelConfig,
    pub bm25: Arc<dyn SearchIndex>,
    pub vector: Option<Arc<dyn VectorIndex>>,
    pub hybrid: Option<Arc<HybridRetriever>>,
    pub embedder: Arc<dyn EmbeddingProvider>,
    pub canonical_cache: Arc<CanonicalQueryCache>,
    pub semantic_cache: Arc<SemanticCache>,
    pub retrieval_cache: Arc<RetrievalCache>,
    pub embedding_cache: Arc<EmbeddingCache>,
    pub rerank_cache: Arc<RerankCache>,
    pub router: Arc<dyn ProviderRouter>,
    pub search_providers: Vec<Arc<dyn SearchProvider>>,
    pub local_port: Arc<dyn LocalKnowledgePort>,
    pub traces: Arc<dyn TraceCollector>,
    pub cost_sink: Arc<dyn CostEventSink>,
    pub task_recorder: Arc<dyn TaskRecorder>,
    pub singleflight: Arc<SingleflightCoordinator>,
    pub cache: Arc<ExactQueryCache>,
    /// RFC-077: hit-time evidence validity. `None` skips the checks (tests
    /// and harnesses that never mutate documents behind the caches).
    pub validity: Option<std::sync::Arc<dyn crate::validity::EvidenceValidityPort>>,
    /// RFC-034: live policy state. `None` keeps the build-time policies.
    pub policy_state: Option<crate::policy::SharedPolicyState>,
    /// RFC-058: entity alias lookup for result annotation.
    pub entity_lookup: Option<std::sync::Arc<dyn crate::entity::EntityLookup>>,
    /// RFC-031 D10: negative-result cache for terminal external failures.
    /// A zero-TTL cache effectively disables the feature.
    pub negative_cache: Arc<ExactQueryCache>,
    /// RFC-072: where query expansion may add queries
    /// (`followup` | `initial` | `both`).
    pub expansion_mode: String,
    /// Config-driven semantic dedup / incremental retrieval switches.
    pub semantic_dedup: bool,
    pub incremental: bool,
    /// Intelligence section: query expansion + rerank cache gate.
    pub query_expansion: bool,
    pub reranking: bool,
    /// When false, external execution is refused (offline mode).
    pub allow_external: bool,
    /// RFC-031 external evidence promotion. `None` = promotion disabled.
    pub promotion: Option<Arc<dyn EvidencePromotionService>>,
    /// Bounded in-process agent session store. `None` disables turn
    /// recording; the runtime wires one so MCP/gateway share it.
    pub session_store: Option<Arc<crate::session::SessionStore>>,
    /// RFC-062: the federated source registry. `None` disables federation
    /// entirely, which is what every test harness and older wiring gets.
    pub federation_registry: Option<Arc<searchos_federated::InMemorySourceRegistry>>,
    /// `legacy` | `plan_only` | `execute`. Only `execute` lets the kernel run
    /// sources; `legacy` (the default) keeps the existing branches
    /// authoritative so behaviour cannot drift by accident.
    pub federation_strategy: String,
    /// Fan-out / deadline caps resolved from `config.federation`.
    pub federation_limits: searchos_domain::FederationLimits,
    /// RFC-080 source ordering: `reliability_first` (default) | `cost_adaptive`.
    pub federation_selection: String,
    /// RFC-080 result-level learning. `None` disables learning (tests and
    /// harnesses that do not wire a judge).
    pub quality_learner: Option<Arc<crate::quality_learning::QualityLearner>>,
    /// RFC-037: derive cross-lingual variants and fuse them as EXTRA recall.
    pub cross_lingual: bool,
}

/// Federation modes recognised by `federation_strategy`.
pub const FEDERATION_LEGACY: &str = "legacy";
pub const FEDERATION_PLAN_ONLY: &str = "plan_only";
pub const FEDERATION_EXECUTE: &str = "execute";

/// Emits RFC-062 §16 plan telemetry. Called by every planner call site so
/// `/search/plan` and the live pipeline cannot drift apart in what they
/// count. Counters only: no per-source labels (the registry is a
/// low-cardinality invariant map by design).
fn record_federation_plan(plan: &searchos_federated::FederatedSearchPlan) {
    let metrics = searchos_observability::MetricsRegistry::global();
    metrics.inc_counter("federation.plan.total");
    for _ in 0..plan.task_count() {
        metrics.inc_counter("federation.plan.sources_selected");
    }
    for _ in &plan.rejections {
        metrics.inc_counter("federation.plan.sources_rejected");
    }
}

/// Emits RFC-062 §16 execution telemetry from one execution report.
fn record_federation_execution(report: &searchos_federated::ExecutionReport) {
    let metrics = searchos_observability::MetricsRegistry::global();
    for result in &report.results {
        metrics.inc_counter("federation.source.executed_total");
        if result.status.is_failure() {
            metrics.inc_counter("federation.source.failed_total");
        }
        if result.timed_out {
            metrics.inc_counter("federation.source.timeout_total");
        }
        for _ in result.candidates() {
            metrics.inc_counter("federation.source.candidates_total");
        }
    }
    for _ in &report.not_executed {
        metrics.inc_counter("federation.source.not_executed_total");
    }
}

/// The configured strategy, normalized. Anything unrecognized behaves as
/// `legacy`: an unset or misspelled knob must never accidentally route real
/// traffic through a new execution path.
///
/// `execute` is accepted as a configuration value but deliberately behaves
/// as `plan_only` until the reserve/billing-aware executor migration lands:
/// the federated executor would call providers outside the pipeline's
/// reserve → execute → settle lifecycle, which would double-bill. An
/// operator who opts in gets a logged warning, not a silent no-op.
fn normalized_federation_strategy(strategy: &str) -> &'static str {
    match strategy {
        FEDERATION_PLAN_ONLY => FEDERATION_PLAN_ONLY,
        FEDERATION_EXECUTE => FEDERATION_EXECUTE,
        _ => FEDERATION_LEGACY,
    }
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
    /// Atomically claims a request id before execution starts.
    async fn idempotency_begin(
        &self,
        _request_id: &str,
        _task_id: &str,
        _workspace_id: &str,
    ) -> Result<IdempotencyClaim, SearchOsError> {
        Ok(IdempotencyClaim::Claimed)
    }
    /// Durable status of an in-flight / failed claim.
    async fn idempotency_status(&self, _request_id: &str) -> Result<Option<String>, SearchOsError> {
        Ok(None)
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

    /// Synchronous-capable settlement handle for the RAII guard.
    fn budget_guard_handle(&self) -> Arc<dyn BudgetSettler> {
        Arc::new(NoopBudgetSettler)
    }

    // --- RFC-004 v1.1 15 / Part 30 INV-30-03: reserve -> execute -> settle --
    async fn reserve_budget(
        &self,
        task_id: &str,
        _trace_id: &str,
        _estimated_micros: i64,
        _currency: &str,
    ) -> Result<BudgetGuard, SearchOsError> {
        Ok(BudgetGuard::new(self.budget_guard_handle(), task_id))
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

    /// Plan-only report (no external call, no billing). Adapters that do not
    /// support planning return an explicit error instead of a fake plan.
    async fn plan(&self, _request: &SearchRequest) -> Result<SearchPlanReport, SearchOsError> {
        Err(SearchOsError::Internal(
            "plan-only is not supported by this search service".into(),
        ))
    }
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

    /// Plan-only report: what WOULD be searched, without buying anything.
    /// No external call, no cost event, no document write — an agent can
    /// inspect the plan (and its estimated cost) before committing budget.
    fn federated_plan(
        &self,
        req: &SearchRequest,
        required: &[String],
        covered: &[String],
    ) -> Result<Option<searchos_federated::FederatedSearchPlan>, SearchOsError> {
        let strategy_name = normalized_federation_strategy(&self.deps.federation_strategy);
        if strategy_name == FEDERATION_LEGACY {
            return Ok(None);
        }
        // `plan_only` is a WIRING mode (plan, don't execute); the planner's
        // strategy vocabulary is auto/parallel/sequential/selective, and the
        // legacy-compatible default is Auto. `execute` plans the same way:
        // its real execution happens in `execute_external_federated`, with
        // billing settled by the service's reserve → execute → settle loop.
        let strategy = match strategy_name {
            FEDERATION_PLAN_ONLY | FEDERATION_EXECUTE => {
                searchos_federated::FederationStrategy::Auto
            }
            other => searchos_federated::FederationStrategy::parse(other)
                .map_err(|message| SearchOsError::validation("federation.strategy", message))?,
        };
        let Some(registry) = self.deps.federation_registry.as_ref() else {
            return Err(SearchOsError::Internal(
                "federation strategy requested but the source registry is not wired".into(),
            ));
        };
        let descriptors = registry.list(
            &searchos_federated::SourceFilter {
                enabled_only: true,
                ..Default::default()
            },
            &req.workspace_id,
        );
        let context = searchos_federated::PlanningContext {
            query: req.query.clone(),
            mode: req.mode,
            strategy,
            workspace_id: req.workspace_id.to_string(),
            requirements: required.to_vec(),
            covered_requirements: covered.to_vec(),
            required_requirements: required.to_vec(),
            max_sources: self.deps.federation_limits.max_sources.max(1),
            max_parallel_sources: self.deps.federation_limits.max_parallel_sources.max(1),
            max_estimated_cost_micros: req.budget.max_total_cost_micros,
            max_latency_ms: req.budget.max_execution_time_ms,
            stop_when_covered: true,
            generic_sources_can_answer: true,
            selection: self.deps.selection_mode(),
        };
        let plan = searchos_federated::FederatedPlanner::plan(&context, &descriptors);
        record_federation_plan(&plan);
        Ok(Some(plan))
    }

    pub async fn plan_report(
        &self,
        req: &SearchRequest,
    ) -> Result<SearchPlanReport, SearchOsError> {
        let deps = &self.deps;
        let analysis = analyze(&req.query);
        let required = if req.focus_requirements.is_empty() {
            required_subquestions(&analysis)
        } else {
            req.focus_requirements.clone()
        };
        let profile = deps
            .profiles
            .get(req.profile.as_str())
            .cloned()
            .ok_or_else(|| {
                SearchOsError::validation("profile", format!("unknown profile {}", req.profile))
            })?;
        let source_policy = deps.effective_source().with_constraints(
            &req.constraints.required_domains,
            &req.constraints.excluded_domains,
        );
        let freshness_label = effective_freshness_label(req, &profile);
        let fp = fingerprint(&QueryFingerprintParts {
            normalized_query: analysis.normalized.clone(),
            language: analysis.language.clone(),
            profile_version: profile.version.clone(),
            freshness_policy: deps.effective_freshness().version.clone(),
            source_policy: deps.effective_source().version.clone(),
            workspace_scope: req.workspace_id.to_string(),
            mode: req.mode.as_str().to_string(),
            freshness_requirement: freshness_label.clone(),
        });
        let task_id = TaskId::new("plan");
        let hits = retrieve_local_candidates(
            deps,
            req,
            &analysis,
            &profile,
            &source_policy,
            &fp,
            &task_id,
        )
        .await?;

        // Coverage of the required sub-questions against local knowledge.
        let covered_texts: Vec<String> = hits
            .iter()
            .map(|c| {
                format!(
                    "{} {} {}",
                    c.title.as_deref().unwrap_or(""),
                    c.url,
                    c.snippet
                )
            })
            .collect();
        let needs = searchos_decision::GapAnalyzer::analyze(
            &required,
            &covered_texts,
            &req.constraints.required_domains,
            false,
            false,
        );
        let federated = self.federated_plan(req, &required, &[])?;
        let estimator = CostEstimator::from_config(&deps.cost_config);
        let provider_id = deps
            .router
            .route(true, req.budget.max_total_cost_micros)
            .await
            .ok()
            .and_then(|sel| sel.first().map(|e| e.id.clone()))
            .unwrap_or_else(|| "tavily".to_string());
        let per_query_cost = estimator.estimate_request(&provider_id).amount_micros;
        let plan = searchos_decision::build_external_plan(
            &needs,
            req.request_id.as_str(),
            req.budget.max_external_queries,
            req.budget.max_total_cost_micros,
            per_query_cost,
            &req.constraints.required_domains,
            &req.constraints.excluded_domains,
        );
        let open_requirements: Vec<searchos_domain::decision::CoverageGap> = needs
            .iter()
            .filter(|n| n.is_open())
            .map(|n| searchos_domain::decision::CoverageGap {
                question: n.description.clone(),
                missing_kind: "evidence".into(),
                status: n.status,
                priority: n.priority,
                strategy: n.strategy,
                requirement_id: n.id.clone(),
            })
            .collect();
        Ok(SearchPlanReport {
            query: req.query.clone(),
            mode: req.mode.as_str().to_string(),
            decision_path: if req.mode.allows_local() {
                "local_first"
            } else {
                "external"
            }
            .into(),
            decision_reason: "plan_only".into(),
            local_candidates: hits.len(),
            required_requirements: required,
            open_requirements,
            planned_queries: plan
                .queries
                .iter()
                .map(|q| PlannedQueryReport {
                    query: q.query.clone(),
                    requirement_id: q.requirement_id.clone(),
                    strategy: format!("{:?}", q.strategy),
                })
                .collect(),
            estimated_cost_micros: plan.estimated_cost_micros,
            currency: req.budget.currency.clone(),
            federation_strategy: federated.as_ref().map(|p| p.strategy.as_str().to_string()),
            federated_sources: federated
                .as_ref()
                .map(|p| p.selected_source_ids())
                .unwrap_or_default(),
            source_selection: federated.as_ref().map(|p| p.selection.as_str().to_string()),
            source_scores: federated
                .as_ref()
                .map(|p| p.selection_scores.clone())
                .unwrap_or_default(),
        })
    }
}

/// One planned external query in a plan-only report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlannedQueryReport {
    pub query: String,
    pub requirement_id: String,
    pub strategy: String,
}

/// Plan-only report returned to agents before any budget is spent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchPlanReport {
    pub query: String,
    pub mode: String,
    pub decision_path: String,
    pub decision_reason: String,
    pub local_candidates: usize,
    pub required_requirements: Vec<String>,
    pub open_requirements: Vec<searchos_domain::decision::CoverageGap>,
    pub planned_queries: Vec<PlannedQueryReport>,
    pub estimated_cost_micros: i64,
    pub currency: String,
    /// RFC-062 plan metadata. `None` means the default legacy pipeline is
    /// authoritative for this request.
    #[serde(default)]
    pub federation_strategy: Option<String>,
    #[serde(default)]
    pub federated_sources: Vec<String>,
    /// RFC-080 ordering policy actually used by the planner.
    #[serde(default)]
    pub source_selection: Option<String>,
    /// RFC-080 per-source score for every eligible source (audit trail).
    #[serde(default)]
    pub source_scores: Vec<searchos_federated::SourceSelectionScore>,
}

/// Builds the external query plan: one targeted query per open requirement,
/// plus config-driven expansion, deduplicated and capped (RFC-031 D5/D9).
///
/// The returned list is *the* plan: it is what the reservation is sized from
/// and what gets executed, so the two can never diverge.
fn plan_external_queries(
    deps: &Arc<SearchDependencies>,
    req: &SearchRequest,
    decision: &SearchDecision,
    per_query_cost: i64,
    required_domains: &[String],
    excluded_domains: &[String],
    affordable_external_queries: u32,
) -> Vec<PlannedQuery> {
    let gaps: Vec<CoverageGap> = match decision {
        SearchDecision::Hybrid {
            missing_requirements,
            ..
        } => missing_requirements.clone(),
        _ => Vec::new(),
    };
    // Gap-Based Search: each external query is linked to one open information
    // requirement, and covered needs are never re-bought (RFC-004 v1.1 6.8/10).
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
    // RFC-076: the plan fits what the budget can afford. The trace records
    // the downsize; the plan enforces it (belt and braces with the cost cap
    // inside build_external_plan).
    let max_queries = req
        .budget
        .max_external_queries
        .min(affordable_external_queries);
    let external_plan = build_external_plan(
        &needs,
        req.request_id.as_str(),
        max_queries,
        req.budget.max_total_cost_micros,
        per_query_cost,
        required_domains,
        excluded_domains,
    );
    let mut planned: Vec<PlannedQuery> = external_plan
        .queries
        .iter()
        .map(|q| PlannedQuery {
            query: q.query.clone(),
            origin: QueryOrigin::Gap,
            requirement_id: Some(q.requirement_id.clone()),
            strategy: Some(q.strategy),
        })
        .collect();
    if planned.is_empty() {
        // ExternalOnly / ExternalFirst carry no missing_requirements; the
        // purchase is still planned (and attributed after acquisition).
        planned.push(PlannedQuery {
            query: req.query.clone(),
            origin: QueryOrigin::Fallback,
            requirement_id: None,
            strategy: None,
        });
    }
    // RFC-072: the MODE decides where expansion may add queries.
    // `followup` (default) expands only on a proven coverage gap, so the
    // first plan never spends extra; `initial` / `both` may also add
    // synonym / sub-question variants to the FIRST plan. Variants compete
    // for the SAME affordable query budget (RFC-076), never exceeding it.
    let followup_expansion_allowed = matches!(deps.expansion_mode.as_str(), "followup" | "both");
    let initial_expansion_allowed = matches!(deps.expansion_mode.as_str(), "initial" | "both");
    if deps.query_expansion && followup_expansion_allowed && !gaps.is_empty() {
        let expander = QueryExpander::new(ExpansionConfig {
            enabled: true,
            require_coverage_gap: true,
            ..Default::default()
        });
        let gap_question = gaps.first().map(|g| g.question.clone()).unwrap_or_default();
        let requirement_id = gaps.first().map(|g| g.requirement_id.clone());
        for extra in expander.expand(&req.query, &gap_question) {
            if !planned.iter().any(|p| p.query == extra) {
                planned.push(PlannedQuery {
                    query: extra,
                    origin: QueryOrigin::Expansion,
                    requirement_id: requirement_id.clone(),
                    strategy: None,
                });
            }
        }
    }
    // RFC-072: synonym / sub-question variants fill the REMAINING budget
    // slots (gap queries win). Bounded by the affordable cap and by the
    // request's expansion budget.
    if deps.query_expansion && initial_expansion_allowed && planned.len() < max_queries as usize {
        let mut variants: Vec<String> = Vec::new();
        variants.extend(searchos_query::expansion::synonym_variants(&req.query, 2));
        variants.extend(searchos_query::expansion::sub_question_variants(
            &req.query, 2,
        ));
        // RFC-037: a cross-language variant competes for the SAME affordable
        // budget as every other expansion, never an extra purchase.
        if deps.cross_lingual {
            variants.extend(searchos_query::crosslingual::cross_lingual_variants(
                &req.query, 2,
            ));
        }
        for variant in variants {
            if planned.len() >= max_queries as usize {
                break;
            }
            if planned
                .iter()
                .any(|p| p.query.eq_ignore_ascii_case(&variant))
            {
                continue;
            }
            planned.push(PlannedQuery {
                query: variant,
                origin: QueryOrigin::Expansion,
                requirement_id: None,
                strategy: None,
            });
        }
    }

    planned.truncate(req.budget.max_external_queries.max(1) as usize);
    planned
}

/// Plans a bounded follow-up round from the FINAL gate's open requirements.
///
/// Unlike the initial plan (which may expand a proven gap), a follow-up is
/// strictly one targeted query per still-open requirement, and it never
/// repeats a query already bought. Costs and query caps remain cumulative
/// through [`AcquisitionState`] (RFC-004 v1.1 6.8, INV-30-04).
fn plan_follow_up_queries(
    gaps: &[searchos_domain::decision::CoverageGap],
    state: &AcquisitionState,
    remaining: usize,
    base_query: &str,
) -> Vec<PlannedQuery> {
    // A follow-up must REFORMULATE, not repeat: the initial plan already
    // bought one query per gap. `QueryExpander` produces the documented
    // reformulation family (base+gap, gap alone, official/documentation/
    // release-notes variants); anything already bought is filtered out by
    // `executed_queries`, so a covered requirement is never re-purchased.
    let expander = QueryExpander::new(ExpansionConfig {
        enabled: true,
        require_coverage_gap: true,
        max_expansions: 5,
        ..Default::default()
    });
    let mut seen: std::collections::HashSet<String> = state.executed_queries.clone();
    let mut out = Vec::new();
    for gap in gaps {
        for query in expander.expand(base_query, &gap.question) {
            let key = query.to_lowercase();
            if query.trim().is_empty() || !seen.insert(key) {
                continue;
            }
            out.push(PlannedQuery {
                query,
                origin: QueryOrigin::Expansion,
                requirement_id: Some(gap.requirement_id.clone()),
                strategy: Some(gap.strategy),
            });
            if out.len() >= remaining {
                return out;
            }
        }
    }
    out
}

/// Whether a second acquisition round is justified. A conflict is
/// unresolvable by more search, a passed gate is done, and a local-only
/// decision must never buy anything (INV-30-01).
fn should_replan(
    decision: &SearchDecision,
    verdict: &searchos_decision::QualityVerdict,
    req: &SearchRequest,
    state: &AcquisitionState,
    round: u32,
) -> bool {
    round < req.budget.max_acquisition_rounds
        && decision.requires_external()
        && !verdict.passed()
        && verdict.status != QualityStatus::Conflict
        && verdict.conflicts.is_empty()
        && !verdict.coverage_gaps.is_empty()
        && state.remaining_queries(req) > 0
        && state.spent_micros < req.budget.max_total_cost_micros
}

/// Attributes each acquired external item to the final quality gate verdict
/// and queues promotion for the accepted ones (RFC-031 D2/D9).
///
/// Evidence the gate rejected is recorded as `rejected` and **never** indexed.
#[allow(clippy::too_many_arguments)]
async fn promote_external_evidence(
    deps: &Arc<SearchDependencies>,
    req: &SearchRequest,
    task_id: &TaskId,
    outcomes: &[ExternalQueryOutcome],
    merged_evidence: &[Evidence],
    verdict: &searchos_decision::QualityVerdict,
) {
    let Some(promotion) = &deps.promotion else {
        return;
    };
    if outcomes.is_empty() {
        return;
    }
    use searchos_document::canonical::canonical_url;
    let accepted: std::collections::HashSet<&str> = verdict
        .accepted_indices
        .iter()
        .filter_map(|i| merged_evidence.get(*i))
        .map(|e| e.url.as_str())
        .collect();
    // An unresolved conflict poisons the whole acquisition: we cannot tell
    // which side is right, so nothing from it becomes knowledge.
    let has_conflict = !verdict.conflicts.is_empty();
    let now = Utc::now();
    for outcome in outcomes {
        let mut items: Vec<EvidenceItem> = Vec::new();
        for item in &outcome.items {
            let canonical = canonical_url(&item.url);
            // The item only counts if its evidence survived the merge; a
            // duplicate of existing knowledge has nothing new to promote.
            let present = merged_evidence.iter().any(|e| e.url == canonical);
            let (state, reason) = if has_conflict {
                (
                    PromotionState::Rejected,
                    Some("unresolved_conflict".to_string()),
                )
            } else if !present {
                // Duplicate of existing knowledge: nothing new to promote.
                (PromotionState::Pending, None)
            } else if accepted.contains(canonical.as_str()) {
                (PromotionState::Accepted, None)
            } else {
                (
                    PromotionState::Rejected,
                    Some("final_gate_rejected".to_string()),
                )
            };
            items.push(EvidenceItem {
                id: searchos_domain::new_id("evi"),
                acquisition_id: String::new(),
                url: item.url.clone(),
                canonical_url: canonical,
                title: item.title.clone(),
                provider_score: item.score,
                relevance: item.score,
                state,
                document_id: None,
                version_id: None,
                chunk_count: None,
                reason,
                promoted_at: None,
                created_at: now,
            });
        }
        if items.is_empty() {
            continue;
        }
        let (acquisition, items) = build_acquisition(
            task_id,
            req.request_id.as_str(),
            &req.workspace_id,
            &outcome.provider_id,
            &outcome.query,
            outcome.origin,
            outcome.requirement_id.clone(),
            outcome
                .strategy
                .map(|s| serde_json::to_string(&s).unwrap_or_default()),
            now,
            items,
        );
        if let Err(e) = promotion.record_and_queue(acquisition, items).await {
            tracing::warn!(code = %e.class().code, "evidence promotion record failed");
        }
    }
}

/// Records reuse events when a local resolution served external-sourced
/// knowledge (RFC-031 5.6).
async fn record_reuse_if_external_sourced(
    deps: &Arc<SearchDependencies>,
    req: &SearchRequest,
    task_id: &TaskId,
    servable_hits: &[RetrievalCandidate],
    merged_evidence: &[Evidence],
    verdict: &searchos_decision::QualityVerdict,
) {
    let Some(promotion) = &deps.promotion else {
        return;
    };
    use searchos_document::canonical::canonical_url;
    let accepted: std::collections::HashSet<&str> = verdict
        .accepted_indices
        .iter()
        .filter_map(|i| merged_evidence.get(*i))
        .map(|e| e.url.as_str())
        .collect();
    let mut ids: Vec<String> = servable_hits
        .iter()
        .filter(|c| accepted.contains(canonical_url(&c.url).as_str()))
        .map(|c| c.document_id.to_string())
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return;
    }
    let Ok(promoted) = promotion.promoted_document_ids(&ids).await else {
        return;
    };
    for id in ids.iter().filter(|id| promoted.contains(*id)) {
        if let Err(e) = promotion.record_reuse(task_id, &req.workspace_id, id).await {
            tracing::warn!(code = %e.class().code, "reuse event write failed");
        }
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

/// Stable cache / semantic-compatibility label for the effective freshness
/// requirement (request constraint + profile default).
fn effective_freshness_label(req: &SearchRequest, profile: &SearchProfile) -> String {
    // RFC-057: a historical view must never share a cache entry with a
    // wall-clock view (or with a different as-of instant).
    match req.constraints.as_of {
        Some(as_of) => format!(
            "{}|{}|asof:{}",
            req.constraints.freshness.cache_label(),
            profile.freshness.cache_label(),
            as_of.to_rfc3339()
        ),
        None => format!(
            "{}|{}",
            req.constraints.freshness.cache_label(),
            profile.freshness.cache_label()
        ),
    }
}

/// The instant freshness is judged at: the caller's as-of point when given,
/// otherwise wall-clock now (RFC-057).
fn freshness_now(req: &SearchRequest) -> chrono::DateTime<chrono::Utc> {
    req.constraints.as_of.unwrap_or_else(chrono::Utc::now)
}

/// Seven-dimensional semantic reuse gate (RFC-007 7.4, G-02).
fn semantic_compatibility(
    deps: &SearchDependencies,
    req: &SearchRequest,
    analysis: &QueryAnalysis,
    source_policy: &SourcePolicy,
    freshness_label: &str,
) -> SemanticCompatibility {
    SemanticCompatibility {
        intents: analysis.intents.clone(),
        language: analysis.language.clone(),
        workspace_scope: req.workspace_id.to_string(),
        freshness_policy: deps.effective_freshness().version.clone(),
        source_policy: source_policy.version.clone(),
        freshness_requirement: freshness_label.to_string(),
        mode: req.mode.as_str().to_string(),
        coverage_requirement: req.constraints.minimum_coverage,
        // Candidate sets are not answers: validity is explicit, not implied.
        validity_status: "candidate_set".into(),
    }
}

fn vector_bucket(vector: &[f32]) -> String {
    vector
        .iter()
        .take(8)
        .map(|x| format!("{x:+.1}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Embeds one text through the configured provider, reusing the embedding
/// cache on model id / version / content hash.
async fn embed_text(deps: &SearchDependencies, text: &str) -> Result<Vec<f32>, SearchOsError> {
    let model_id = deps.embedder.model_id();
    let model_version = deps.embedder.model_version();
    let hash = content_hash(text.as_bytes());
    if let Some(cached) = deps.embedding_cache.get(model_id, model_version, &hash) {
        return Ok((*cached).clone());
    }
    let texts = vec![text.to_string()];
    let mut vectors = deps
        .embedder
        .embed(&texts)
        .await
        .map_err(|e| SearchOsError::Retrieval(format!("embed query: {e}")))?;
    let vector = vectors
        .pop()
        .ok_or_else(|| SearchOsError::Retrieval("empty embedding".into()))?;
    deps.embedding_cache
        .put(model_id, model_version, &hash, vector.clone());
    Ok(vector)
}

/// Computes the semantic bucket and the query embedding from the
/// configured provider. Both are returned because the bucket alone is only
/// a coarse recall hint — the similarity gate needs the vector.
async fn query_embedding_and_bucket(
    deps: &SearchDependencies,
    text: &str,
) -> Result<(String, Vec<f32>), SearchOsError> {
    let vector = embed_text(deps, text).await?;
    Ok((vector_bucket(&vector), vector))
}

/// Retrieval chain: Canonical / Semantic / Retrieval caches → BM25 + Vector
/// → metadata filtering → RRF → optional rerank → evidence candidates.
async fn retrieve_local_candidates(
    deps: &Arc<SearchDependencies>,
    req: &SearchRequest,
    analysis: &QueryAnalysis,
    profile: &SearchProfile,
    source_policy: &SourcePolicy,
    fp: &str,
    task_id: &TaskId,
) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
    let metrics = searchos_observability::MetricsRegistry::global();
    let top_k = clamp_max_results(req.max_results.max(10)) as usize;
    // Candidate pool (RFC-007 7.1): freshness/source filtering happens at
    // the application layer AFTER retrieval, so the index must return more
    // candidates than the response cap — otherwise stale documents ranked
    // above fresh ones crowd the fresh ones out of the truncated candidate
    // window entirely (verified: 12 stale + 1 fresh, strict freshness →
    // fresh doc absent from a top-k=10 pool).
    let pool_k = top_k.saturating_mul(4).clamp(top_k, 200);
    let freshness_label = effective_freshness_label(req, profile);
    let policy_version = format!(
        "{}|{}|{}|{}",
        profile.version,
        deps.effective_freshness().version,
        source_policy.version,
        deps.policy_stamp()
    );
    let compatibility =
        semantic_compatibility(deps, req, analysis, source_policy, &freshness_label);
    let (bucket, query_vector) = query_embedding_and_bucket(deps, &analysis.normalized).await?;
    let semantic_key = format!("{bucket}|{}|{}", req.mode.as_str(), freshness_label);

    if deps.semantic_dedup {
        if let Some(cached) = deps.semantic_cache.get(
            &semantic_key,
            &compatibility,
            &policy_version,
            &query_vector,
        ) {
            if validity_gate_allows(deps, &cached).await {
                metrics.inc_semantic_reuse();
                return Ok(cached);
            }
            metrics.inc_counter("cache.validity_reject_total");
        }
    }
    if let Some(cached) = deps.canonical_cache.get(
        fp,
        &analysis.language,
        req.workspace_id.as_str(),
        &policy_version,
    ) {
        if validity_gate_allows(deps, &cached).await {
            return Ok(cached);
        }
        metrics.inc_counter("cache.validity_reject_total");
    }
    if deps.incremental {
        if let Some(cached) = deps.retrieval_cache.get(fp, pool_k, &policy_version) {
            if validity_gate_allows(deps, &cached).await {
                return Ok(cached);
            }
            metrics.inc_counter("cache.validity_reject_total");
        }
    }

    let mut query = RetrievalQuery::new(&analysis.normalized, pool_k);
    query.workspace_id = Some(req.workspace_id.to_string());
    query.language = req.constraints.language.clone();
    query.required_domains = req.constraints.required_domains.clone();
    query.excluded_domains = req.constraints.excluded_domains.clone();
    // Keep stale candidates available for the refresh path; application-level
    // filtering decides what is servable (Part 30 INV-30-01).
    query.include_stale = true;
    let fetched_after = match req.constraints.freshness {
        FreshnessRequirement::Strict => Some(freshness_now(req) - chrono::Duration::days(1)),
        FreshnessRequirement::Recent => Some(freshness_now(req) - chrono::Duration::days(30)),
        FreshnessRequirement::MaximumAgeSeconds(seconds) => Some(
            freshness_now(req) - chrono::Duration::seconds(seconds.min(i64::MAX as u64) as i64),
        ),
        FreshnessRequirement::Any => None,
    };
    query.fetched_after = fetched_after;

    let mut candidates = match &deps.hybrid {
        Some(hybrid) => hybrid.retrieve(&query).await?,
        None => {
            deps.local_port
                .retrieve(&analysis.normalized, top_k, Some(req.workspace_id.as_str()))
                .await?
        }
    };
    // RFC-037: a Chinese query cannot BM25-match an English document that
    // describes the same concept. Try ONE translated recall list and fuse it
    // with the original — a variant is an ADDITION, never a substitution, so
    // results the original already found cannot be pushed out by it.
    if deps.cross_lingual {
        if let Some(translated) =
            searchos_query::crosslingual::full_cross_lingual_variant(&analysis.normalized)
        {
            if translated != analysis.normalized {
                let mut translated_query = RetrievalQuery::new(&translated, pool_k);
                translated_query.workspace_id = query.workspace_id.clone();
                translated_query.language = query.language.clone();
                translated_query.required_domains = query.required_domains.clone();
                translated_query.excluded_domains = query.excluded_domains.clone();
                translated_query.include_stale = true;
                translated_query.fetched_after = fetched_after;
                let extra = match &deps.hybrid {
                    Some(hybrid) => hybrid.retrieve(&translated_query).await?,
                    None => {
                        deps.local_port
                            .retrieve(&translated, top_k, Some(req.workspace_id.as_str()))
                            .await?
                    }
                };
                if !extra.is_empty() {
                    let config = searchos_retrieval::RrfConfig::default();
                    candidates = searchos_retrieval::rrf_fuse_weighted(
                        &[
                            searchos_retrieval::RankedSource::new("original", 1.0, &candidates),
                            searchos_retrieval::RankedSource::new("cross_lingual", 1.0, &extra),
                        ],
                        &config,
                    );
                }
            }
        }
    }
    let ids: Vec<&str> = candidates.iter().map(|c| c.chunk_id.as_str()).collect();
    if deps.reranking {
        // RFC-007 7.7: a real reranker may have reordered the fused set.
        // Its cost is billed through the same ledger as every other stage
        // (estimates stay estimates; `is_estimate` is never rewritten).
        if candidates
            .iter()
            .any(|c| c.source == RetrievalSource::Reranked)
        {
            let estimator = CostEstimator::from_config(&deps.cost_config);
            let cost =
                estimator.estimate_cpu_millis(2u64.saturating_mul(candidates.len() as u64).max(1));
            let _ = deps
                .cost_sink
                .append(CostEvent {
                    id: searchos_domain::ids::CostEventId::generate(),
                    task_id: task_id.clone(),
                    workspace_id: req.workspace_id.clone(),
                    agent_id: req.agent_id.clone(),
                    operation: CostOperation::Reranking,
                    provider_id: Some("reranker".into()),
                    input_units: candidates.len() as u64,
                    output_units: candidates.len() as u64,
                    estimated_cost: cost,
                    actual_cost: None,
                    duration_ms: 0,
                    occurred_at: Utc::now(),
                    trace_id: None,
                    shared: false,
                    pricing_model_version: "1".into(),
                    status: Default::default(),
                    error_code: None,
                })
                .await;
        }
        if let Some(order) = deps.rerank_cache.get("rrf", &ids, &policy_version) {
            candidates = order;
        } else {
            deps.rerank_cache
                .put("rrf", &ids, &policy_version, candidates.clone());
        }
    }

    deps.canonical_cache.put(
        fp,
        &analysis.language,
        req.workspace_id.as_str(),
        candidates.clone(),
        &policy_version,
    );
    deps.semantic_cache.put(
        &semantic_key,
        candidates.clone(),
        compatibility,
        &policy_version,
        query_vector,
    );
    if deps.incremental {
        deps.retrieval_cache
            .put(fp, top_k, &policy_version, candidates.clone());
    }
    Ok(candidates)
}

/// RFC-077 hit-time validity for a candidate list: `true` when there is no
/// port wired (all existing behaviour) or every cited document still exists
/// at the SAME version; a port error is fail-open.
async fn validity_gate_allows(
    deps: &Arc<SearchDependencies>,
    candidates: &[RetrievalCandidate],
) -> bool {
    match deps.validity.as_ref() {
        None => true,
        Some(port) => crate::validity::candidates_still_valid(port.as_ref(), candidates).await,
    }
}

/// RFC-077 hit-time validity for a cached exact-response payload.
async fn validity_gate_allows_payload(
    deps: &SearchDependencies,
    payload: &serde_json::Value,
) -> bool {
    match deps.validity.as_ref() {
        None => true,
        Some(port) => crate::validity::payload_still_valid(port.as_ref(), payload).await,
    }
}

/// RFC-071: records a session turn + accumulates its evidence into the
/// research workspace. Shared by the full pipeline and the exact-cache hit
/// path so a cache-served continuation still advances the workspace.
fn record_session_turn(
    store: &Arc<crate::session::SessionStore>,
    req: &SearchRequest,
    response: &mut SearchResponse,
    decision_path: &str,
    coverage_gaps: &[searchos_domain::decision::CoverageGap],
) {
    let Some(session_id) = req.session_id.as_deref() else {
        return;
    };
    store.create_or_get(session_id, req.workspace_id.as_str());
    let provenance_by_url: std::collections::HashMap<&str, &Evidence> = response
        .provenance
        .iter()
        .map(|p| (p.url.as_str(), p))
        .collect();
    let evidence: Vec<crate::session::WorkspaceEvidence> = response
        .results
        .iter()
        .map(|r| {
            let provenance = provenance_by_url.get(r.url.as_str());
            crate::session::WorkspaceEvidence {
                url: r.url.clone(),
                title: r.title.clone(),
                source: r.source.clone(),
                score: r.score,
                trace_id: response.trace_id.to_string(),
                recorded_at: Utc::now().to_rfc3339(),
                document_id: provenance
                    .and_then(|p| p.provenance.document_id.as_ref())
                    .map(|d| d.to_string()),
                document_version_id: provenance
                    .and_then(|p| p.document_version_id.as_ref())
                    .map(|v| v.to_string()),
            }
        })
        .collect();
    let turn = crate::session::SessionTurn {
        query: req.query.clone(),
        trace_id: response.trace_id.clone(),
        status: response.status.as_str().to_string(),
        decision_path: decision_path.to_string(),
        open_requirements: coverage_gaps.len(),
        recorded_at: Utc::now().to_rfc3339(),
    };
    if let Some(session) = store.record_turn_with_evidence(
        session_id,
        req.workspace_id.as_str(),
        turn,
        coverage_gaps.to_vec(),
        &evidence,
    ) {
        response.session_id = Some(session.session_id);
        response.focus_requirements = session
            .open_requirements
            .iter()
            .map(|g| g.question.clone())
            .collect();
    }
}

/// RFC-071 v2: builds the carried workspace-evidence block for a session
/// continuation. Entries already in this turn's results are omitted; LOCAL
/// entries are re-validated against the durable store, so a deleted or
/// re-versioned document is dropped and COUNTED instead of re-served.
async fn carry_workspace_evidence(
    deps: &Arc<SearchDependencies>,
    req: &SearchRequest,
    results: &[searchos_domain::response::SearchResultItem],
) -> Option<searchos_domain::response::WorkspaceEvidenceBlock> {
    let store = deps.session_store.as_ref()?;
    let session_id = req.session_id.as_deref()?;
    let session = store.get(session_id)?;
    if session.evidence.is_empty() {
        return None;
    }
    let current: std::collections::HashSet<&str> = results.iter().map(|r| r.url.as_str()).collect();
    let mut considered = 0usize;
    let mut items: Vec<searchos_domain::response::WorkspaceEvidenceItem> = Vec::new();
    // Local identity per URL, snapshotted once (no borrows into the store).
    let mut local_identity: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    for entry in &session.evidence {
        if current.contains(entry.url.as_str()) {
            continue;
        }
        if let (Some(document_id), Some(version_id)) =
            (entry.document_id.clone(), entry.document_version_id.clone())
        {
            local_identity.insert(entry.url.clone(), (document_id, version_id));
        }
        considered += 1;
        items.push(searchos_domain::response::WorkspaceEvidenceItem {
            url: entry.url.clone(),
            title: entry.title.clone(),
            source: entry.source.clone(),
            score: entry.score,
            trace_id: entry.trace_id.clone(),
            recorded_at: entry.recorded_at.clone(),
        });
        if items.len() >= 20 {
            break;
        }
    }
    if items.is_empty() {
        return None;
    }
    // Validate local identities when a validity port is wired.
    let mut invalidated = 0usize;
    if let Some(port) = deps.validity.as_ref() {
        let mut valid: Vec<searchos_domain::response::WorkspaceEvidenceItem> =
            Vec::with_capacity(items.len());
        for item in items {
            let valid_entry = match local_identity.get(&item.url) {
                Some((document_id, version_id)) => {
                    let doc = searchos_domain::ids::DocumentId::new(document_id.clone());
                    match port.current_versions(std::slice::from_ref(&doc)).await {
                        Ok(current_versions) => current_versions
                            .get(&doc)
                            .map(|v| v.as_ref().map(|v| v.to_string()) == Some(version_id.clone()))
                            .unwrap_or(true),
                        // Fail-open: an unavailable port must not disable
                        // the workspace view.
                        Err(_) => true,
                    }
                }
                None => true,
            };
            if valid_entry {
                valid.push(item);
            } else {
                invalidated += 1;
            }
        }
        items = valid;
    }
    Some(searchos_domain::response::WorkspaceEvidenceBlock {
        items,
        invalidated,
        considered,
    })
}

/// RFC-080: teaches the router which source produced judged-relevant
/// results. Best-effort and never fails a request; a search that reveals no
/// judgement is simply not a learning sample.
async fn learn_source_quality(
    deps: &Arc<SearchDependencies>,
    req: &SearchRequest,
    results: &[SearchResultItem],
) {
    let Some(learner) = deps.quality_learner.as_ref() else {
        return;
    };
    let mut by_source: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for item in results {
        by_source
            .entry(item.source.as_str())
            .or_default()
            .push(item.url.clone());
    }
    for (source_id, urls) in by_source {
        learner.learn(source_id, req.query.as_str(), &urls).await;
    }
}

/// Best-effort analytics event; never fails the search request.
async fn record_analytics(
    deps: &Arc<SearchDependencies>,
    req: &SearchRequest,
    payload: &serde_json::Value,
) {
    let mut analytics_payload = payload.clone();
    analytics_payload["workspace_id"] = serde_json::Value::String(req.workspace_id.to_string());
    analytics_payload["request_id"] = serde_json::Value::String(req.request_id.to_string());
    let _ = deps
        .task_recorder
        .record_search_completed(&analytics_payload)
        .await;
}

/// Bounded singleflight timeout is explicit insufficient evidence, never a
/// false success (Part 30 §30.8).
fn singleflight_timeout_response(
    req: &SearchRequest,
    trace_id: &TraceId,
    task_id: &TaskId,
) -> SearchResponse {
    let decision = SearchDecision::Reject {
        reason: DecisionReason::ProviderUnavailable,
    };
    let warnings = vec!["singleflight_timeout".to_string()];
    build_response(
        req,
        &decision,
        Vec::new(),
        QualityStatus::Failed,
        FreshnessStatus::Unknown,
        CoverageStatus::Insufficient,
        warnings.clone(),
        Money::zero(&req.budget.currency),
        trace_id.clone(),
        Vec::new(),
        warnings,
        task_id,
        SearchStatus::InsufficientEvidence,
        None,
        None,
    )
}

/// An idempotency loser waits a bounded time for the winner's durable result.
async fn wait_for_idempotent_result(
    deps: &Arc<SearchDependencies>,
    req: &SearchRequest,
    trace_id: &TraceId,
    task_id: &TaskId,
    started: Instant,
) -> Result<SearchResponse, SearchOsError> {
    let metrics = searchos_observability::MetricsRegistry::global();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(payload) = deps
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
        match deps
            .task_recorder
            .idempotency_status(req.request_id.as_str())
            .await?
        {
            Some(status) if status == "failed" => {
                return Err(SearchOsError::Internal("idempotency leader failed".into()))
            }
            Some(status) if status == "completed" => {}
            _ => {}
        }
        if Instant::now() >= deadline {
            metrics.inc_counter("singleflight.follower_timeout_total");
            return Ok(singleflight_timeout_response(req, trace_id, task_id));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Whether a terminal failure may be negatively cached (RFC-031 D10).
///
/// Strictly limited to `insufficient_evidence` produced by an external
/// acquisition that returned nothing. A budget rejection, provider outage or
/// partial result must never be cached: those conditions change.
fn should_negative_cache(response: &SearchResponse) -> bool {
    response.status == SearchStatus::InsufficientEvidence
        && response.warnings.iter().any(|w| w == "external_no_results")
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
            score: normalize_candidate_score(c),
            published_at: None,
            source: "local".into(),
            highlight: c.highlight.clone(),
            entity_id: None,
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

impl SearchDependencies {
    /// RFC-080 selection policy. An unparsable value falls back to the
    /// historical order and is reported — never silently applied.
    pub fn selection_mode(&self) -> searchos_federated::SelectionMode {
        match searchos_federated::SelectionMode::parse(&self.federation_selection) {
            Ok(mode) => mode,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "unknown federation.selection; falling back to reliability_first"
                );
                searchos_federated::SelectionMode::default()
            }
        }
    }

    /// RFC-034: policies currently in force (hot-reloaded) or the build-time
    /// defaults. Every policy read in the pipeline goes through these.
    pub fn effective_quality(&self) -> searchos_policy::QualityPolicy {
        self.policy_state
            .as_ref()
            .map(|s| s.quality())
            .unwrap_or_else(|| self.quality.clone())
    }
    pub fn effective_freshness(&self) -> searchos_policy::FreshnessPolicy {
        self.policy_state
            .as_ref()
            .map(|s| s.freshness())
            .unwrap_or_else(|| self.freshness.clone())
    }
    pub fn effective_source(&self) -> searchos_policy::SourcePolicy {
        self.policy_state
            .as_ref()
            .map(|s| s.source())
            .unwrap_or_else(|| self.source.clone())
    }
    /// Cache-key component: a policy change invalidates cached answers.
    pub fn policy_stamp(&self) -> String {
        self.policy_state
            .as_ref()
            .map(|s| s.stamp())
            .unwrap_or_else(|| "static".into())
    }
}

#[async_trait]
impl SearchService for SearchServiceImpl {
    /// Plan-only entry point (trait object): delegates to the inherent
    /// implementation so every transport gets the same plan.
    async fn plan(&self, req: &SearchRequest) -> Result<SearchPlanReport, SearchOsError> {
        SearchServiceImpl::plan_report(self, req).await
    }

    async fn search(&self, mut req: SearchRequest) -> Result<SearchResponse, SearchOsError> {
        let default_limit = DEFAULT_MAX_RESULTS;
        req.max_results = clamp_max_results(if req.max_results == 0 {
            default_limit
        } else {
            req.max_results
        });
        let trace_id = TraceId::generate();
        let task_id = TaskId::generate();
        let started = Instant::now();
        let metrics = searchos_observability::MetricsRegistry::global();

        // A completed request id is a durable replay: no second execution,
        // bill, task row or document version (Part 30 INV-30-02).
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

        // Query intelligence + policy resolution. Mode and effective
        // freshness are part of both fingerprint and cache key (INV-30-04).
        let analysis = analyze(&req.query);
        let profile = self
            .deps
            .profiles
            .get(req.profile.as_str())
            .cloned()
            .ok_or_else(|| {
                SearchOsError::validation("profile", format!("unknown profile {}", req.profile))
            })?;
        let freshness_label = effective_freshness_label(&req, &profile);
        let fp = fingerprint(&QueryFingerprintParts {
            normalized_query: analysis.normalized.clone(),
            language: analysis.language.clone(),
            profile_version: profile.version.clone(),
            freshness_policy: self.deps.effective_freshness().version.clone(),
            source_policy: self.deps.effective_source().version.clone(),
            workspace_scope: req.workspace_id.to_string(),
            mode: req.mode.as_str().to_string(),
            freshness_requirement: freshness_label.clone(),
        });
        let cache_key = SearchCacheKey {
            query_fingerprint: fp.clone(),
            profile_version: profile.version.clone(),
            freshness_policy_version: self.deps.effective_freshness().version.clone(),
            source_policy_version: self.deps.effective_source().version.clone(),
            language: analysis.language.clone(),
            workspace_scope: req.workspace_id.to_string(),
            mode: req.mode.as_str().to_string(),
            freshness_requirement: freshness_label.clone(),
            // RFC-034: a policy submit/rollback changes this component, so
            // cached answers cannot outlive the policy that produced them.
            key_version: format!("{}|{}", CACHE_KEY_VERSION, self.deps.policy_stamp()),
        };

        // Claim BEFORE the cache lookup. A cache hit is still a request with
        // an idempotency identity (Part 30 INV-30-04).
        let claim = self
            .deps
            .task_recorder
            .idempotency_begin(
                req.request_id.as_str(),
                task_id.as_str(),
                req.workspace_id.as_str(),
            )
            .await?;
        match claim {
            IdempotencyClaim::Claimed => {}
            IdempotencyClaim::AlreadyRunning { task_id: leader } => {
                metrics.inc_counter("search.idempotency.claim_conflict_total");
                tracing::debug!(leader = %leader, "joining in-flight idempotent request");
                return wait_for_idempotent_result(&self.deps, &req, &trace_id, &task_id, started)
                    .await;
            }
            IdempotencyClaim::Completed { response } => {
                let mut replayed = serde_json::from_value::<SearchResponse>(response)
                    .map_err(|e| SearchOsError::Internal(e.to_string()))?;
                replayed.replayed = true;
                metrics.record_search(started.elapsed().as_millis() as f64, "idempotent_replay");
                return Ok(replayed);
            }
            IdempotencyClaim::Failed => {
                return Err(SearchOsError::Internal("idempotency leader failed".into()))
            }
        }

        let bypass_cache = should_bypass_exact_cache(&req, &analysis, &profile);
        if !bypass_cache {
            if let CacheLookup::Hit(hit) = self.deps.cache.get(&cache_key, &profile.version) {
                let path = hit
                    .value
                    .payload
                    .get("decision")
                    .and_then(|d| d.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("");
                let cross_mode = req.mode == SearchMode::LocalOnly
                    && !matches!(path, "local" | "local_with_refresh" | "reject");
                if cross_mode {
                    metrics.inc_counter("cache.cross_mode_hit_total");
                } else if !validity_gate_allows_payload(self.deps.as_ref(), &hit.value.payload)
                    .await
                {
                    // RFC-077: a cached response citing deleted or changed
                    // documents is recomputed, never served.
                    metrics.inc_counter("cache.validity_reject_total");
                } else if let Ok(mut resp) =
                    serde_json::from_value::<SearchResponse>(hit.value.payload.clone())
                {
                    resp.replayed = true;
                    // RFC-071: a cache-served turn still advances the
                    // research workspace (the session is bookkeeping, not a
                    // retrieval shortcut) — and carries the workspace's
                    // earlier evidence, exactly like a computed turn.
                    if let Some(store) = self.deps.session_store.as_ref() {
                        let path = resp.decision.path.clone();
                        let gaps = resp.coverage_gaps.clone();
                        record_session_turn(store, &req, &mut resp, &path, &gaps);
                        let carried =
                            carry_workspace_evidence(&self.deps, &req, &resp.results).await;
                        resp.workspace_evidence = carried;
                    }
                    let payload = serde_json::to_value(&resp)
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?;
                    self.deps
                        .task_recorder
                        .idempotency_complete(req.request_id.as_str(), &payload)
                        .await?;
                    record_analytics(&self.deps, &req, &payload).await;
                    metrics.inc_cache_hit();
                    metrics
                        .record_search(started.elapsed().as_millis() as f64, &resp.decision.path);
                    metrics.record_quality(resp.quality.status.as_str());
                    metrics.record_status(resp.status.as_str());
                    return Ok(resp);
                }
            }
        }

        // RFC-031 D10: a terminal external failure replays honestly instead of
        // re-buying the same empty search. The status is preserved and
        // `replayed` is set; this is never a disguised success.
        if !bypass_cache {
            if let CacheLookup::Hit(hit) =
                self.deps.negative_cache.get(&cache_key, &profile.version)
            {
                if let Ok(mut resp) =
                    serde_json::from_value::<SearchResponse>(hit.value.payload.clone())
                {
                    resp.replayed = true;
                    metrics.inc_negative_cache_hit();
                    metrics.record_search(started.elapsed().as_millis() as f64, "negative_replay");
                    metrics.record_status(resp.status.as_str());
                    return Ok(resp);
                }
            }
        }

        // Singleflight merge (RFC-009 9.8). The fingerprint already includes
        // mode/freshness, so LocalOnly cannot join an external execution.
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
            Err(e) if e == "singleflight_timeout" => {
                metrics.inc_counter("singleflight.follower_timeout_total");
                return Ok(singleflight_timeout_response(&req, &trace_id, &task_id));
            }
            Err(e) => {
                if e.contains("leader_abandoned") {
                    metrics.inc_counter("singleflight.leader_abandoned_total");
                }
                let _ = self
                    .deps
                    .task_recorder
                    .idempotency_fail(req.request_id.as_str())
                    .await;
                return Err(SearchOsError::Internal(e));
            }
        };

        self.deps
            .task_recorder
            .idempotency_complete(req.request_id.as_str(), &payload)
            .await?;
        record_analytics(&self.deps, &req, &payload).await;

        if !is_leader {
            // One follower avoided one provider acquisition. Savings are
            // measurable, not structurally zero (Part 30 INV-30-13).
            metrics.inc_singleflight();
            let saved = CostEstimator::from_config(&self.deps.cost_config)
                .estimate_request("tavily")
                .amount_micros
                .max(1);
            let leader = payload
                .get("task_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let shared_ev = CostEvent {
                id: searchos_domain::ids::CostEventId::generate(),
                task_id: TaskId::new(leader.clone()),
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
                status: Default::default(),
                error_code: None,
            };
            self.deps.cost_sink.append(shared_ev).await?;
            self.deps
                .cost_sink
                .record_singleflight(&fp, &leader, &[task_id.to_string()], saved)
                .await?;
        }

        let response: SearchResponse =
            serde_json::from_value(payload).map_err(|e| SearchOsError::Internal(e.to_string()))?;
        // RFC-080 result-level learning: only the NON-CACHED path teaches the
        // router, so replaying a cached answer cannot inflate a source's
        // score. A source whose results nobody judged is left untouched.
        learn_source_quality(&self.deps, &req, &response.results).await;
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
        } else if !bypass_cache && should_negative_cache(&response) {
            // Only terminal "external acquisition found nothing" outcomes:
            // anything servable is never cached as a failure.
            self.deps.negative_cache.put(
                &cache_key,
                CachedSearchResult {
                    payload: serde_json::to_value(&response)
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?,
                    quality_status: "insufficient_evidence".into(),
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
    // Coverage sub-questions: explicit follow-up focus replaces the derived
    // decomposition so an agent can target exactly what the previous turn
    // left open (RFC-005 5.6 contract for multi-turn research).
    let required = if req.focus_requirements.is_empty() {
        required_subquestions(&analysis)
    } else {
        req.focus_requirements.clone()
    };
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

    // Hybrid retrieval + cache layers, then application-level constraint
    // filtering BEFORE any result/evidence is served (Part 30 INV-30-01).
    // Decision input cap: the retrieval pool is oversized for freshness, but
    // the engine must judge the same top_k it did before the pool existed.
    let decision_top_k = clamp_max_results(req.max_results.max(10)) as usize;
    let source_policy = deps.effective_source().with_constraints(
        &req.constraints.required_domains,
        &req.constraints.excluded_domains,
    );
    let all_hits = retrieve_local_candidates(
        &deps,
        &req,
        &analysis,
        &profile,
        &source_policy,
        &fp,
        &task_id,
    )
    .await?;
    let freshness_policy = deps.effective_freshness();
    let now = freshness_now(&req);
    let evals: Vec<LocalCandidateEval> = all_hits
        .iter()
        .map(|c| {
            let category = searchos_policy::classify_content(&c.url, c.title.as_deref());
            let age = (now - c.fetched_at).num_seconds().max(0);
            LocalCandidateEval {
                candidate: c.clone(),
                relevance: normalize_candidate_score(c),
                // A document fetched AFTER the as-of point did not exist yet
                // in that historical view (RFC-057).
                freshness_ok: c.fetched_at <= now && freshness_policy.is_fresh(category, age),
                source_ok: source_policy.allows(&c.url),
            }
        })
        .collect();
    // The retrieval pool is oversized so freshness/source filtering can
    // recover servable documents ranked below stale ones. The DECISION input
    // is still capped at the original top_k: otherwise a large pool of weak
    // candidates dilutes the quality gate's average relevance and falsely
    // escalates a covered local query to external acquisition.
    let servable_evals: Vec<LocalCandidateEval> = evals
        .iter()
        .filter(|e| e.freshness_ok && e.source_ok)
        .take(decision_top_k)
        .cloned()
        .collect();
    let servable_hits: Vec<RetrievalCandidate> =
        servable_evals.iter().map(|e| e.candidate.clone()).collect();
    // Only stale-but-allowed candidates may enter the refresh enqueue path;
    // excluded domains are never returned and never refreshed.
    let stale_hits: Vec<RetrievalCandidate> = evals
        .iter()
        .filter(|e| !e.freshness_ok && e.source_ok)
        .take(decision_top_k)
        .map(|e| e.candidate.clone())
        .collect();
    let stale_document_ids: Vec<searchos_domain::ids::DocumentId> =
        stale_hits.iter().map(|c| c.document_id.clone()).collect();
    let local_candidates = servable_evals;
    let max_results = clamp_max_results(req.max_results) as usize;

    // RFC-062 planner integration. In `legacy` this is a no-op, preserving
    // the exact existing pipeline. In `plan_only` / `execute` it produces an
    // auditable source plan, but the legacy decision engine remains the final
    // quality authority until the reserve/billing-aware executor migration
    // lands (see the dependency review's pipeline-level gate).
    let federated_plan = {
        let service = SearchServiceImpl {
            deps: Arc::clone(&deps),
        };
        service.federated_plan(&req, &required, &[])?
    };
    if let Some(plan) = &federated_plan {
        // Planning is observation, not an outcome: it is logged, never
        // surfaced as a warning. A warning changes `derive_status` and would
        // make the equivalence gate (federated_default_matches_legacy_pipeline)
        // fail by design — the planner must not perturb the legacy verdict.
        tracing::debug!(
            strategy = plan.strategy.as_str(),
            sources = ?plan.selected_source_ids(),
            "federated plan computed"
        );
    }

    // ---- Decision (RFC-004) ------------------------------------------
    let gate = QualityGate::new(
        deps.effective_quality(),
        deps.effective_freshness(),
        source_policy.clone(),
    );
    let engine = searchos_decision::DefaultDecisionEngine::new(gate);
    // RFC-076 source-awareness: observe whether any healthy web source
    // exists right now (enabled + circuit closed, visible to this
    // workspace). `allow_external = false` keeps the existing offline path
    // untouched (its warning contract is asserted by tests); no registry →
    // `None` (decision behaves exactly as before).
    let external_sources_healthy = if deps.allow_external {
        deps.federation_registry.as_ref().map(|registry| {
            registry
                .list(
                    &searchos_federated::SourceFilter {
                        enabled_only: true,
                        categories: vec![searchos_federated::SourceCategory::WebSearch],
                        ..Default::default()
                    },
                    &req.workspace_id,
                )
                .iter()
                .any(|descriptor| !descriptor.reliability.circuit_open)
        })
    } else {
        None
    };
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
        estimated_external_cost_per_query_micros: if req.budget.max_external_queries > 0 {
            estimated_external / i64::from(req.budget.max_external_queries)
        } else {
            0
        },
        currency: req.budget.currency.clone(),
        required_subquestions: required.clone(),
        required_domains: req.constraints.required_domains.clone(),
        excluded_domains: req.constraints.excluded_domains.clone(),
        stale_document_ids: stale_document_ids.clone(),
        external_sources_healthy,
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

    // ---- External query planning (RFC-031 D5) -------------------------
    // Expansion is part of the plan, not an afterthought: the reservation is
    // sized from the final query list, plus the promotion estimate, so the
    // ledger and the plan agree (RFC-031 G9).
    let per_query_cost = estimator
        .estimate_request(provider_id.as_deref().unwrap_or("tavily"))
        .amount_micros;
    // RFC-076: the affordable cap comes from the decision trace.
    let affordable = trace
        .cost_adaptive
        .as_ref()
        .map(|c| c.affordable_external_queries)
        .unwrap_or(req.budget.max_external_queries);
    let planned_queries = plan_external_queries(
        &deps,
        &req,
        &decision,
        per_query_cost,
        &req.constraints.required_domains,
        &req.constraints.excluded_domains,
        affordable,
    );
    let max_promotions = deps
        .promotion
        .as_ref()
        .map(|p| p.max_promotions_per_task())
        .unwrap_or(0);
    let promotion_estimate_micros = if max_promotions > 0
        && matches!(
            decision,
            SearchDecision::Hybrid { .. } | SearchDecision::ExternalOnly { .. }
        ) {
        estimator
            .estimate_request("web_fetch")
            .amount_micros
            .saturating_mul(max_promotions as i64)
    } else {
        0
    };
    let plan = builder.build(
        task_id.clone(),
        &decision,
        &req.budget,
        provider_id.as_deref(),
    )?;
    let estimated_total_micros = plan
        .estimated_cost
        .amount_micros
        .saturating_add(promotion_estimate_micros);
    // Budget enforcement BEFORE execution (RFC-009 9.6).
    let budget_check = searchos_policy::BudgetEnforcer::new(None, req.budget.currency.clone())
        .check(&req.budget, estimated_total_micros);
    if !budget_check.is_allowed() {
        let _ = deps.cost_sink.exceed_budget(task_id.as_str()).await;
        return Err(SearchOsError::Budget(
            budget_check.violation().unwrap_or("budget").to_string(),
        ));
    }
    // Budget is a decision constraint, not事后统计: reserve before
    // execution, settle with actual spend afterwards (RFC-004 v1.1 15).
    let mut guard = deps
        .cost_sink
        .reserve_budget(
            task_id.as_str(),
            trace_id.as_str(),
            estimated_total_micros,
            &req.budget.currency,
        )
        .await?;

    // ---- Execute -------------------------------------------------------
    let mut results: Vec<SearchResultItem> = Vec::new();
    let mut evidence: Vec<Evidence> = Vec::new();
    let mut cost_spent = Money::zero(&req.budget.currency);
    // RFC-031: what was bought this task, kept out of the index until the
    // final quality gate has judged it.
    let mut external_outcomes: Vec<ExternalQueryOutcome> = Vec::new();
    let mut acquisition = AcquisitionState::default();
    let mut acquisition_failed = false;
    let svc = SearchServiceImplStub {
        deps: Arc::clone(&deps),
        estimator: searchos_cost::CostEstimator::from_config(&deps.cost_config),
    };

    match &decision {
        SearchDecision::LocalOnly { .. } => {
            // Serve from local candidates (already retrieved above).
            let (l_items, l_ev, _rel) = local_results_and_evidence(&servable_hits, max_results);
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
                status: Default::default(),
                error_code: None,
            };
            deps.cost_sink.append(ev).await?;
            metrics.add_cost_micros(local_cost.amount_micros);
        }
        SearchDecision::Reject { reason } => {
            // Rule 5 partial local results must obey the same freshness /
            // source constraints as any other served evidence (INV-30-01).
            for c in servable_hits.iter().take(max_results) {
                results.push(SearchResultItem {
                    title: c.title.clone(),
                    url: c.url.clone(),
                    content: c.snippet.clone(),
                    score: normalize_candidate_score(c),
                    published_at: None,
                    source: "local_partial".into(),
                    highlight: c.highlight.clone(),
                    entity_id: None,
                });
            }
            warnings.push(format!("rejected: {}", reason.code()));
            let status = derive_status(&decision, QualityStatus::Failed, 0, &warnings);
            // Every reserve reaches a terminal state, including this early
            // return (Part 30 INV-30-03).
            guard.set_spent(cost_spent.amount_micros);
            if *reason == searchos_domain::decision::DecisionReason::BudgetExceeded {
                guard.exceed().await?;
            } else {
                guard.settle(cost_spent.amount_micros).await?;
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
            // RFC-071 v2: even a rejected turn carries the workspace's
            // earlier evidence — a follow-up still needs to see what the
            // research task already found.
            let carried = carry_workspace_evidence(&deps, &req, &results).await;
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
                None,
                carried,
            );
            return serde_json::to_value(&response)
                .map_err(|e| SearchOsError::Internal(e.to_string()));
        }
        SearchDecision::Hybrid { .. } | SearchDecision::ExternalOnly { .. } => {
            // Local evidence supplements external evidence in Hybrid and
            // ExternalFirst (RFC-004 v1.1 9.3/9.4), never replaces it.
            if matches!(
                req.mode,
                SearchMode::Hybrid | SearchMode::ExternalFirst | SearchMode::LocalFirst
            ) {
                let (l_items, l_ev, _rel) = local_results_and_evidence(&servable_hits, max_results);
                results.extend(l_items);
                evidence.extend(l_ev);
            }

            let execution = svc
                .execute_external(
                    &req,
                    &planned_queries,
                    &trace_id,
                    &task_id,
                    &mut acquisition,
                )
                .await;
            match execution {
                Ok((outcomes, evs, calls, spent)) => {
                    for outcome in outcomes {
                        results.extend(outcome.items.clone());
                        external_outcomes.push(outcome);
                    }
                    if calls > 0 && external_outcomes.iter().all(|o| o.items.is_empty()) {
                        // RFC-031 D10: the knowledge is absent. Mark it so the
                        // terminal failure can be negatively cached without
                        // ever disguising it as success.
                        warnings.push("external_no_results".into());
                    }
                    evidence.extend(evs);
                    cost_spent = Money {
                        amount_micros: spent,
                        currency: req.budget.currency.clone(),
                    };
                }
                Err(e) => {
                    // A failure after earlier successful calls must still
                    // report and settle the spend already in the ledger.
                    tracing::warn!(
                        code = %e.error_code,
                        spent_micros = e.spent_micros,
                        "external execution failed after partial spend"
                    );
                    metrics.inc_provider_failure();
                    cost_spent = Money {
                        amount_micros: e.spent_micros,
                        currency: req.budget.currency.clone(),
                    };
                    warnings.push(format!(
                        "external_execution_failed:{}:{}",
                        e.error_code, e.spent_micros
                    ));
                    // A provider failure is not a coverage gap: re-buying from
                    // a failing provider only burns the budget. Report
                    // honestly instead of iterating.
                    acquisition_failed = true;
                }
            }
            // RFC-031 D1/D2: external results are NOT indexed here. Promotion
            // is quality-gated and outbox-driven, so unverified snippets can
            // never become local knowledge.
        }
        SearchDecision::LocalWithRefresh { documents, .. } => {
            // RefreshExistingSource: serve valid local evidence AND enqueue
            // re-fetch of stale URLs. Never delete authoritative knowledge
            // (RFC-006 9.5, RFC-018 18.2).
            let (l_items, l_ev, _rel) = local_results_and_evidence(&servable_hits, max_results);
            results.extend(l_items);
            evidence.extend(l_ev);
            let urls: Vec<String> = stale_hits
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
                    status: Default::default(),
                    error_code: None,
                })
                .await?;
            metrics.add_cost_micros(refresh_cost.amount_micros);
        }
    }

    // ---- Evidence merge (RFC-004 v1.1 6.11) ---------------------------
    // `evidence` is retained (cloned) so a bounded follow-up round can merge
    // the accumulated set again without losing earlier acquisitions.
    let (mut merged_evidence, merge_report): (Vec<Evidence>, MergeReport) =
        merge_evidence(evidence.clone(), Vec::new());
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
        deps.effective_quality(),
        deps.effective_freshness(),
        source_policy.clone(),
    );
    // Relevance comes from the actual retrieval / provider scores, not a
    // constant placeholder (RFC-005 5.4: measured features only).
    let relevances: Vec<f32> = merged_evidence
        .iter()
        .map(|ev| {
            results
                .iter()
                .find(|r| r.url == ev.url)
                .map(|r| r.score.clamp(0.05, 1.0))
                .unwrap_or(0.6)
        })
        .collect();
    let mut verdict = gate.evaluate_set(&merged_evidence, &relevances, &required);
    if !merge_report.conflicts.is_empty() {
        verdict.status = QualityStatus::Conflict;
        verdict
            .failed_constraints
            .push("unresolved_evidence_conflict".into());
    }

    // ---- Bounded iterative acquisition (search-agent loop) -------------
    // If the final gate still has OPEN requirements, re-plan one targeted
    // query per gap and acquire again. Query/cost caps are cumulative via
    // `acquisition`, idempotency and the singleflight leader path are
    // unchanged (this all happens before the response is cached), and a
    // conflict never triggers re-buying — more search cannot resolve a
    // contradiction.
    let mut round = 1u32;
    while !acquisition_failed && should_replan(&decision, &verdict, &req, &acquisition, round) {
        let follow_up = plan_follow_up_queries(
            &verdict.coverage_gaps,
            &acquisition,
            acquisition.remaining_queries(&req),
            &req.query,
        );
        if follow_up.is_empty() {
            break;
        }
        let execution = svc
            .execute_external(&req, &follow_up, &trace_id, &task_id, &mut acquisition)
            .await;
        match execution {
            Ok((outcomes, evs, _calls, spent)) => {
                for outcome in outcomes {
                    results.extend(outcome.items.clone());
                    external_outcomes.push(outcome);
                }
                evidence.extend(evs);
                cost_spent = Money {
                    amount_micros: spent,
                    currency: req.budget.currency.clone(),
                };
            }
            Err(e) => {
                tracing::warn!(
                    code = %e.error_code,
                    round,
                    "follow-up acquisition failed after partial spend"
                );
                cost_spent = Money {
                    amount_micros: e.spent_micros,
                    currency: req.budget.currency.clone(),
                };
                warnings.push(format!("follow_up_failed:{}:{}", round, e.error_code));
                break;
            }
        }
        round += 1;
        warnings.push(format!("acquisition_round:{round}"));

        let (re_merged, re_report) = merge_evidence(evidence.clone(), Vec::new());
        merged_evidence = re_merged;
        for c in &re_report.conflicts {
            if !warnings.contains(c) {
                warnings.push(c.clone());
            }
        }
        let re_relevances: Vec<f32> = merged_evidence
            .iter()
            .map(|ev| {
                results
                    .iter()
                    .find(|r| r.url == ev.url)
                    .map(|r| r.score.clamp(0.05, 1.0))
                    .unwrap_or(0.6)
            })
            .collect();
        verdict = gate.evaluate_set(&merged_evidence, &re_relevances, &required);
        if !re_report.conflicts.is_empty() {
            verdict.status = QualityStatus::Conflict;
            verdict
                .failed_constraints
                .push("unresolved_evidence_conflict".into());
        }
    }

    if verdict.status == QualityStatus::Passed {
        deps.cost_sink.record_quality_pass(task_id.as_str()).await?;
        metrics.inc_coverage_pass();
    }
    if verdict.freshness == FreshnessStatus::Stale {
        metrics.inc_freshness_violation();
    }

    // ---- RFC-031: promotion + reuse accounting (after the gate) ---------
    promote_external_evidence(
        &deps,
        &req,
        &task_id,
        &external_outcomes,
        &merged_evidence,
        &verdict,
    )
    .await;
    record_reuse_if_external_sourced(
        &deps,
        &req,
        &task_id,
        &servable_hits,
        &merged_evidence,
        &verdict,
    )
    .await;

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
    let spent_micros = cost_spent.amount_micros;
    guard.set_spent(spent_micros);
    guard.settle(spent_micros).await?;

    // RFC-062 §16: the federated executor filled a per-source summary —
    // attach it to the trace and re-record. The SQLite collector uses
    // INSERT OR REPLACE, so this UPDATES the pre-execution trace; the
    // in-memory collector serves the last record for the id.
    if let Some(federation) = acquisition.federation_execution.take() {
        trace.federation_execution = Some(federation);
        deps.traces.record(trace.clone()).await?;
    }

    // RFC-071 v2: carry the research workspace's earlier evidence into this
    // turn (validated; never part of `results`).
    let workspace_evidence = carry_workspace_evidence(&deps, &req, &results).await;

    // RFC-038: the graph comes from what the merge observed — dedup folds
    // and syndication groups. An empty relation set yields `None`.
    let graph = {
        let block = crate::merge::build_evidence_graph(&merged_evidence, &merge_report);
        if block.edges.is_empty() {
            None
        } else {
            Some(block)
        }
    };
    let mut response = build_response(
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
        graph,
        workspace_evidence,
    );
    // Agent-facing contract: expose the open requirements so a caller can
    // decide between a targeted follow-up query and giving up (RFC-005 5.6).
    response.coverage_gaps = verdict.coverage_gaps.clone();

    // ---- Agent session turn recording (best-effort) --------------------
    // Runs after the response exists so the session can round-trip the
    // follow-up contract (next focus requirements) onto the response.
    if let Some(store) = &deps.session_store {
        let path = decision.path_name().to_string();
        record_session_turn(store, &req, &mut response, &path, &verdict.coverage_gaps);
    }
    // RFC-058: annotate served results with stable entity ids from the
    // alias read model (registered aliases only; no guessed identities).
    if let Some(lookup) = deps.entity_lookup.as_ref() {
        crate::entity::annotate_results(lookup.as_ref(), &mut response.results);
    }
    // Shared transport result cap (Part 30 INV-30-19).
    response.results.truncate(max_results);
    response.provenance.truncate(max_results);
    serde_json::to_value(&response).map_err(|e| SearchOsError::Internal(e.to_string()))
}

fn normalize_candidate_score(c: &RetrievalCandidate) -> f32 {
    match c.source {
        // RRF ~2/61 → 1.0. Reranked candidates keep the fused RRF score
        // (rerankers only reorder), so both branches share one scale.
        RetrievalSource::Hybrid | RetrievalSource::Reranked => {
            (c.score * 61.0 / 2.0).clamp(0.05, 1.0)
        }
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
    evidence_graph: Option<searchos_domain::response::EvidenceGraphBlock>,
    workspace_evidence: Option<searchos_domain::response::WorkspaceEvidenceBlock>,
) -> SearchResponse {
    SearchResponse {
        query: req.query.clone(),
        decision: DecisionReport {
            path: decision.path_name().into(),
            reason: decision.reason().code().into(),
        },
        results: results.clone(),
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
        coverage_gaps: Vec::new(),
        session_id: None,
        focus_requirements: Vec::new(),
        context: assemble_context_block(req, &results),
        evidence_graph,
        workspace_evidence,
    }
}

/// Assembles the Agent-facing context block (RFC-067/068) when the caller
/// declared a budget.
///
/// Every entry in `results` has already passed the pipeline's workspace
/// scoping and the final quality gate, so permission is asserted here rather
/// than re-derived: the permission boundary in this system is the pipeline,
/// not the assembler. The assembler still fails closed — an entry it cannot
/// map is dropped and *reported*, never silently included.
fn assemble_context_block(
    req: &SearchRequest,
    results: &[searchos_domain::response::SearchResultItem],
) -> Option<searchos_domain::context::ContextBlockSummary> {
    let budget = req.context_budget.as_ref()?;
    if let Err(message) = budget.validate() {
        tracing::warn!(%message, "invalid context_budget; skipping context assembly");
        return None;
    }
    let engine_budget = searchos_context::ContextBudget {
        max_tokens: budget.max_tokens,
        reserved_tokens: budget.reserved_tokens,
        max_sources: budget.max_sources,
        max_chunks_per_source: budget.max_chunks_per_source,
        deduplicate: budget.deduplicate,
        preserve_citations: budget.preserve_citations,
        include_conflicts: budget.include_conflicts,
    };
    let chunks: Vec<searchos_context::EvidenceChunk> = results
        .iter()
        .map(|item| searchos_context::EvidenceChunk {
            chunk_id: item.url.clone(),
            source_id: item.source.clone(),
            canonical_ref: item.url.clone(),
            title: item.title.clone(),
            content: item.content.clone(),
            content_hash: searchos_domain::evidence::normalized_content_fingerprint(&item.content),
            score: item.score,
            conflicting: false,
            permission_ok: true,
        })
        .collect();
    let block = match searchos_context::assemble_with_default_estimator(&chunks, &engine_budget) {
        Ok(block) => block,
        Err(error) => {
            tracing::warn!(%error, "context assembly failed; returning results without a context block");
            return None;
        }
    };
    Some(searchos_domain::context::ContextBlockSummary {
        status: match block.status {
            searchos_context::ContextStatus::Complete => {
                searchos_domain::context::ContextStatus::Complete
            }
            searchos_context::ContextStatus::BudgetLimited => {
                searchos_domain::context::ContextStatus::BudgetLimited
            }
            searchos_context::ContextStatus::CitationLimited => {
                searchos_domain::context::ContextStatus::CitationLimited
            }
        },
        max_tokens: block.token_usage.max_tokens,
        reserved_tokens: block.token_usage.reserved_tokens,
        usable_tokens: block.token_usage.usable_tokens,
        used_tokens: block.token_usage.used_tokens,
        truncated: block.truncated,
        evidence: block
            .evidence
            .iter()
            .map(|e| searchos_domain::context::ContextSnippet {
                citation: e.canonical_ref.clone(),
                source_id: e.source_id.clone(),
                title: e.title.clone(),
                content: e.content.clone(),
                truncated: e.truncated,
                conflicting: e.conflicting,
                occurrences: e.occurrences,
                estimated_tokens: e.estimated_tokens,
            })
            .collect(),
        citations: block.citation_refs.clone(),
        dropped: block
            .dropped
            .iter()
            .map(|d| searchos_domain::context::ContextDrop {
                chunk_id: d.chunk_id.clone(),
                source_id: d.source_id.clone(),
                canonical_ref: d.canonical_ref.clone(),
                reason: drop_reason(d.reason),
            })
            .collect(),
    })
}

/// Stable snake-case reason shared with the engine's wire contract.
fn drop_reason(reason: searchos_context::DropReason) -> String {
    match reason {
        searchos_context::DropReason::PermissionDenied => "permission_denied",
        searchos_context::DropReason::Duplicate => "duplicate",
        searchos_context::DropReason::ConflictExcluded => "conflict_excluded",
        searchos_context::DropReason::SourceCap => "source_cap",
        searchos_context::DropReason::ChunksPerSourceCap => "chunks_per_source_cap",
        searchos_context::DropReason::TokenBudget => "token_budget",
        searchos_context::DropReason::CitationBudget => "citation_budget",
    }
    .to_string()
}

/// One externally planned query, with its provenance and requirement link
/// (RFC-031 D5/D9). The plan and the executed queries are the same list.
#[derive(Debug, Clone)]
struct PlannedQuery {
    query: String,
    origin: QueryOrigin,
    requirement_id: Option<String>,
    strategy: Option<searchos_domain::decision::GapResolutionStrategy>,
}

/// One executed provider call and the items it returned.
#[derive(Debug, Clone)]
struct ExternalQueryOutcome {
    provider_id: String,
    query: String,
    origin: QueryOrigin,
    requirement_id: Option<String>,
    strategy: Option<searchos_domain::decision::GapResolutionStrategy>,
    items: Vec<SearchResultItem>,
}

/// RFC-066: orders federated per-source outcomes by trust-weighted rank
/// fusion instead of plan order.
///
/// Weights come from the sources' `ReliabilityProfile.success_rate` — never
/// from provider raw scores, which are not comparable across sources
/// (RFC-062 §9.2). Fusion only ORDERS: every item keeps its own payload and
/// its owning outcome, so billing, promotion attribution and per-provider
/// provenance stay intact. Within one source, items follow the fused
/// subsequence (the provider's own ranking when it is the only contributor).
/// A source with no candidates stays last.
fn order_federated_outcomes(
    outcomes: &mut [ExternalQueryOutcome],
    reliability: &std::collections::HashMap<String, f32>,
) {
    if outcomes.len() < 2 {
        return;
    }
    let fused = {
        let lists: Vec<searchos_federated::RankedSourceList<'_, _>> = outcomes
            .iter()
            .map(|o| searchos_federated::RankedSourceList {
                source_id: &o.provider_id,
                weight: reliability.get(&o.provider_id).copied().unwrap_or(1.0),
                items: &o.items,
            })
            .collect();
        searchos_federated::fuse_ranks(&lists, |i| i.url.clone(), 60.0)
    };
    let position = |url: &str| {
        fused
            .iter()
            .position(|i| i.url == url)
            .unwrap_or(usize::MAX)
    };
    for outcome in outcomes.iter_mut() {
        outcome.items.sort_by_key(|i| position(&i.url));
    }
    // Stable sort: equal best-rank sources keep plan order.
    outcomes.sort_by_key(|o| {
        o.items
            .iter()
            .map(|i| position(&i.url))
            .min()
            .unwrap_or(usize::MAX)
    });
}

/// Cumulative acquisition state across bounded iterative rounds. The query
/// and cost caps are TOTAL caps: no round may re-buy covered requirements or
/// exceed the reservation (INV-30-04 / RFC-004 v1.1 6.8).
#[derive(Default, Debug, Clone)]
struct AcquisitionState {
    calls: u64,
    spent_micros: i64,
    /// Lowercased queries already executed; a follow-up round never repeats
    /// a query that was just bought.
    executed_queries: std::collections::HashSet<String>,
    /// Per-source federation execution summary (RFC-062 §16). Filled by the
    /// federated executor path; attached to the decision trace at the common
    /// tail so `trace show` records what happened to every planned source.
    federation_execution: Option<searchos_domain::decision::FederationExecutionTrace>,
}

impl AcquisitionState {
    fn remaining_queries(&self, req: &SearchRequest) -> usize {
        req.budget
            .max_external_queries
            .saturating_sub(self.calls as u32) as usize
    }
}

/// Thin internal handle to reuse execute_external from the pipeline.
struct SearchServiceImplStub {
    deps: Arc<SearchDependencies>,
    estimator: CostEstimator,
}

#[allow(clippy::unused_self)]
impl SearchServiceImplStub {
    /// Executes targeted external queries. Every error carries the spend
    /// already written to the ledger and appends a zero-amount failed
    /// `provider_attempt` event for audit (Part 30 INV-30-06).
    async fn execute_external(
        &self,
        req: &SearchRequest,
        planned: &[PlannedQuery],
        trace_id: &TraceId,
        task_id: &TaskId,
        state: &mut AcquisitionState,
    ) -> Result<(Vec<ExternalQueryOutcome>, Vec<Evidence>, u64, i64), ExternalExecutionError> {
        let mut outcomes: Vec<ExternalQueryOutcome> = Vec::new();
        let mut evidence = Vec::new();
        let mut calls = state.calls;
        let mut spent = Money {
            amount_micros: state.spent_micros,
            currency: req.budget.currency.clone(),
        };
        let max_results = clamp_max_results(req.max_results) as usize;
        let mut provider_id: Option<String> = None;

        macro_rules! fail {
            ($code:expr, $message:expr) => {{
                state.calls = calls;
                state.spent_micros = spent.amount_micros;
                let err = ExternalExecutionError {
                    spent_micros: spent.amount_micros,
                    error_code: $code.to_string(),
                    message: $message.to_string(),
                };
                let attempt = CostEvent {
                    id: searchos_domain::ids::CostEventId::generate(),
                    task_id: task_id.clone(),
                    workspace_id: req.workspace_id.clone(),
                    agent_id: req.agent_id.clone(),
                    operation: CostOperation::ProviderAttempt,
                    provider_id: provider_id.clone(),
                    input_units: 0,
                    output_units: 0,
                    estimated_cost: Money::zero(&req.budget.currency),
                    actual_cost: None,
                    duration_ms: 0,
                    occurred_at: Utc::now(),
                    trace_id: Some(trace_id.clone()),
                    shared: false,
                    pricing_model_version: "1".into(),
                    status: CostAttemptStatus::Failed,
                    error_code: Some(err.error_code.clone()),
                };
                let _ = self.deps.cost_sink.append(attempt).await;
                return Err(err);
            }};
        }

        if !self.deps.allow_external {
            fail!("POLICY_VIOLATION", "external execution disabled (offline)");
        }
        let selection = match self
            .deps
            .router
            .route(true, req.budget.max_total_cost_micros)
            .await
        {
            Ok(v) => v,
            Err(e) => fail!(e.class().code, e.to_string()),
        };
        let Some(entry) = selection.first().cloned() else {
            fail!(
                "PROVIDER_FAILURE",
                "no healthy provider satisfies quality/budget constraints"
            );
        };
        provider_id = Some(entry.id.clone());
        let Some(provider) = self
            .deps
            .search_providers
            .iter()
            .find(|p| p.id() == entry.id)
        else {
            fail!("PROVIDER_FAILURE", "router picked unregistered provider");
        };

        // RFC-062 task 4b: when the federation strategy opts in, source
        // selection AND fan-out move to the federated planner/executor. The
        // billing semantics are unchanged and stay HERE — one cost event per
        // executed query, priced by the same estimator, settled by the
        // pipeline's reservation guard. See `execute_external_federated`.
        let federated_dispatch = self.deps.federation_strategy == FEDERATION_EXECUTE
            && self.deps.federation_registry.is_some();
        if federated_dispatch {
            return self
                .execute_external_federated(req, planned, trace_id, task_id, state)
                .await;
        }

        let mut queries: Vec<PlannedQuery> = planned.to_vec();
        queries.retain(|q| !q.query.trim().is_empty());
        if state.calls == 0 {
            // First round: preserve the legacy single-call fallback. Only
            // follow-up rounds de-duplicate against what was already bought.
            queries.truncate(req.budget.max_external_queries.max(1) as usize);
        } else {
            queries.retain(|q| !state.executed_queries.contains(&q.query.to_lowercase()));
            queries.truncate(state.remaining_queries(req));
        }
        if queries.is_empty() {
            queries.push(PlannedQuery {
                query: req.query.clone(),
                origin: QueryOrigin::Fallback,
                requirement_id: None,
                strategy: None,
            });
        }

        for planned_query in &queries {
            if calls >= req.budget.max_external_queries as u64 {
                break;
            }
            let price = self.estimator.estimate_request(provider.id());
            if spent.amount_micros + price.amount_micros > req.budget.max_total_cost_micros {
                break;
            }
            let q = &planned_query.query;
            state.executed_queries.insert(q.to_lowercase());
            let params = SearchParams {
                query: q.clone(),
                max_results: req.max_results,
                required_domains: req.constraints.required_domains.clone(),
                excluded_domains: req.constraints.excluded_domains.clone(),
                language: req.constraints.language.clone(),
                freshness_days: match &req.constraints.freshness {
                    FreshnessRequirement::Strict => Some(1),
                    FreshnessRequirement::Recent => Some(30),
                    FreshnessRequirement::MaximumAgeSeconds(s) => Some((s / 86400).max(1) as u32),
                    FreshnessRequirement::Any => None,
                },
                include_raw_content: false,
            };
            let result = match provider.search(&params).await {
                Ok(r) => r,
                Err(e) => {
                    let domain = SearchOsError::from(e);
                    fail!(domain.class().code, domain.to_string())
                }
            };
            calls += 1;
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
                status: CostAttemptStatus::Succeeded,
                error_code: None,
            };
            if let Err(e) = self.deps.cost_sink.append(ev).await {
                fail!(e.class().code, e.to_string());
            }
            spent = match spent.checked_add(&price) {
                Ok(v) => v,
                Err(e) => fail!("INTERNAL_ERROR", e),
            };
            searchos_observability::MetricsRegistry::global().add_cost_micros(price.amount_micros);

            let mut call_items = Vec::new();
            for entry in result.entries.iter().take(max_results) {
                call_items.push(SearchResultItem {
                    title: entry.title.clone(),
                    url: entry.url.clone(),
                    content: entry.content.clone(),
                    score: entry.score,
                    published_at: entry.published_at,
                    source: provider.id().to_string(),
                    highlight: None,
                    entity_id: None,
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
            outcomes.push(ExternalQueryOutcome {
                provider_id: provider.id().to_string(),
                query: q.clone(),
                origin: planned_query.origin,
                requirement_id: planned_query.requirement_id.clone(),
                strategy: planned_query.strategy,
                items: call_items,
            });
        }
        state.calls = calls;
        state.spent_micros = spent.amount_micros;
        Ok((outcomes, evidence, calls, spent.amount_micros))
    }
}

/// Unused import suppression: FetchRequest/ExtractDepth referenced by
/// provider plumbing in sibling services.
#[allow(dead_code)]
fn _unused(_r: FetchRequest, _d: ExtractDepth) {}

impl SearchServiceImplStub {
    /// RFC-062 task 4b: executes the planned queries through the federated
    /// executor (per-source deadline, bounded fan-out, failure isolation)
    /// while BILLING stays in this service — one cost event per executed
    /// source, priced by the same estimator as the legacy loop, settled by
    /// the pipeline's reservation guard. Spend and call counters flow back
    /// through `state` exactly like the legacy path, so the iterative
    /// acquisition loop and the INV-30-13 singleflight accounting are
    /// unchanged.
    ///
    /// Requirement mapping: the plan's generic (undeclared) sources inherit
    /// the planned queries' requirement ids in order — the same loose
    /// mapping the legacy loop has, because wiring does not know what a
    /// provider will answer.
    #[allow(clippy::too_many_arguments)]
    async fn execute_external_federated(
        &self,
        req: &SearchRequest,
        planned: &[PlannedQuery],
        trace_id: &TraceId,
        task_id: &TaskId,
        state: &mut AcquisitionState,
    ) -> Result<(Vec<ExternalQueryOutcome>, Vec<Evidence>, u64, i64), ExternalExecutionError> {
        let metrics = searchos_observability::MetricsRegistry::global();
        let mut outcomes: Vec<ExternalQueryOutcome> = Vec::new();
        let mut evidence: Vec<Evidence> = Vec::new();
        let mut calls = state.calls;
        let mut spent_micros = state.spent_micros;
        let max_results = clamp_max_results(req.max_results) as usize;

        // Shared failure path: records the failed provider_attempt event
        // (Part 30 INV-30-06) and carries the spend already written to the
        // ledger back through `state`.
        macro_rules! fail {
            ($code:expr, $message:expr) => {{
                state.calls = calls;
                state.spent_micros = spent_micros;
                let attempt = CostEvent {
                    id: searchos_domain::ids::CostEventId::generate(),
                    task_id: task_id.clone(),
                    workspace_id: req.workspace_id.clone(),
                    agent_id: req.agent_id.clone(),
                    operation: CostOperation::ProviderAttempt,
                    provider_id: None,
                    input_units: 0,
                    output_units: 0,
                    estimated_cost: Money::zero(&req.budget.currency),
                    actual_cost: None,
                    duration_ms: 0,
                    occurred_at: Utc::now(),
                    trace_id: Some(trace_id.clone()),
                    shared: false,
                    pricing_model_version: "1".into(),
                    status: CostAttemptStatus::Failed,
                    error_code: Some($code.to_string()),
                };
                let _ = self.deps.cost_sink.append(attempt).await;
                return Err(ExternalExecutionError {
                    spent_micros,
                    error_code: $code.to_string(),
                    message: $message.to_string(),
                });
            }};
        }

        if !self.deps.allow_external {
            fail!("POLICY_VIOLATION", "external execution disabled (offline)");
        }
        let Some(registry) = self.deps.federation_registry.as_ref() else {
            fail!(
                "INTERNAL_ERROR",
                "federation execute requested but no registry is wired"
            );
        };

        // Plan over the WEB sources only: the local index is the pipeline's
        // own local path and must never be re-purchased as an external call.
        let descriptors = registry.list(
            &searchos_federated::SourceFilter {
                enabled_only: true,
                categories: vec![searchos_federated::SourceCategory::WebSearch],
                ..Default::default()
            },
            &req.workspace_id,
        );
        let strategy =
            searchos_federated::FederationStrategy::parse(&self.deps.federation_strategy)
                .unwrap_or(searchos_federated::FederationStrategy::Auto);
        let mut requirements: Vec<String> = planned
            .iter()
            .map(|q| q.requirement_id.clone().unwrap_or_default())
            .filter(|r| !r.is_empty())
            .collect();
        if requirements.is_empty() {
            // ExternalOnly / ExternalFirst carry no gap-derived needs: the
            // legacy loop still buys ONE fallback query, attributed after
            // acquisition. Without an open requirement the planner would
            // reject every billable source as "fills nothing" (INV-30-04)
            // and `execute` mode would acquire nothing at all. The synthetic
            // id is namespaced so it can never collide with an executed
            // query string in `covered_requirements`.
            requirements.push(format!("fallback:{}", req.query.to_lowercase()));
        }
        let context = searchos_federated::PlanningContext {
            query: req.query.clone(),
            mode: req.mode,
            strategy,
            workspace_id: req.workspace_id.to_string(),
            requirements: requirements.clone(),
            covered_requirements: state.executed_queries.iter().cloned().collect(),
            required_requirements: Vec::new(),
            max_sources: self.deps.federation_limits.max_sources.max(1),
            max_parallel_sources: self.deps.federation_limits.max_parallel_sources.max(1),
            // Remaining budget bounds the plan: the reservation is already
            // held and cannot be exceeded by the executor.
            max_estimated_cost_micros: req
                .budget
                .max_total_cost_micros
                .saturating_sub(state.spent_micros),
            max_latency_ms: req.budget.max_execution_time_ms,
            stop_when_covered: true,
            generic_sources_can_answer: true,
            selection: self.deps.selection_mode(),
        };
        let plan = searchos_federated::FederatedPlanner::plan(&context, &descriptors);
        tracing::debug!(
            sources = ?plan.selected_source_ids(),
            rejected = plan.rejections.len(),
            estimated_micros = plan.estimated_cost_micros,
            "federation execute plan"
        );
        if plan.selected_source_ids().is_empty() {
            fail!(
                "PROVIDER_FAILURE",
                "federation plan selected no eligible web source"
            );
        }

        // One representative query per executed source: the legacy loop runs
        // the planned queries sequentially against ONE router-picked
        // provider; the federated path runs them across the SELECTED
        // providers in parallel. Query attribution follows plan order.
        let query_for = |index: usize| -> PlannedQuery {
            planned.get(index).cloned().unwrap_or_else(|| PlannedQuery {
                query: req.query.clone(),
                origin: QueryOrigin::Fallback,
                requirement_id: None,
                strategy: None,
            })
        };

        let executor = searchos_federated::FederatedExecutor::new(
            registry,
            searchos_federated::ExecutionScope {
                request_id: req.request_id.clone(),
                workspace_id: req.workspace_id.clone(),
                max_parallel_sources: self.deps.federation_limits.max_parallel_sources.max(1),
            },
            // RFC-080: the caller's wall-clock budget becomes the executor's
            // absolute deadline. Every per-source timeout is capped by the
            // remaining part of it (no source may overrun the request).
            searchos_federated::ExecutionControls::new().with_deadline(
                chrono::Utc::now()
                    + chrono::Duration::milliseconds(req.budget.max_execution_time_ms as i64),
            ),
        );
        let request = SourceSearchRequest::new(
            req.query.clone(),
            req.mode,
            req.workspace_id.clone(),
            max_results.max(1),
        );
        let report = executor.execute(&plan, &request).await;
        record_federation_execution(&report);

        // RFC-080 slice: feed the outcome back into the LIVE reliability
        // profile. A source that fails three times in a row is opened by the
        // registry and the next plan rejects it as `Reliability`, instead of
        // paying for it forever. A success resets the counter. This never
        // touches billing (settlement happens below) and never affects the
        // results of THIS request — only the next plan.
        for result in &report.results {
            let success = !result.status.is_failure() && !result.timed_out;
            let source_key = searchos_domain::ids::SourceId::new(&result.source_id);
            if registry
                .record_outcome(&source_key, success)
                .unwrap_or(false)
            {
                searchos_observability::MetricsRegistry::global()
                    .inc_counter("federation.source.circuit_opened_total");
            }
            // Only a response that actually arrived carries a MEASURED
            // latency; a timeout or failure has no trustworthy sample.
            if let Some(latency_ms) = result.response.as_ref().map(|r| r.latency_ms) {
                registry.record_latency(&source_key, latency_ms);
            }
        }

        // RFC-062 §16: per-source execution summary for the decision trace.
        // The pipeline attaches this to the trace and re-records it once
        // acquisition settles, so a timeout or failure is visible in
        // `trace show` without cross-referencing cost events.
        state.federation_execution = Some(searchos_domain::decision::FederationExecutionTrace {
            strategy: self.deps.federation_strategy.clone(),
            planned_sources: plan.selected_source_ids(),
            executed_sources: report.results.iter().map(|r| r.source_id.clone()).collect(),
            failed_sources: report
                .results
                .iter()
                .filter(|r| r.status.is_failure())
                .map(|r| r.source_id.clone())
                .collect(),
            timed_out_sources: report
                .results
                .iter()
                .filter(|r| r.timed_out)
                .map(|r| r.source_id.clone())
                .collect(),
            not_executed_sources: report.not_executed.clone(),
            candidates: report.results.iter().map(|r| r.candidates().len()).sum(),
        });

        for (index, result) in report.results.iter().enumerate() {
            // Budget is checked per executed source against the SAME cap the
            // legacy loop used; the reservation guard settles the total.
            let price = self.estimator.estimate_request(&result.source_id);
            if spent_micros + price.amount_micros.max(0) > req.budget.max_total_cost_micros {
                break;
            }
            let planned_query = query_for(index);
            let source_name = result.source_id.clone();
            let duration_ms = result
                .response
                .as_ref()
                .map(|r| r.latency_ms)
                .unwrap_or_default();
            let ev = CostEvent {
                id: searchos_domain::ids::CostEventId::generate(),
                task_id: task_id.clone(),
                workspace_id: req.workspace_id.clone(),
                agent_id: req.agent_id.clone(),
                operation: CostOperation::WebSearch,
                provider_id: Some(source_name.clone()),
                input_units: 1,
                output_units: 0,
                estimated_cost: price.clone(),
                actual_cost: None,
                duration_ms,
                occurred_at: Utc::now(),
                trace_id: Some(trace_id.clone()),
                shared: false,
                pricing_model_version: "1".into(),
                status: if result.status == SourceExecutionStatus::Success {
                    CostAttemptStatus::Succeeded
                } else {
                    CostAttemptStatus::Failed
                },
                error_code: result
                    .failure
                    .as_ref()
                    .map(|f| f.status.as_str().to_string()),
            };
            if let Err(e) = self.deps.cost_sink.append(ev).await {
                fail!(e.class().code, e.to_string());
            }
            spent_micros += price.amount_micros.max(0);
            calls += 1;
            metrics.add_cost_micros(price.amount_micros.max(0));

            // Fail-open: a failed or timed-out source is reported (its cost
            // event above already carries the failure status) and the other
            // sources' results are kept. This mirrors the kernel's
            // `aggregate()` boundary — availability is not a quality pass.
            let candidates = result.candidates();
            if result.status != SourceExecutionStatus::Success || candidates.is_empty() {
                outcomes.push(ExternalQueryOutcome {
                    provider_id: source_name.clone(),
                    query: planned_query.query.clone(),
                    origin: planned_query.origin,
                    requirement_id: planned_query.requirement_id.clone(),
                    strategy: planned_query.strategy,
                    items: Vec::new(),
                });
                continue;
            }

            let mut call_items = Vec::new();
            for candidate in candidates.iter().take(max_results) {
                call_items.push(SearchResultItem {
                    title: candidate.title.clone(),
                    url: candidate.canonical_ref.clone(),
                    content: candidate.snippet.clone(),
                    score: candidate.raw_score.unwrap_or(0.0),
                    published_at: candidate.published_at,
                    source: source_name.clone(),
                    highlight: None,
                    entity_id: None,
                });
                evidence.push(Evidence {
                    id: searchos_domain::ids::EvidenceId::generate(),
                    source_id: searchos_domain::ids::SourceId::generate(),
                    document_version_id: None,
                    title: candidate.title.clone(),
                    url: candidate.canonical_ref.clone(),
                    content_ref: None,
                    snippet: Some(candidate.snippet.clone()),
                    published_at: candidate.published_at,
                    fetched_at: Utc::now(),
                    source_reliability: source_reliability_of(&candidate.canonical_ref),
                    provenance: searchos_domain::evidence::Provenance {
                        origin: searchos_domain::evidence::ProvenanceOrigin::External,
                        provider_id: Some(source_name.clone()),
                        source_url: Some(candidate.canonical_ref.clone()),
                        document_id: None,
                        retrieved_at: Utc::now(),
                    },
                });
            }
            outcomes.push(ExternalQueryOutcome {
                provider_id: source_name.clone(),
                query: planned_query.query.clone(),
                origin: planned_query.origin,
                requirement_id: planned_query.requirement_id.clone(),
                strategy: planned_query.strategy,
                items: call_items,
            });
            // Mark the executed query so follow-up rounds never re-buy it
            // (INV-30-04).
            state
                .executed_queries
                .insert(planned_query.query.to_lowercase());
        }

        state.calls = calls;
        state.spent_micros = spent_micros;
        // RFC-066: inter-source order follows trust-weighted rank fusion
        // over the sources' ReliabilityProfiles — never plan order, and
        // never provider raw scores (scores from different sources are not
        // comparable). Within a source the provider's own ranking stands.
        // Single-source plans (every existing gate) are unchanged.
        if outcomes.len() > 1 {
            let reliability: std::collections::HashMap<String, f32> = descriptors
                .iter()
                .map(|d| (d.source_id.to_string(), d.reliability.success_rate))
                .collect();
            order_federated_outcomes(&mut outcomes, &reliability);
        }

        Ok((outcomes, evidence, calls, spent_micros))
    }
}

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

    // RFC-067/068 (RFC-062 §12): the context block honours the budget, keeps
    // every citation even when content must shrink, and never reports a
    // truncated assembly as complete.
    #[test]
    fn context_assembly_never_drops_citations() {
        let items = |refs: &[(&str, &str)]| -> Vec<SearchResultItem> {
            refs.iter()
                .map(|(reference, content)| SearchResultItem {
                    title: Some(reference.to_string()),
                    url: reference.to_string(),
                    content: content.to_string(),
                    score: 1.0,
                    published_at: None,
                    source: "local".into(),
                    highlight: None,
                    entity_id: None,
                })
                .collect()
        };
        let mut req = request("q", SearchMode::LocalFirst);
        req.context_budget = Some(searchos_domain::context::ContextBudget {
            // 10 usable tokens: three 1-token citations + zero content pool.
            max_tokens: 12,
            reserved_tokens: 2,
            ..Default::default()
        });

        let results = items(&[
            ("https://a.dev/1", "one two three"),
            ("https://b.dev/2", "four five six"),
        ]);
        let context = assemble_context_block(&req, &results).expect("budget is valid");
        assert_eq!(context.evidence.len(), 2);
        assert_eq!(
            context.citations.len(),
            2,
            "citations survive a zero content pool"
        );
        assert!(context.evidence.iter().all(|e| e.content.is_empty()));
        assert!(context.evidence.iter().all(|e| e.truncated));
        assert_eq!(
            context.status,
            searchos_domain::context::ContextStatus::BudgetLimited
        );
        assert!(context.truncated);
        assert!(context.used_tokens <= context.usable_tokens);

        // A generous budget keeps the original text verbatim.
        req.context_budget = Some(searchos_domain::context::ContextBudget {
            max_tokens: 500,
            reserved_tokens: 10,
            ..Default::default()
        });
        let context = assemble_context_block(&req, &results).expect("budget is valid");
        assert_eq!(
            context.status,
            searchos_domain::context::ContextStatus::Complete
        );
        assert!(!context.truncated);
        for (entry, original) in context.evidence.iter().zip(results.iter()) {
            assert_eq!(entry.content, original.content, "verbatim, never a summary");
            assert_eq!(entry.citation, original.url);
        }

        // No budget declared -> no context block, and results untouched.
        let mut req = request("q", SearchMode::LocalFirst);
        req.context_budget = None;
        assert!(assemble_context_block(&req, &results).is_none());

        // An invalid budget is rejected at the edge, not silently assembled.
        let mut req = request("q", SearchMode::LocalFirst);
        req.context_budget = Some(searchos_domain::context::ContextBudget {
            reserved_tokens: 10,
            max_tokens: 5,
            ..Default::default()
        });
        assert!(assemble_context_block(&req, &results).is_none());

        let _ = items(&[]); // keep the helper used symmetrically
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

    // ---- RFC-066: trust-weighted cross-source ordering -----------------

    fn fusion_outcome(provider: &str, urls: &[&str]) -> ExternalQueryOutcome {
        ExternalQueryOutcome {
            provider_id: provider.into(),
            query: "q".into(),
            origin: QueryOrigin::Fallback,
            requirement_id: None,
            strategy: None,
            items: urls
                .iter()
                .map(|u| SearchResultItem {
                    title: None,
                    url: (*u).into(),
                    content: String::new(),
                    score: 0.9,
                    published_at: None,
                    source: provider.into(),
                    highlight: None,
                    entity_id: None,
                })
                .collect(),
        }
    }

    #[test]
    fn reliability_weight_overrides_plan_order_in_federated_outcomes() {
        // Plan order puts the LOW-reliability source first (e.g. higher
        // priority); fusion must nevertheless put the trustworthy source's
        // best-ranked item first.
        let mut outcomes = vec![
            fusion_outcome("weak", &["https://weak.dev/1", "https://weak.dev/2"]),
            fusion_outcome(
                "trusted",
                &["https://trusted.dev/1", "https://trusted.dev/2"],
            ),
        ];
        let reliability = std::collections::HashMap::from([
            ("weak".to_string(), 0.1),
            ("trusted".to_string(), 1.0),
        ]);
        order_federated_outcomes(&mut outcomes, &reliability);
        assert_eq!(outcomes[0].provider_id, "trusted");
        assert_eq!(outcomes[0].items[0].url, "https://trusted.dev/1");
        // Fusion only reorders: every item keeps its payload and owner.
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes.iter().map(|o| o.items.len()).sum::<usize>(), 4);
        assert!(outcomes
            .iter()
            .all(|o| o.items.iter().all(|i| i.source == o.provider_id)));
    }

    #[test]
    fn federated_outcome_order_is_deterministic() {
        let reliability =
            std::collections::HashMap::from([("a".to_string(), 0.7), ("b".to_string(), 0.7)]);
        let run = || {
            let mut outcomes = vec![
                fusion_outcome("a", &["https://a.dev/1", "https://a.dev/2"]),
                fusion_outcome("b", &["https://b.dev/1"]),
            ];
            order_federated_outcomes(&mut outcomes, &reliability);
            outcomes
                .iter()
                .flat_map(|o| o.items.iter().map(|i| i.url.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
        // Equal weights and equal best rank → key tie-break wins, not plan
        // order's randomness.
        assert_eq!(run()[0], "https://a.dev/1");
    }

    #[test]
    fn single_source_outcomes_are_left_untouched() {
        let mut outcomes = vec![fusion_outcome(
            "only",
            &["https://only.dev/1", "https://only.dev/2"],
        )];
        order_federated_outcomes(&mut outcomes, &Default::default());
        assert_eq!(outcomes[0].items[0].url, "https://only.dev/1");
        assert_eq!(outcomes[0].items[1].url, "https://only.dev/2");
    }
}
