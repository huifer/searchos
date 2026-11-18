//! Adapters that wrap the existing retrieval paths as `SearchSource`s
//! (RFC-062 Phase 2).
//!
//! Two rules govern this module:
//!
//! 1. **Behaviour-preserving.** An adapter adds no filtering, no re-scoring
//!    and no truncation of its own beyond what the underlying path already
//!    does. The equivalence gate
//!    (`legacy_pipeline_matches_federated_adapters`) proves local retrieval
//!    through the adapter is identical to retrieval through `local_port`.
//! 2. **No second billing path.** An external adapter never settles cost.
//!    Billing stays where it is today — in the search service's reserve →
//!    execute → settle lifecycle. An adapter that also billed would create a
//!    second ledger entry per provider call.

use async_trait::async_trait;
use searchos_domain::ids::SourceId;
use searchos_domain::retrieval::RetrievalQuery;
use searchos_domain::SearchOsError;
use searchos_retrieval::SearchIndex;

use crate::source::{
    ReliabilityProfile, SearchSource, SearchSourceDescriptor, SourceCandidate, SourceCapabilities,
    SourceCategory, SourceCostProfile, SourceError, SourceExecutionContext, SourceExecutionStatus,
    SourceLatencyProfile, SourceScope, SourceSearchRequest, SourceSearchResponse,
};

/// The local knowledge index wrapped as a federated source.
///
/// `bm25` is the same `Arc<dyn SearchIndex>` the runtime uses for local
/// retrieval (`storage.fulltext`, ADR-027), so a remote full-text backend is
/// federated automatically — no second implementation to drift.
pub struct LocalIndexSource {
    descriptor: SearchSourceDescriptor,
    index: std::sync::Arc<dyn SearchIndex>,
}

impl LocalIndexSource {
    /// Source id used when a caller does not name one.
    pub const DEFAULT_ID: &'static str = "local_knowledge";

    pub fn new(index: std::sync::Arc<dyn SearchIndex>, scope: SourceScope, priority: i32) -> Self {
        Self::with_id(Self::DEFAULT_ID, index, scope, priority)
    }

    pub fn with_id(
        id: &str,
        index: std::sync::Arc<dyn SearchIndex>,
        scope: SourceScope,
        priority: i32,
    ) -> Self {
        Self {
            descriptor: SearchSourceDescriptor {
                source_id: SourceId::new(id),
                name: "local knowledge index".into(),
                category: SourceCategory::LocalKnowledge,
                version: "1".into(),
                enabled: true,
                priority,
                // The index does lexical matching and applies the domain and
                // freshness predicates itself; it does not fetch whole
                // documents, paginate, or sync incrementally through this
                // port.
                capabilities: SourceCapabilities {
                    lexical_search: true,
                    semantic_search: false,
                    metadata_filter: true,
                    time_filter: true,
                    freshness_metadata: true,
                    provenance_metadata: true,
                    permission_filtering: true,
                    ..Default::default()
                },
                reliability: ReliabilityProfile {
                    // Local: free, in-process, never billable.
                    success_rate: 1.0,
                    metadata_trust: 0.8,
                    consecutive_failures: 0,
                    circuit_open: false,
                    ..ReliabilityProfile::default()
                },
                cost: SourceCostProfile {
                    estimated_cost_micros: 0,
                    currency: "USD".into(),
                    billable: false,
                },
                latency: SourceLatencyProfile {
                    p50_ms: 5,
                    p95_ms: 40,
                    timeout_ms: 2_000,
                },
                scope,
                satisfies_requirements: Vec::new(),
            },
            index,
        }
    }

    /// Declares the requirement ids this source is authoritative for, so the
    /// planner can select it to fill an open gap instead of only as a
    /// free filler. Wiring supplies these from the gap analyzer.
    pub fn with_satisfies_requirements(
        mut self,
        requirements: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.descriptor.satisfies_requirements =
            requirements.into_iter().map(|r| r.into()).collect();
        self
    }

    /// Builds the retrieval query from a federated request.
    ///
    /// Kept separate so the equivalence test can compare it against the
    /// service's own query construction.
    pub fn retrieval_query(request: &SourceSearchRequest, top_k: usize) -> RetrievalQuery {
        let mut query = RetrievalQuery::new(request.query.clone(), top_k);
        query.workspace_id = Some(request.workspace_id.to_string());
        // Same policy the service applies: stale rows stay in the pool for
        // the refresh path, and the caller filters what is servable
        // (Part 30 INV-30-01). An adapter must not decide that alone.
        query.include_stale = true;
        query.fetched_after = request.fetched_after;
        query.required_domains = request.required_domains.clone();
        query.excluded_domains = request.excluded_domains.clone();
        query
    }
}

