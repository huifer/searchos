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
use searchos_document::canonical::canonical_url;
use searchos_document::content_hash;
use searchos_domain::cost::{CostAttemptStatus, CostEvent, CostOperation, Money};
use searchos_domain::decision::{DecisionReason, SearchDecision};
use searchos_domain::ids::{TaskId, TraceId};
use searchos_domain::request::{
    clamp_max_results, FreshnessRequirement, SearchIntent, SearchMode, SearchRequest,
    DEFAULT_MAX_RESULTS,
};
use searchos_domain::response::{
    CostReport, CoverageStatus, DecisionReport, FreshnessStatus, QualityReport, QualityStatus,
    SearchResponse, SearchResultItem, SearchStatus,
};
use searchos_domain::retrieval::{
    IndexedDocument, RetrievalCandidate, RetrievalQuery, RetrievalSource, SearchCacheKey,
    CACHE_KEY_VERSION,
};
use searchos_domain::{CoverageGap, Evidence, SearchOsError};
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
#[allow(clippy::too_many_arguments)]
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
    /// Config-driven semantic dedup / incremental retrieval switches.
    pub semantic_dedup: bool,
    pub incremental: bool,
    /// Intelligence section: query expansion + rerank cache gate.
    pub query_expansion: bool,
    pub reranking: bool,
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

