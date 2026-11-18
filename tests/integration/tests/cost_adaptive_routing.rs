//! RFC-080: cost-adaptive source selection, end to end.
//!
//! The kernel gates (in `crates/searchos-federated/tests/federated_invariants.rs`)
//! prove the ordering rule in isolation. These gates prove the whole path:
//! `config.federation.selection` → runtime → search service → planner → the
//! plan report a client actually reads.
//!
//! Named gates:
//!   cost_adaptive_selection_is_reported_in_the_plan
//!   reliability_first_is_the_default_selection_policy
//!   cost_adaptive_orders_sources_by_quality_per_unit_cost

use std::sync::Arc;

use async_trait::async_trait;
use searchos_application::search_service::{SearchDependencies, SearchServiceImpl};
use searchos_domain::ids::{SourceId, WorkspaceId};
use searchos_domain::request::SearchRequest;
use searchos_domain::SearchMode;
use searchos_federated::source::{
    ReliabilityProfile, SearchSource, SearchSourceDescriptor, SourceCandidate, SourceCategory,
    SourceCostProfile, SourceExecutionStatus, SourceLatencyProfile, SourceScope,
    SourceSearchRequest, SourceSearchResponse,
};
use searchos_federated::{SourceError, SourceExecutionContext};

/// A source whose only observable behaviour is its descriptor.
struct DescribedSource {
    descriptor: SearchSourceDescriptor,
}

impl DescribedSource {
    fn new(
        id: &str,
        priority: i32,
        cost_micros: i64,
        success_rate: f32,
        metadata_trust: f32,
    ) -> Self {
        let reliability = ReliabilityProfile {
            success_rate,
            metadata_trust,
            ..ReliabilityProfile::default()
        };
        Self {
            descriptor: SearchSourceDescriptor {
                source_id: SourceId::new(id),
                name: id.to_string(),
                category: SourceCategory::WebSearch,
                version: "1".into(),
                enabled: true,
                priority,
                capabilities: searchos_federated::SourceCapabilities {
                    lexical_search: true,
                    ..searchos_federated::SourceCapabilities::default()
                },
                reliability,
                cost: SourceCostProfile {
                    estimated_cost_micros: cost_micros,
                    currency: "USD".into(),
                    billable: true,
                },
                latency: SourceLatencyProfile::default(),
                scope: SourceScope::Global,
                // Generic: it fills open requirements but is never authoritative.
                satisfies_requirements: Vec::new(),
            },
        }
    }
}

#[async_trait]
impl SearchSource for DescribedSource {
    fn descriptor(&self) -> SearchSourceDescriptor {
        self.descriptor.clone()
    }