#[async_trait]
impl SearchSource for LocalIndexSource {
    fn descriptor(&self) -> SearchSourceDescriptor {
        self.descriptor.clone()
    }

    async fn search(
        &self,
        request: SourceSearchRequest,
        _context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, SourceError> {
        let started = std::time::Instant::now();
        let query = Self::retrieval_query(&request, request.max_results.max(1));
        let candidates = self
            .index
            .search(&query)
            .await
            .map_err(|error| map_index_error(&error))?;
        let mapped: Vec<SourceCandidate> = candidates
            .into_iter()
            .map(|candidate| SourceCandidate {
                source_id: self.descriptor.source_id.clone(),
                canonical_ref: candidate.url.clone(),
                title: candidate.title.clone(),
                snippet: candidate.snippet.clone(),
                raw_score: Some(candidate.score),
                observed_at: candidate.fetched_at,
                published_at: None,
                satisfies_requirements: Vec::new(),
                // The index applies the workspace predicate on the query
                // itself, so permission is enforced at the source.
                permission_verified: true,
            })
            .collect();
        Ok(SourceSearchResponse {
            source_id: self.descriptor.source_id.clone(),
            status: SourceExecutionStatus::Success,
            candidates: mapped,
            failure: None,
            estimated_cost_micros: 0,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }
}

fn map_index_error(error: &SearchOsError) -> SourceError {
    match error {
        SearchOsError::Timeout(message) => SourceError::Timeout(message.clone()),
        SearchOsError::Security(message) => SourceError::PermissionDenied(message.clone()),
        other => SourceError::Failed(other.to_string()),
    }
}

/// One configured web-search provider wrapped as a federated source.
///
/// The adapter deliberately does **not** bill. Cost accounting stays in the
/// search service's reserve → execute → settle lifecycle (RFC-008 /
/// INV-30-12); a source that also wrote a cost event would double every
/// provider call. `estimated_cost_micros` is reported for planning only, and
/// it is an estimate — never an invoice (P-005).
pub struct WebSearchProviderSource {
    descriptor: SearchSourceDescriptor,
    provider: std::sync::Arc<dyn searchos_provider::traits::SearchProvider>,
    estimated_cost_micros: i64,
}

impl WebSearchProviderSource {
    pub fn new(
        provider: std::sync::Arc<dyn searchos_provider::traits::SearchProvider>,
        scope: SourceScope,
        priority: i32,
        estimated_cost_micros: i64,
        latency: SourceLatencyProfile,
    ) -> Self {
        let capabilities = provider.capabilities();
        let id = provider.id().to_string();
        Self {
            descriptor: SearchSourceDescriptor {
                source_id: SourceId::new(id.clone()),
                name: format!("web search provider {id}"),
                category: SourceCategory::WebSearch,
                version: "1".into(),
                enabled: true,
                priority,
                capabilities: SourceCapabilities {
                    lexical_search: capabilities.search,
                    semantic_search: capabilities.semantic_search,
                    // Domain/date policy may be honoured upstream only when
                    // the adapter declares it; otherwise the client-side
                    // filter below applies it. Declaring `metadata_filter`
                    // when the provider ignores the params would let the
                    // planner trust a filter that never ran.
                    metadata_filter: capabilities.supports_domain_filter,
                    time_filter: capabilities.supports_date_filter,
                    freshness_metadata: capabilities.supports_date_filter,
                    // Providers do not know about SearchOS workspaces.
                    permission_filtering: false,
                    provenance_metadata: true,
                    // Tavily/Brave both return a bounded result list; the
                    // port has no page cursor.
                    pagination: false,
                    ..Default::default()
                },
                reliability: ReliabilityProfile::default(),
                cost: SourceCostProfile {
                    estimated_cost_micros,
                    currency: "USD".into(),
                    // Every external provider call can produce a real bill.
                    billable: true,
                },
                latency,
                scope,
                satisfies_requirements: Vec::new(),
            },
            provider,
            estimated_cost_micros,
        }
    }

    /// Applies the domain policy to returned entries on the client side.
    ///
    /// It runs unconditionally rather than only when the provider declares
    /// no support: the matching rule here (exact host or subdomain) is the
    /// same one the local retrieval layer uses (`candidate_allowed`), so the
    /// two paths cannot disagree about what a domain policy means. When the
    /// provider already honoured the filter this is a no-op; when it did not,
    /// it is the only enforcement that exists.
    fn domain_filter(
        &self,
        entry: &searchos_provider::traits::SearchResultEntry,
        request: &SourceSearchRequest,
    ) -> bool {
        let Some(host) = host_of(&entry.url) else {
            return request.required_domains.is_empty();
        };
        if request
            .excluded_domains
            .iter()
            .any(|d| domain_matches(&host, d))
        {
            return false;
        }
        if request.required_domains.is_empty() {
            return true;
        }
        request
            .required_domains
            .iter()
            .any(|d| domain_matches(&host, d))
    }
}

impl WebSearchProviderSource {
    /// Seeds the reliability profile from the provider registry's observed
    /// stats, so the planner's reliability stage reasons over the same
    /// history the router already uses (RFC-039/074) instead of a default.
    pub fn with_reliability(mut self, success_rate: f32, circuit_open: bool) -> Self {
        self.descriptor.reliability.success_rate = success_rate;
        self.descriptor.reliability.circuit_open = circuit_open;
        self
    }

    /// Declares the requirement ids this provider is authoritative for.
    ///
    /// The kernel deliberately does not read a provider's private state to
    /// guess this: the caller (wiring / gap analysis) decides which source is
    /// on the hook for which requirement. Selective planning spends money
    /// precisely because it can see that decision.
    pub fn with_satisfies_requirements(
        mut self,
        requirements: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.descriptor.satisfies_requirements =
            requirements.into_iter().map(|r| r.into()).collect();
        self
    }
}

#[async_trait]
impl SearchSource for WebSearchProviderSource {
    fn descriptor(&self) -> SearchSourceDescriptor {
        self.descriptor.clone()
    }

    async fn search(
        &self,
        request: SourceSearchRequest,
        _context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, SourceError> {
        let params = searchos_provider::traits::SearchParams {
            query: request.query.clone(),
            max_results: request.max_results.max(1) as u32,
            required_domains: request.required_domains.clone(),
            excluded_domains: request.excluded_domains.clone(),
            language: None,
            freshness_days: request.fetched_after.map(|after| {
                let seconds = (chrono::Utc::now() - after).num_seconds().max(0);
                (seconds / 86_400).max(1) as u32
            }),
            include_raw_content: false,
        };
        match self.provider.search(&params).await {
            Ok(result) => {
                let candidates: Vec<SourceCandidate> = result
                    .entries
                    .iter()
                    .filter(|entry| self.domain_filter(entry, &request))
                    .map(|entry| SourceCandidate {
                        source_id: self.descriptor.source_id.clone(),
                        canonical_ref: entry.url.clone(),
                        title: entry.title.clone(),
                        snippet: entry.content.clone(),
                        raw_score: Some(entry.score),
                        observed_at: chrono::Utc::now(),
                        published_at: entry.published_at,
                        satisfies_requirements: Vec::new(),
                        // Permission filtering is not this source's job; the
                        // caller must not place these candidates in a context
                        // without an explicit permission check.
                        permission_verified: false,
                    })
                    .collect();
                Ok(SourceSearchResponse {
                    source_id: self.descriptor.source_id.clone(),
                    status: SourceExecutionStatus::Success,
                    candidates,
                    failure: None,
                    estimated_cost_micros: self.estimated_cost_micros,
                    latency_ms: result.duration_ms,
                })
            }
            Err(error) => Err(map_provider_error(&error)),
        }
    }

    /// Probes the real provider; a probe failure is reported as `Down`
    /// (bookkeeping), never as a request failure.
    async fn health_check(&self) -> crate::source::SourceHealth {
        match self.provider.health_check().await {
            Ok(true) => crate::source::SourceHealth::Healthy,
            Ok(false) => crate::source::SourceHealth::Degraded,
            Err(_) => crate::source::SourceHealth::Down,
        }
    }
}

fn map_provider_error(error: &searchos_provider::error::ProviderError) -> SourceError {
    use searchos_provider::error::ProviderErrorKind as Kind;
    let message = format!("{}: {}", error.kind.code(), error.message);
    match error.kind {
        Kind::Timeout => SourceError::Timeout(message),
        Kind::RateLimit | Kind::QuotaExhausted => SourceError::RateLimited(message),
        Kind::ProviderUnavailable | Kind::NetworkError => SourceError::Unavailable(message),
        Kind::AuthenticationError => SourceError::PermissionDenied(message),
        Kind::InvalidResponse => SourceError::Failed(message),
    }
}

/// Host extraction for the domain policy.
///
/// Deliberately identical to `searchos_retrieval::host_of` — the same
/// function the local `candidate_allowed` filter uses — so the local and
/// external paths cannot disagree about what a domain policy means. It does
/// not special-case userinfo: neither does the local path, and a divergence
/// here would be a silent policy difference, not a fix.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    Some(host.split(':').next()?.to_lowercase())
}

/// Host matches a domain policy entry exactly or as a subdomain.
pub fn domain_matches(host: &str, domain: &str) -> bool {
    let domain = domain.to_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}