/// Stable cache / semantic-compatibility label for the effective freshness
/// requirement (request constraint + profile default).
fn effective_freshness_label(req: &SearchRequest, profile: &SearchProfile) -> String {
    format!(
        "{}|{}",
        req.constraints.freshness.cache_label(),
        profile.freshness.cache_label()
    )
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
        freshness_policy: deps.freshness.version.clone(),
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
) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
    let metrics = searchos_observability::MetricsRegistry::global();
    let top_k = clamp_max_results(req.max_results.max(10)) as usize;
    let freshness_label = effective_freshness_label(req, profile);
    let policy_version = format!(
        "{}|{}|{}",
        profile.version, deps.freshness.version, source_policy.version
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
            metrics.inc_semantic_reuse();
            return Ok(cached);
        }
    }
    if let Some(cached) = deps.canonical_cache.get(
        fp,
        &analysis.language,
        req.workspace_id.as_str(),
        &policy_version,
    ) {
        return Ok(cached);
    }
    if deps.incremental {
        if let Some(cached) = deps.retrieval_cache.get(fp, top_k, &policy_version) {
            return Ok(cached);
        }
    }

    let mut query = RetrievalQuery::new(&analysis.normalized, top_k);
    query.workspace_id = Some(req.workspace_id.to_string());
    query.language = req.constraints.language.clone();
    query.required_domains = req.constraints.required_domains.clone();
    query.excluded_domains = req.constraints.excluded_domains.clone();
    // Keep stale candidates available for the refresh path; application-level
    // filtering decides what is servable (Part 30 INV-30-01).
    query.include_stale = true;
    let fetched_after = match req.constraints.freshness {
        FreshnessRequirement::Strict => Some(Utc::now() - chrono::Duration::days(1)),
        FreshnessRequirement::Recent => Some(Utc::now() - chrono::Duration::days(30)),
        FreshnessRequirement::MaximumAgeSeconds(seconds) => {
            Some(Utc::now() - chrono::Duration::seconds(seconds.min(i64::MAX as u64) as i64))
        }
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
    let ids: Vec<&str> = candidates.iter().map(|c| c.chunk_id.as_str()).collect();
    if deps.reranking {
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
            freshness_policy: self.deps.freshness.version.clone(),
            source_policy: self.deps.source.version.clone(),
            workspace_scope: req.workspace_id.to_string(),
            mode: req.mode.as_str().to_string(),
            freshness_requirement: freshness_label.clone(),
        });
        let cache_key = SearchCacheKey {
            query_fingerprint: fp.clone(),
            profile_version: profile.version.clone(),
            freshness_policy_version: self.deps.freshness.version.clone(),
            source_policy_version: self.deps.source.version.clone(),
            language: analysis.language.clone(),
            workspace_scope: req.workspace_id.to_string(),
            mode: req.mode.as_str().to_string(),
            freshness_requirement: freshness_label.clone(),
            key_version: CACHE_KEY_VERSION.to_string(),
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
                } else if let Ok(mut resp) =
                    serde_json::from_value::<SearchResponse>(hit.value.payload.clone())
                {
                    resp.replayed = true;
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

    // Hybrid retrieval + cache layers, then application-level constraint
    // filtering BEFORE any result/evidence is served (Part 30 INV-30-01).
    let source_policy = deps.source.clone().with_constraints(
        &req.constraints.required_domains,
        &req.constraints.excluded_domains,
    );
    let all_hits =
        retrieve_local_candidates(&deps, &req, &analysis, &profile, &source_policy, &fp).await?;
    let freshness_policy = deps.freshness.clone();
    let evals: Vec<LocalCandidateEval> = all_hits
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
    let servable_evals: Vec<LocalCandidateEval> = evals
        .iter()
        .filter(|e| e.freshness_ok && e.source_ok)
        .cloned()
        .collect();
    let servable_hits: Vec<RetrievalCandidate> =
        servable_evals.iter().map(|e| e.candidate.clone()).collect();
    // Only stale-but-allowed candidates may enter the refresh enqueue path;
    // excluded domains are never returned and never refreshed.
    let stale_hits: Vec<RetrievalCandidate> = evals
        .iter()
        .filter(|e| !e.freshness_ok && e.source_ok)
        .map(|e| e.candidate.clone())
        .collect();
    let stale_document_ids: Vec<searchos_domain::ids::DocumentId> =
        stale_hits.iter().map(|c| c.document_id.clone()).collect();
    let local_candidates = servable_evals;
    let max_results = clamp_max_results(req.max_results) as usize;

    // ---- Decision (RFC-004) ------------------------------------------
    let gate = QualityGate::new(
        deps.quality.clone(),
        deps.freshness.clone(),
        source_policy.clone(),
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
    let mut guard = deps
        .cost_sink
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
            // Config-driven query expansion only on a proven coverage gap.
            if deps.query_expansion && !gaps.is_empty() {
                let expander = QueryExpander::new(ExpansionConfig {
                    enabled: true,
                    require_coverage_gap: true,
                    ..Default::default()
                });
                let gap_question = gaps.first().map(|g| g.question.clone()).unwrap_or_default();
                for extra in expander.expand(&req.query, &gap_question) {
                    if !queries.contains(&extra) {
                        queries.push(extra);
                    }
                }
                queries.truncate(req.budget.max_external_queries.max(1) as usize);
            }

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
                    Vec::new()
                }
            };

            // Persist only newly acquired external pages for reuse
            // (Incremental Reuse P-007). Index writes use the one canonical
            // URL implementation and are mirrored into the vector index.
            for item in external_items.iter().take(max_results.min(2)) {
                let payload = IndexedDocument {
                    chunk_id: searchos_domain::ids::ChunkId::generate(),
                    document_id: searchos_domain::ids::DocumentId::generate(),
                    version_id: searchos_domain::ids::DocumentVersionId::generate(),
                    title: item.title.clone(),
                    body: item.content.clone(),
                    url: canonical_url(&item.url),
                    language: req.constraints.language.clone(),
                    workspace_id: req.workspace_id.to_string(),
                    fetched_at: Utc::now(),
                };
                deps.bm25.index(payload.clone()).await.ok();
                if let Some(vector) = &deps.vector {
                    if let Ok(embedding) = embed_text(&deps, &payload.body).await {
                        let _ = vector
                            .upsert(
                                payload.chunk_id.as_str(),
                                payload.document_id.as_str(),
                                embedding,
                                deps.embedder.model_id(),
                            )
                            .await;
                    }
                }
            }
            deps.bm25.count().await.ok();
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
    let spent_micros = cost_spent.amount_micros;
    guard.set_spent(spent_micros);
    guard.settle(spent_micros).await?;

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
    );
    // Shared transport result cap (Part 30 INV-30-19).
    response.results.truncate(max_results);
    response.provenance.truncate(max_results);
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
    /// Executes targeted external queries. Every error carries the spend
    /// already written to the ledger and appends a zero-amount failed
    /// `provider_attempt` event for audit (Part 30 INV-30-06).
    async fn execute_external(
        &self,
        req: &SearchRequest,
        planned_queries: &[String],
        trace_id: &TraceId,
        task_id: &TaskId,
    ) -> Result<(Vec<SearchResultItem>, Vec<Evidence>, u64, i64), ExternalExecutionError> {
        let mut items = Vec::new();
        let mut evidence = Vec::new();
        let mut calls = 0u64;
        let mut spent = Money::zero(&req.budget.currency);
        let max_results = clamp_max_results(req.max_results) as usize;
        let mut provider_id: Option<String> = None;

        macro_rules! fail {
            ($code:expr, $message:expr) => {{
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

        let mut queries: Vec<String> = planned_queries
            .iter()
            .map(|q| q.trim().to_string())
            .filter(|q| !q.is_empty())
            .take(req.budget.max_external_queries.max(1) as usize)
            .collect();
        if queries.is_empty() {
            queries.push(req.query.clone());
        }

        for q in &queries {
            if calls >= req.budget.max_external_queries as u64 {
                break;
            }
            let price = self.estimator.estimate_request(provider.id());
            if spent.amount_micros + price.amount_micros > req.budget.max_total_cost_micros {
                break;
            }
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

            for entry in result.entries.iter().take(max_results) {
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