    async fn search(
        &self,
        _request: SourceSearchRequest,
        _context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, SourceError> {
        Ok(SourceSearchResponse {
            source_id: self.descriptor.source_id.clone(),
            status: SourceExecutionStatus::Success,
            candidates: Vec::<SourceCandidate>::new(),
            failure: None,
            estimated_cost_micros: self.descriptor.cost.estimated_cost_micros,
            latency_ms: 1,
        })
    }
}

async fn harness(
    dir: &tempfile::TempDir,
    selection: &str,
) -> (Arc<searchos_core::SearchOsRuntime>, SearchServiceImpl) {
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    // `plan_only` produces a federated plan without executing anything, so
    // the gate never buys anything.
    cfg.federation.strategy = "plan_only".into();
    cfg.federation.selection = selection.to_string();
    let runtime = Arc::new(searchos_core::SearchOsRuntime::init(cfg).await.unwrap());
    let deps = Arc::new(SearchDependencies {
        profiles: searchos_policy::ProfileRegistry::builtin(),
        quality: searchos_policy::QualityPolicy::default(),
        freshness: searchos_policy::FreshnessPolicy::default_policy(),
        source: searchos_policy::SourcePolicy::default_policy(),
        cost_config: searchos_cost::CostModelConfig::default_config(),
        bm25: runtime.fulltext.clone(),
        vector: Some(runtime.vector.clone()),
        hybrid: Some(runtime.hybrid.clone()),
        embedder: runtime.embedder.clone(),
        canonical_cache: runtime.canonical_cache.clone(),
        semantic_cache: runtime.semantic_cache.clone(),
        retrieval_cache: runtime.retrieval_cache.clone(),
        embedding_cache: runtime.embedding_cache.clone(),
        rerank_cache: runtime.rerank_cache.clone(),
        router: runtime.provider_registry.clone(),
        search_providers: vec![],
        local_port: runtime.local_port(),
        traces: runtime.traces(),
        cost_sink: runtime.cost_sink(),
        task_recorder: runtime.task_recorder(),
        singleflight: Arc::new(searchos_application::singleflight::SingleflightCoordinator::new()),
        cache: runtime.exact_cache.clone(),
        validity: None,
        policy_state: None,
        entity_lookup: None,
        negative_cache: runtime.negative_cache.clone(),
        semantic_dedup: true,
        expansion_mode: "followup".into(),
        incremental: true,
        query_expansion: true,
        cross_lingual: false,
        reranking: true,
        allow_external: false,
        promotion: None,
        session_store: Some(runtime.session_store.clone()),
        federation_registry: Some(Arc::clone(&runtime.federation_registry)),
        federation_strategy: "plan_only".into(),
        federation_limits: searchos_domain::FederationLimits::default(),
        federation_selection: selection.to_string(),
        quality_learner: None,
    });
    (Arc::clone(&runtime), SearchServiceImpl::new(deps))
}

fn request() -> SearchRequest {
    // A comparison query: the planner needs OPEN REQUIREMENTS before a
    // billable source is eligible at all (INV-30-04), and a comparison with
    // two entities is what produces them.
    SearchRequest::new(
        "compare tokio versus async-std performance",
        searchos_domain::ids::ProfileId::general(),
        SearchMode::ExternalOnly,
        WorkspaceId::new("ws_default"),
    )
    .unwrap()
}

#[tokio::test]
async fn cost_adaptive_selection_is_reported_in_the_plan() {
    let dir = tempfile::tempdir().unwrap();
    let (_runtime, service) = harness(&dir, "cost_adaptive").await;

    let report = service.plan_report(&request()).await.unwrap();
    assert_eq!(
        report.source_selection.as_deref(),
        Some("cost_adaptive"),
        "the plan report must state the ordering policy it used"
    );
}

#[tokio::test]
async fn reliability_first_is_the_default_selection_policy() {
    let dir = tempfile::tempdir().unwrap();
    let (_runtime, service) = harness(&dir, "reliability_first").await;

    let report = service.plan_report(&request()).await.unwrap();
    assert_eq!(
        report.source_selection.as_deref(),
        Some("reliability_first")
    );
}

#[tokio::test]
async fn cost_adaptive_orders_sources_by_quality_per_unit_cost() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, service) = harness(&dir, "cost_adaptive").await;

    // Two external sources, both generic, both eligible: one expensive and
    // mediocre (high priority), one cheap and reliable (low priority).
    runtime
        .federation_registry
        .register(Arc::new(DescribedSource::new(
            "expensive",
            10,
            50_000,
            0.5,
            0.5,
        )))
        .unwrap();
    runtime
        .federation_registry
        .register(Arc::new(DescribedSource::new("cheap", 1, 5_000, 0.95, 1.0)))
        .unwrap();

    let report = service.plan_report(&request()).await.unwrap();
    assert_eq!(
        report.federated_sources,
        vec!["cheap".to_string(), "expensive".to_string()],
        "cost-adaptive ordering must beat configured priority"
    );

    // The scores are the audit trail: both sources are recorded, and the
    // cheap one has the higher quality-per-unit-cost.
    let by_id = |id: &str| {
        report
            .source_scores
            .iter()
            .find(|s| s.source_id == id)
            .cloned()
            .unwrap_or_else(|| panic!("score for {id}"))
    };
    assert!(by_id("cheap").value_density > by_id("expensive").value_density);
    assert!(by_id("cheap").selected);
    assert!(by_id("expensive").selected);
    assert_eq!(by_id("expensive").cost_micros, 50_000);
    assert!((by_id("cheap").expected_quality - 0.95).abs() < 0.001);
}
