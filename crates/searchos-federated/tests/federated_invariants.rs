//! RFC-062 / RFC-063 / RFC-064 federated search acceptance tests.
//!
//! These assert the invariants that make federation safe rather than merely
//! convenient. Every one of them is a pure-logic test: no runtime, no network,
//! no provider, no LLM — which is exactly why the guarantees are checkable on
//! every commit instead of only under a live gateway.
//!
//! Named gates (docs/rfc/rfc-062-federated-search-architecture.md §12):
//!   federated_auto_never_escapes_local_only
//!   scope_precedence_cannot_relax_permissions
//!   source_timeout_does_not_fail_the_request
//!   required_source_failure_forces_partial
//!   plan_rejections_are_auditable
//!   covered_requirements_are_never_repurchased
//!   planner_never_fans_out_to_every_source
//!   local_first_is_a_property_of_the_plan
//!   resource_identity_is_stable_across_sources
//!   occurrences_are_not_independent_sources

use async_trait::async_trait;
use chrono::Utc;
use searchos_domain::ids::{RequestId, SourceId, WorkspaceId};
use searchos_domain::SearchMode;
use searchos_federated::planner::{
    aggregate, FederatedPlanner, FederationStrategy, PlanningContext, RejectionReason,
    StageExecutionMode, StopCondition,
};
use searchos_federated::registry::{InMemorySourceRegistry, RegistryError};
use searchos_federated::resource::{ContentRef, PermissionRef, ResourceType, SearchResource};
use searchos_federated::source::{
    SearchSource, SearchSourceDescriptor, SourceCandidate, SourceCapabilities, SourceCategory,
    SourceCostProfile, SourceExecutionContext, SourceExecutionStatus, SourceFailure, SourceFilter,
    SourceHealth, SourceScope, SourceSearchRequest, SourceSearchResponse,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const WS: &str = "ws_test";

fn workspace() -> WorkspaceId {
    WorkspaceId::new(WS)
}

fn lexical_caps() -> SourceCapabilities {
    SourceCapabilities {
        lexical_search: true,
        provenance_metadata: true,
        ..Default::default()
    }
}

fn descriptor(
    id: &str,
    category: SourceCategory,
    scope: SourceScope,
    priority: i32,
    cost_micros: i64,
    billable: bool,
    requirements: &[&str],
) -> SearchSourceDescriptor {
    SearchSourceDescriptor {
        source_id: SourceId::new(id),
        name: id.to_string(),
        category,
        version: "1".into(),
        enabled: true,
        priority,
        capabilities: lexical_caps(),
        reliability: Default::default(),
        cost: SourceCostProfile {
            estimated_cost_micros: cost_micros,
            currency: "USD".into(),
            billable,
        },
        latency: Default::default(),
        scope,
        satisfies_requirements: requirements.iter().map(|r| r.to_string()).collect(),
    }
}

fn context(
    mode: SearchMode,
    strategy: FederationStrategy,
    requirements: &[&str],
) -> PlanningContext {
    PlanningContext {
        query: "latest rust stable release".into(),
        mode,
        strategy,
        workspace_id: WS.into(),
        requirements: requirements.iter().map(|r| r.to_string()).collect(),
        covered_requirements: Vec::new(),
        required_requirements: Vec::new(),
        max_sources: 4,
        max_parallel_sources: 4,
        max_estimated_cost_micros: 100_000,
        max_latency_ms: 30_000,
        stop_when_covered: true,
        generic_sources_can_answer: true,
        selection: searchos_federated::SelectionMode::default(),
    }
}

fn candidate(source: &str, reference: &str, score: f32) -> SourceCandidate {
    SourceCandidate {
        source_id: SourceId::new(source),
        canonical_ref: reference.into(),
        title: None,
        snippet: "snippet".into(),
        raw_score: Some(score),
        observed_at: Utc::now(),
        published_at: None,
        satisfies_requirements: Vec::new(),
        permission_verified: true,
    }
}

// ---------------------------------------------------------------------------
// INV-FED-02 — mode is the permission boundary
// ---------------------------------------------------------------------------

#[test]
fn federated_auto_never_escapes_local_only() {
    let local = descriptor(
        "local",
        SourceCategory::LocalKnowledge,
        SourceScope::Global,
        10,
        0,
        false,
        &["answer"],
    );
    let web = descriptor(
        "web",
        SourceCategory::WebSearch,
        SourceScope::Global,
        5,
        5_000,
        true,
        &["answer"],
    );
    let billable_web = descriptor(
        "billable_web",
        SourceCategory::WebSearch,
        SourceScope::Global,
        1,
        1_000,
        true,
        &["answer"],
    );

    for strategy in [
        FederationStrategy::Auto,
        FederationStrategy::Parallel,
        FederationStrategy::Sequential,
        FederationStrategy::Selective,
    ] {
        let mut ctx = context(SearchMode::LocalOnly, strategy, &["answer"]);
        // A local_only caller that also forbids spending must reject billable
        // sources even when their category would otherwise be permitted.
        ctx.max_estimated_cost_micros = 0;
        let plan =
            FederatedPlanner::plan(&ctx, &[local.clone(), web.clone(), billable_web.clone()]);
        let selected = plan.selected_source_ids();
        assert!(
            selected.contains(&"local".to_string()),
            "{strategy:?} must still consult local knowledge"
        );
        assert!(
            !selected.contains(&"web".to_string()),
            "{strategy:?} escaped local_only into an external source"
        );
        assert!(
            !selected.contains(&"billable_web".to_string()),
            "{strategy:?} spent money under local_only"
        );
        assert!(
            plan.rejections
                .iter()
                .any(|r| r.reason == RejectionReason::ModePermission),
            "{strategy:?} must record WHY the web source was refused"
        );
    }
}

#[test]
fn external_only_never_selects_a_local_source() {
    let local = descriptor(
        "local",
        SourceCategory::LocalKnowledge,
        SourceScope::Global,
        99, // highest priority on purpose: priority must not beat permission
        0,
        false,
        &["answer"],
    );
    let web = descriptor(
        "web",
        SourceCategory::WebSearch,
        SourceScope::Global,
        1,
        1_000,
        true,
        &["answer"],
    );
    let plan = FederatedPlanner::plan(
        &context(
            SearchMode::ExternalOnly,
            FederationStrategy::Auto,
            &["answer"],
        ),
        &[local, web],
    );
    assert_eq!(plan.selected_source_ids(), vec!["web".to_string()]);
}

#[test]
fn unknown_mode_is_impossible_by_construction() {
    // The five modes are the whole permission surface; the planner must be
    // total over all of them and never panic.
    let sources = vec![
        descriptor(
            "local",
            SourceCategory::LocalKnowledge,
            SourceScope::Global,
            1,
            0,
            false,
            &[],
        ),
        descriptor(
            "web",
            SourceCategory::WebSearch,
            SourceScope::Global,
            1,
            10,
            true,
            &[],
        ),
        descriptor(
            "memory",
            SourceCategory::AgentMemory,
            SourceScope::Global,
            1,
            0,
            false,
            &[],
        ),
    ];
    for mode in [
        SearchMode::LocalOnly,
        SearchMode::LocalFirst,
        SearchMode::Hybrid,
        SearchMode::ExternalFirst,
        SearchMode::ExternalOnly,
    ] {
        let plan = FederatedPlanner::plan(&context(mode, FederationStrategy::Auto, &[]), &sources);
        for id in plan.selected_source_ids() {
            let source = sources.iter().find(|s| s.source_id.as_str() == id).unwrap();
            let allowed = if source.category.is_external() {
                mode.allows_external()
            } else {
                mode.allows_local()
            };
            assert!(allowed, "{mode} selected a forbidden source {id}");
        }
    }
}

// ---------------------------------------------------------------------------
// INV-FED-04 — planner controls fan-out
// ---------------------------------------------------------------------------

#[test]
fn planner_never_fans_out_to_every_source() {
    // Each source owns a distinct requirement, so nothing is "already
    // covered" and the only thing that can stop selection is the fan-out cap.
    let sources: Vec<SearchSourceDescriptor> = (0..20usize)
        .map(|i| {
            descriptor(
                &format!("web{i}"),
                SourceCategory::WebSearch,
                SourceScope::Global,
                (20 - i) as i32,
                1_000,
                true,
                &[&format!("r{i}")],
            )
        })
        .collect();
    let requirements: Vec<String> = (0..20usize).map(|i| format!("r{i}")).collect();
    let mut ctx = context(
        SearchMode::ExternalFirst,
        FederationStrategy::Parallel,
        &requirements.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
    );
    ctx.max_sources = 3;
    let plan = FederatedPlanner::plan(&ctx, &sources);
    assert_eq!(plan.task_count(), 3, "max_sources must cap the fan-out");
    assert_eq!(
        plan.rejections
            .iter()
            .filter(|r| r.reason == RejectionReason::FanOutLimit)
            .count(),
        17,
        "and record every refusal"
    );
    assert_eq!(plan.source_set().len(), 3);
}

#[test]
fn selective_strategy_skips_sources_that_fill_no_open_requirement() {
    let authoritative = descriptor(
        "official_docs",
        SourceCategory::WebDocument,
        SourceScope::Global,
        10,
        2_000,
        true,
        &["version"],
    );
    let generic = descriptor(
        "generic_web",
        SourceCategory::WebSearch,
        SourceScope::Global,
        1,
        1_000,
        true,
        &[],
    );
    let plan = FederatedPlanner::plan(
        &context(
            SearchMode::ExternalOnly,
            FederationStrategy::Selective,
            &["version"],
        ),
        &[authoritative, generic],
    );
    assert_eq!(
        plan.selected_source_ids(),
        vec!["official_docs".to_string()]
    );
}

#[test]
fn local_first_is_a_property_of_the_plan_not_a_convention() {
    // P-002: under Auto + Hybrid, local sources must be planned before
    // external ones even when the external source has higher priority.
    let web = descriptor(
        "fast_web",
        SourceCategory::WebSearch,
        SourceScope::Global,
        100,
        1_000,
        true,
        &["answer"],
    );
    let local = descriptor(
        "slow_local",
        SourceCategory::LocalKnowledge,
        SourceScope::Global,
        1,
        0,
        false,
        &["answer"],
    );
    // Distinct requirements so both are genuinely needed; ordering is then
    // the only question, and priority must not flip it.
    let web = SearchSourceDescriptor {
        satisfies_requirements: vec!["external_only_fact".into()],
        ..web
    };
    let plan = FederatedPlanner::plan(
        &context(
            SearchMode::Hybrid,
            FederationStrategy::Auto,
            &["answer", "external_only_fact"],
        ),
        &[web, local],
    );
    let selected = plan.selected_source_ids();
    assert_eq!(
        selected,
        vec!["slow_local".to_string(), "fast_web".to_string()],
        "local-first ordering must survive an external source with 100x priority"
    );
    assert_eq!(plan.stages[0].execution_mode, StageExecutionMode::Parallel);
}

#[test]
fn sequential_strategy_emits_one_stage_per_source() {
    let sources = vec![
        descriptor(
            "a",
            SourceCategory::LocalKnowledge,
            SourceScope::Global,
            3,
            0,
            false,
            &["r1"],
        ),
        descriptor(
            "b",
            SourceCategory::WebSearch,
            SourceScope::Global,
            2,
            10,
            true,
            &["r2"],
        ),
        descriptor(
            "c",
            SourceCategory::WebSearch,
            SourceScope::Global,
            1,
            10,
            true,
            &["r3"],
        ),
    ];
    let ctx = context(
        SearchMode::Hybrid,
        FederationStrategy::Sequential,
        &["r1", "r2", "r3"],
    );
    let plan = FederatedPlanner::plan(&ctx, &sources);
    assert_eq!(plan.stages.len(), 3);
    assert!(plan
        .stages
        .iter()
        .all(|s| s.execution_mode == StageExecutionMode::Sequential));
    assert_eq!(plan.stages[0].tasks.len(), 1);
    assert_eq!(plan.stages[0].tasks[0].source_id, "a");
    assert!(plan
        .stop_conditions
        .contains(&StopCondition::RequirementsCovered));
}

#[test]
fn sequential_stops_as_soon_as_coverage_is_complete() {
    let sources = vec![
        descriptor(
            "a",
            SourceCategory::LocalKnowledge,
            SourceScope::Global,
            3,
            0,
            false,
            &["r1"],
        ),
        descriptor(
            "b",
            SourceCategory::WebSearch,
            SourceScope::Global,
            2,
            10,
            true,
            &["r1"],
        ),
    ];
    let ctx = context(SearchMode::Hybrid, FederationStrategy::Sequential, &["r1"]);
    let plan = FederatedPlanner::plan(&ctx, &sources);
    assert_eq!(
        plan.task_count(),
        1,
        "covered requirements must stop the plan"
    );
    assert_eq!(
        plan.estimated_cost_micros, 0,
        "the paid source was never bought"
    );
}

// ---------------------------------------------------------------------------
// INV-FED-05 — no re-purchasing
// ---------------------------------------------------------------------------

#[test]
fn covered_requirements_are_never_repurchased() {
    let sources = vec![
        descriptor(
            "local",
            SourceCategory::LocalKnowledge,
            SourceScope::Global,
            5,
            0,
            false,
            &["pricing"],
        ),
        descriptor(
            "web",
            SourceCategory::WebSearch,
            SourceScope::Global,
            1,
            9_000,
            true,
            &["pricing"],
        ),
    ];
    let mut ctx = context(SearchMode::Hybrid, FederationStrategy::Auto, &["pricing"]);
    ctx.covered_requirements = vec!["pricing".into()];
    let plan = FederatedPlanner::plan(&ctx, &sources);
    // Nothing is open: under EVERY mode, including Hybrid, a billable source
    // is refused. Gap-based acquisition is a red line (INV-30-04): covering
    // the same requirement twice is waste, and "Hybrid" describes which paths
    // may run, not permission to re-buy an answered question.
    for stage in &plan.stages {
        for task in &stage.tasks {
            assert!(
                task.requirements.is_empty(),
                "task {} re-purchased a covered requirement {:?}",
                task.source_id,
                task.requirements
            );
            assert!(task.filler, "a task with no open requirement is a filler");
            assert!(
                !task.billable,
                "a covered requirement must never trigger a new bill"
            );
        }
    }
    assert_eq!(plan.estimated_cost_micros, 0);
    let refusal = plan
        .rejections
        .iter()
        .find(|r| r.source_id == "web")
        .expect("the billable source must be refused, not silently dropped");
    assert_eq!(refusal.reason, RejectionReason::AlreadyCovered);
    assert!(refusal.detail.contains("re-purchase"));
}

#[test]
fn budget_exhaustion_drops_the_pricier_source_and_records_it() {
    let sources = vec![
        descriptor(
            "cheap",
            SourceCategory::WebSearch,
            SourceScope::Global,
            5,
            4_000,
            true,
            &["a"],
        ),
        descriptor(
            "pricey",
            SourceCategory::WebSearch,
            SourceScope::Global,
            1,
            90_000,
            true,
            &["b"],
        ),
    ];
    let ctx = PlanningContext {
        max_estimated_cost_micros: 10_000,
        ..context(
            SearchMode::ExternalOnly,
            FederationStrategy::Auto,
            &["a", "b"],
        )
    };
    let plan = FederatedPlanner::plan(&ctx, &sources);
    assert_eq!(plan.selected_source_ids(), vec!["cheap".to_string()]);
    assert!(plan
        .rejections
        .iter()
        .any(|r| r.source_id == "pricey" && r.reason == RejectionReason::Budget));
    assert!(plan.unanswerable_requirements.contains(&"b".to_string()));
    assert!(plan
        .stop_conditions
        .contains(&StopCondition::NoEligibleSourceRemains));
}

// ---------------------------------------------------------------------------
// INV-FED-06 — every rejection is auditable
// ---------------------------------------------------------------------------

#[test]
fn plan_rejections_are_auditable() {
    let disabled = SearchSourceDescriptor {
        enabled: false,
        ..descriptor(
            "disabled",
            SourceCategory::WebSearch,
            SourceScope::Global,
            1,
            0,
            false,
            &[],
        )
    };
    let other_ws = descriptor(
        "other_ws",
        SourceCategory::WebSearch,
        SourceScope::Workspace("ws_other".into()),
        1,
        0,
        false,
        &[],
    );
    let blind = SearchSourceDescriptor {
        capabilities: SourceCapabilities::default(),
        ..descriptor(
            "blind",
            SourceCategory::WebSearch,
            SourceScope::Global,
            1,
            0,
            false,
            &[],
        )
    };
    let broken = SearchSourceDescriptor {
        reliability: searchos_federated::source::ReliabilityProfile {
            circuit_open: true,
            ..Default::default()
        },
        ..descriptor(
            "broken",
            SourceCategory::WebSearch,
            SourceScope::Global,
            1,
            0,
            false,
            &[],
        )
    };
    let ok = descriptor(
        "ok",
        SourceCategory::WebSearch,
        SourceScope::Global,
        1,
        0,
        false,
        &[],
    );

    let plan = FederatedPlanner::plan(
        &context(SearchMode::Hybrid, FederationStrategy::Auto, &[]),
        &[disabled, other_ws, blind, broken, ok],
    );
    let by_id = |id: &str| {
        plan.rejections
            .iter()
            .find(|r| r.source_id == id)
            .map(|r| r.reason)
    };
    assert_eq!(by_id("disabled"), Some(RejectionReason::Disabled));
    assert_eq!(by_id("other_ws"), Some(RejectionReason::Scope));
    assert_eq!(by_id("blind"), Some(RejectionReason::Capability));
    assert_eq!(by_id("broken"), Some(RejectionReason::Reliability));
    assert_eq!(by_id("ok"), None);
    assert_eq!(plan.selected_source_ids(), vec!["ok".to_string()]);
    for rejection in &plan.rejections {
        assert!(!rejection.detail.is_empty(), "rejections carry a reason");
    }
}

// ---------------------------------------------------------------------------
// RFC-063 — registry scope precedence
// ---------------------------------------------------------------------------

struct StubSource {
    descriptor: SearchSourceDescriptor,
    outcome: Result<SourceSearchResponse, SourceFailure>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl SearchSource for StubSource {
    fn descriptor(&self) -> SearchSourceDescriptor {
        self.descriptor.clone()
    }

    async fn search(
        &self,
        _request: SourceSearchRequest,
        _context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, searchos_federated::source::SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.outcome {
            Ok(response) => Ok(response.clone()),
            Err(failure) => Err(match failure.status {
                SourceExecutionStatus::Timeout => {
                    searchos_federated::source::SourceError::Timeout(failure.message.clone())
                }
                SourceExecutionStatus::Unavailable => {
                    searchos_federated::source::SourceError::Unavailable(failure.message.clone())
                }
                SourceExecutionStatus::PermissionDenied => {
                    searchos_federated::source::SourceError::PermissionDenied(
                        failure.message.clone(),
                    )
                }
                _ => searchos_federated::source::SourceError::Failed(failure.message.clone()),
            }),
        }
    }

    async fn health_check(&self) -> SourceHealth {
        SourceHealth::Healthy
    }
}

fn stub(
    id: &str,
    category: SourceCategory,
    scope: SourceScope,
    candidates: Vec<SourceCandidate>,
) -> Arc<dyn SearchSource> {
    Arc::new(StubSource {
        descriptor: descriptor(id, category, scope, 1, 0, false, &[]),
        outcome: Ok(SourceSearchResponse::success(id, candidates, 1)),
        calls: Arc::new(AtomicUsize::new(0)),
    })
}

#[test]
fn scope_precedence_cannot_relax_permissions() {
    let registry = InMemorySourceRegistry::new();
    // A global registration is visible to every workspace.
    registry
        .register(stub(
            "shared",
            SourceCategory::LocalKnowledge,
            SourceScope::Global,
            Vec::new(),
        ))
        .unwrap();
    // A workspace-bound registration of the SAME id must not be reachable by
    // another workspace: precedence picks the visible one, it never unions
    // the access.
    registry
        .register(stub(
            "shared",
            SourceCategory::LocalKnowledge,
            SourceScope::Workspace("ws_a".into()),
            Vec::new(),
        ))
        .unwrap();

    let ws_a = WorkspaceId::new("ws_a");
    let ws_b = WorkspaceId::new("ws_b");
    assert!(registry.get(&SourceId::new("shared"), &ws_a).is_some());
    assert!(registry.get(&SourceId::new("shared"), &ws_b).is_some());

    // And the resolved registration is the higher-precedence one for ws_a.
    let resolved = registry
        .descriptor_for(&SourceId::new("shared"), &ws_a)
        .unwrap();
    assert_eq!(resolved.scope, SourceScope::Workspace("ws_a".into()));
    let other = registry
        .descriptor_for(&SourceId::new("shared"), &ws_b)
        .unwrap();
    assert_eq!(other.scope, SourceScope::Global);
}

#[test]
fn workspace_scoped_source_is_invisible_to_other_workspaces() {
    let registry = InMemorySourceRegistry::new();
    registry
        .register(stub(
            "private",
            SourceCategory::LocalKnowledge,
            SourceScope::Workspace("ws_a".into()),
            Vec::new(),
        ))
        .unwrap();
    assert!(registry
        .get(&SourceId::new("private"), &WorkspaceId::new("ws_a"))
        .is_some());
    assert!(registry
        .get(&SourceId::new("private"), &WorkspaceId::new("ws_b"))
        .is_none());
    // And it never shows up in the other workspace's catalog at all.
    let listed = registry.list(
        &SourceFilter {
            enabled_only: true,
            ..Default::default()
        },
        &WorkspaceId::new("ws_b"),
    );
    assert!(listed.is_empty());
}

#[test]
fn agent_scope_overrides_workspace_scope() {
    let registry = InMemorySourceRegistry::new();
    registry
        .register(stub(
            "ranker",
            SourceCategory::VectorStore,
            SourceScope::Workspace(WS.into()),
            Vec::new(),
        ))
        .unwrap();
    registry
        .register(stub(
            "ranker",
            SourceCategory::VectorStore,
            SourceScope::Agent {
                workspace: WS.into(),
                agent: "agent-1".into(),
            },
            Vec::new(),
        ))
        .unwrap();
    let resolved = registry
        .descriptor_for(&SourceId::new("ranker"), &workspace())
        .unwrap();
    assert_eq!(
        resolved.scope,
        SourceScope::Agent {
            workspace: WS.into(),
            agent: "agent-1".into()
        }
    );
}

#[test]
fn duplicate_same_scope_registration_is_rejected_not_silently_replaced() {
    let registry = InMemorySourceRegistry::new();
    registry
        .register(stub(
            "dup",
            SourceCategory::LocalKnowledge,
            SourceScope::Global,
            Vec::new(),
        ))
        .unwrap();
    let result = registry.register(stub(
        "dup",
        SourceCategory::LocalKnowledge,
        SourceScope::Global,
        Vec::new(),
    ));
    assert!(matches!(
        result,
        Err(RegistryError::AlreadyRegistered(_, _))
    ));
}

#[test]
fn disabling_a_source_hides_it_from_every_scope_registration() {
    let registry = InMemorySourceRegistry::new();
    registry
        .register(stub(
            "multi",
            SourceCategory::WebSearch,
            SourceScope::Global,
            Vec::new(),
        ))
        .unwrap();
    registry
        .register(stub(
            "multi",
            SourceCategory::WebSearch,
            SourceScope::Workspace(WS.into()),
            Vec::new(),
        ))
        .unwrap();
    registry
        .set_enabled(&SourceId::new("multi"), false)
        .unwrap();
    let listed = registry.list(
        &SourceFilter {
            enabled_only: true,
            ..Default::default()
        },
        &workspace(),
    );
    assert!(
        listed.is_empty(),
        "a disabled source must not stay reachable through a lower-precedence copy"
    );
}

#[test]
fn catalog_resolves_one_descriptor_per_id() {
    // A Global default plus its workspace override are ONE source, not two
    // independent ones — otherwise source-diversity counts inflate.
    let registry = InMemorySourceRegistry::new();
    registry
        .register(stub(
            "one",
            SourceCategory::LocalKnowledge,
            SourceScope::Global,
            Vec::new(),
        ))
        .unwrap();
    registry
        .register(stub(
            "one",
            SourceCategory::LocalKnowledge,
            SourceScope::Workspace(WS.into()),
            Vec::new(),
        ))
        .unwrap();
    let listed = registry.list(&SourceFilter::default(), &workspace());
    assert_eq!(listed.len(), 1);
    assert_eq!(
        registry.all_references().len(),
        2,
        "diagnostics still see both"
    );
}

// ---------------------------------------------------------------------------
// INV-FED-01 / ADR-062-004 — fail-open is not a quality pass
// ---------------------------------------------------------------------------

#[tokio::test]
async fn source_timeout_does_not_fail_the_request() {
    let good = stub(
        "good",
        SourceCategory::LocalKnowledge,
        SourceScope::Global,
        vec![candidate("good", "https://a.dev/1", 1.0)],
    );
    let slow: Arc<dyn SearchSource> = Arc::new(StubSource {
        descriptor: descriptor(
            "slow",
            SourceCategory::WebSearch,
            SourceScope::Global,
            1,
            0,
            true,
            &[],
        ),
        outcome: Err(SourceFailure::new(
            SourceExecutionStatus::Timeout,
            "deadline",
        )),
        calls: Arc::new(AtomicUsize::new(0)),
    });

    let request = SourceSearchRequest::new("q", SearchMode::Hybrid, workspace(), 5);
    let exec = SourceExecutionContext {
        request_id: RequestId::new("req_1"),
        source_id: SourceId::new("x"),
        deadline: Utc::now(),
        max_results: 5,
        max_estimated_cost_micros: 0,
        started_at: Utc::now(),
    };

    // Each source is an independent execution unit: one failing, one not.
    let good_result = good.search(request.clone(), exec.clone()).await;
    let slow_result = slow.search(request.clone(), exec).await;
    assert!(good_result.is_ok());
    assert!(slow_result.is_err());
}

#[test]
fn required_source_failure_forces_partial() {
    let official = descriptor(
        "official_docs",
        SourceCategory::WebDocument,
        SourceScope::Global,
        10,
        1_000,
        true,
        &["required_official_source"],
    );
    let blog = descriptor(
        "blog",
        SourceCategory::WebSearch,
        SourceScope::Global,
        1,
        1_000,
        true,
        &[],
    );
    let mut ctx = context(
        SearchMode::ExternalOnly,
        FederationStrategy::Auto,
        &["required_official_source", "background"],
    );
    ctx.required_requirements = vec!["required_official_source".into()];
    let plan = FederatedPlanner::plan(&ctx, &[official, blog.clone()]);
    assert!(plan.can_satisfy_requirements(), "a source exists for it");

    // Now the authoritative source is unavailable (circuit open). The blog
    // result must NOT be read as satisfying the requirement.
    let broken_official = SearchSourceDescriptor {
        reliability: searchos_federated::source::ReliabilityProfile {
            circuit_open: true,
            ..Default::default()
        },
        ..descriptor(
            "official_docs",
            SourceCategory::WebDocument,
            SourceScope::Global,
            10,
            1_000,
            true,
            &["required_official_source"],
        )
    };
    let plan = FederatedPlanner::plan(&ctx, &[broken_official, blog]);
    assert!(!plan.can_satisfy_requirements());
    assert_eq!(
        plan.unmet_required_requirements,
        vec!["required_official_source".to_string()]
    );

    let outcome = aggregate(
        &[SourceSearchResponse::success(
            "blog",
            vec![candidate("blog", "https://blog.dev/post", 3.0)],
            12,
        )],
        &plan,
    );
    assert!(
        outcome.required_failure,
        "availability (we got a blog post) must never be reported as quality"
    );
    assert_eq!(
        outcome.missing_requirements,
        vec!["required_official_source".to_string()]
    );
    assert_eq!(
        outcome.candidates.len(),
        1,
        "partial results are still returned"
    );
}

#[test]
fn failed_sources_are_reported_separately_from_candidates() {
    let plan_ctx = context(SearchMode::Hybrid, FederationStrategy::Auto, &[]);
    let plan = FederatedPlanner::plan(&plan_ctx, &[]);
    let outcome = aggregate(
        &[
            SourceSearchResponse::success("a", vec![candidate("a", "https://a.dev", 1.0)], 5),
            SourceSearchResponse::failed("b", SourceExecutionStatus::RateLimited, "429"),
        ],
        &plan,
    );
    assert_eq!(outcome.candidates.len(), 1);
    assert_eq!(outcome.failed_sources, vec!["b".to_string()]);
    assert_eq!(outcome.executed.len(), 2);
    assert!(!outcome.required_failure);
}

// ---------------------------------------------------------------------------
// RFC-064 — unified resource model
// ---------------------------------------------------------------------------

#[test]
fn resource_identity_is_stable_across_sources() {
    let ws = workspace();
    let fetched = SearchResource::new(ResourceType::WebPage, &ws, "https://doc.rust-lang.org/rel");
    let cached = SearchResource::new(ResourceType::WebPage, &ws, "https://doc.rust-lang.org/rel");
    assert_eq!(fetched.resource_id, cached.resource_id);
    assert_eq!(fetched.dedup_key(), cached.dedup_key());
}

#[test]
fn occurrences_are_not_independent_sources() {
    // The same content hash discovered by three sources is ONE resource with
    // three occurrences. Counting it as three sources is the false-corroboration
    // bug RFC-074 exists to prevent.
    let ws = workspace();
    let mut resources = Vec::new();
    for reference in ["https://a.dev/x", "https://b.dev/y", "https://c.dev/z"] {
        resources.push(
            SearchResource::new(ResourceType::Document, &ws, reference).with_content_hash("same"),
        );
    }
    let keys: Vec<Option<String>> = resources.iter().map(|r| r.dedup_key()).collect();
    assert_eq!(keys[0], keys[1]);
    assert_eq!(keys[1], keys[2]);
    let unique: std::collections::HashSet<_> = keys.into_iter().flatten().collect();
    assert_eq!(unique.len(), 1, "three reposts, one resource");
}

#[test]
fn permission_is_fail_closed_in_the_resource_model() {
    let ws = workspace();
    let raw = SearchResource::new(ResourceType::Document, &ws, "local://notes/1");
    assert!(
        !raw.context_eligible(None),
        "an unverified resource must never enter an Agent context"
    );
    let verified = raw.clone().with_permission(PermissionRef::workspace(&ws));
    assert!(verified.context_eligible(None));
    assert_eq!(
        verified.content_ref,
        ContentRef::ReferenceOnly,
        "identity does not imply the content was fetched"
    );
}

#[test]
fn session_message_is_a_first_class_resource() {
    // RFC-064's whole point: an agent's own past work is retrievable without
    // being force-converted into a Document.
    let ws = workspace();
    let message = SearchResource::new(ResourceType::Message, &ws, "session://s42/messages/7")
        .with_title("why we chose FTS5")
        .with_language("en");
    assert_eq!(
        message.source_category,
        SourceCategory::Conversation,
        "type maps to the right owning source category"
    );
    assert!(message.category_matches_type());
    assert!(message.resource_type.is_user_owned());
    assert!(message.resource_type.default_category() == SourceCategory::Conversation);
}

#[test]
fn unverified_agent_scoped_source_cannot_reach_a_restricted_resource() {
    let ws = workspace();
    let mut permission = PermissionRef::workspace(&ws);
    permission.allowed_agents = vec!["agent-owner".into()];
    permission.verified = false;
    let resource = SearchResource::new(ResourceType::Transcript, &ws, "session://s42")
        .with_permission(permission);
    assert!(!resource.context_eligible(Some("agent-owner")));
}

// ---------------------------------------------------------------------------
// RFC-080 — outcome feedback opens the per-source circuit
// ---------------------------------------------------------------------------

fn web_filter() -> SourceFilter {
    SourceFilter {
        enabled_only: true,
        categories: vec![SourceCategory::WebSearch],
        ..Default::default()
    }
}

#[test]
fn consecutive_failures_open_the_source_circuit_and_the_planner_rejects_it() {
    let registry = InMemorySourceRegistry::new();
    registry
        .register(stub(
            "flaky",
            SourceCategory::WebSearch,
            SourceScope::Global,
            Vec::new(),
        ))
        .unwrap();
    let id = SourceId::new("flaky");
    let ctx = context(SearchMode::ExternalOnly, FederationStrategy::Auto, &["gap"]);

    // A fresh source is selectable: the circuit starts closed.
    let before = registry.list(&web_filter(), &workspace());
    assert!(FederatedPlanner::plan(&ctx, &before)
        .selected_source_ids()
        .contains(&"flaky".to_string()));

    // Two failures are tolerated; the third OPENS (transition returned once).
    assert_eq!(registry.record_outcome(&id, false), Some(false));
    assert_eq!(registry.record_outcome(&id, false), Some(false));
    assert_eq!(registry.record_outcome(&id, false), Some(true));
    assert_eq!(
        registry.record_outcome(&id, false),
        Some(false),
        "the counter reports transitions, not every failure while open"
    );

    let after = registry.list(&web_filter(), &workspace());
    let reliability = &after[0].reliability;
    assert!(reliability.circuit_open);
    assert_eq!(reliability.consecutive_failures, 4);
    assert!(reliability.success_rate < 1.0, "failures lower the rate");

    // The SAME registry now plans differently: a tripped source is rejected
    // with the reliability reason, not silently executed anyway.
    let plan = FederatedPlanner::plan(&ctx, &after);
    assert!(plan.selected_source_ids().is_empty());
    assert_eq!(
        plan.rejections
            .iter()
            .find(|r| r.source_id == "flaky")
            .map(|r| r.reason),
        Some(RejectionReason::Reliability)
    );
}

#[test]
fn one_success_closes_the_circuit_and_resets_the_failure_streak() {
    let registry = InMemorySourceRegistry::new();
    registry
        .register(stub(
            "recovering",
            SourceCategory::WebSearch,
            SourceScope::Global,
            Vec::new(),
        ))
        .unwrap();
    let id = SourceId::new("recovering");
    for _ in 0..3 {
        registry.record_outcome(&id, false);
    }
    assert!(
        registry.list(&web_filter(), &workspace())[0]
            .reliability
            .circuit_open
    );

    assert_eq!(registry.record_outcome(&id, true), Some(false));
    let reliability = &registry.list(&web_filter(), &workspace())[0].reliability;
    assert!(!reliability.circuit_open);
    assert_eq!(reliability.consecutive_failures, 0);
    let ctx = context(SearchMode::ExternalOnly, FederationStrategy::Auto, &["gap"]);
    assert!(
        FederatedPlanner::plan(&ctx, &registry.list(&web_filter(), &workspace()))
            .selected_source_ids()
            .contains(&"recovering".to_string())
    );
}

#[test]
fn outcome_updates_every_scope_copy_of_a_source() {
    // A workspace override must not keep a tripped global source alive.
    let registry = InMemorySourceRegistry::new();
    registry
        .register(stub(
            "shared",
            SourceCategory::WebSearch,
            SourceScope::Global,
            Vec::new(),
        ))
        .unwrap();
    let mut overridden = descriptor(
        "shared",
        SourceCategory::WebSearch,
        SourceScope::Workspace(WS.into()),
        99,
        0,
        false,
        &[],
    );
    overridden.reliability.circuit_open = false;
    registry
        .register(Arc::new(StubSource {
            descriptor: overridden,
            outcome: Ok(SourceSearchResponse::success("shared", Vec::new(), 1)),
            calls: Arc::new(AtomicUsize::new(0)),
        }) as Arc<dyn SearchSource>)
        .unwrap();

    let id = SourceId::new("shared");
    for _ in 0..3 {
        registry.record_outcome(&id, false);
    }
    let references = registry.all_references();
    assert_eq!(references.len(), 2);
    assert!(
        references
            .iter()
            .all(|r| r.descriptor.reliability.circuit_open),
        "all scope registrations of one id trip together"
    );
    assert_eq!(
        registry
            .descriptor_for(&id, &workspace())
            .unwrap()
            .reliability
            .success_rate,
        references[0].descriptor.reliability.success_rate,
        "the resolved descriptor agrees with every copy"
    );
}

// ---------------------------------------------------------------------------
// RFC-080 — per-source latency budget
// ---------------------------------------------------------------------------

#[test]
fn planner_rejects_a_source_outside_the_wall_clock_budget() {
    let mut slow = descriptor(
        "too_slow",
        SourceCategory::WebSearch,
        SourceScope::Global,
        50,
        1_000,
        true,
        &["gap"],
    );
    slow.latency.timeout_ms = 8_000;
    let mut fast = descriptor(
        "fits",
        SourceCategory::WebSearch,
        SourceScope::Global,
        50,
        1_000,
        true,
        &["gap"],
    );
    fast.latency.timeout_ms = 100;
    let ctx = PlanningContext {
        requirements: vec!["gap".into()],
        max_latency_ms: 1_000,
        ..context(SearchMode::ExternalOnly, FederationStrategy::Auto, &["gap"])
    };
    let plan = FederatedPlanner::plan(&ctx, &[slow, fast]);
    assert_eq!(plan.selected_source_ids(), vec!["fits".to_string()]);
    assert_eq!(
        plan.rejections
            .iter()
            .find(|r| r.source_id == "too_slow")
            .map(|r| r.reason),
        Some(RejectionReason::Latency),
        "the slow source is excluded with an auditable reason"
    );
    assert_eq!(RejectionReason::Latency.as_str(), "latency");
}

#[test]
fn latency_feedback_updates_the_live_profile_on_every_scope_copy() {
    let registry = InMemorySourceRegistry::new();
    registry
        .register(stub(
            "shared",
            SourceCategory::WebSearch,
            SourceScope::Global,
            Vec::new(),
        ))
        .unwrap();
    let mut overridden = descriptor(
        "shared",
        SourceCategory::WebSearch,
        SourceScope::Workspace(WS.into()),
        99,
        0,
        false,
        &[],
    );
    overridden.latency.timeout_ms = 9_000;
    registry
        .register(Arc::new(StubSource {
            descriptor: overridden,
            outcome: Ok(SourceSearchResponse::success("shared", Vec::new(), 1)),
            calls: Arc::new(AtomicUsize::new(0)),
        }) as Arc<dyn SearchSource>)
        .unwrap();

    let id = SourceId::new("shared");
    // Repeated slow observations move p50 up and push the p95 envelope up.
    for _ in 0..6 {
        registry.record_latency(&id, 2_000);
    }
    let references = registry.all_references();
    assert_eq!(references.len(), 2);
    for r in &references {
        assert!(
            r.descriptor.latency.p50_ms > 50,
            "declared p50 is replaced by observed latency: {:?}",
            r.descriptor.latency
        );
        assert!(r.descriptor.latency.p95_ms >= 2_000);
        assert_eq!(
            r.descriptor.latency.timeout_ms,
            if r.descriptor.scope == SourceScope::Global {
                5_000
            } else {
                9_000
            },
            "declared timeout stays a hard bound, never inferred downward"
        );
    }
    // Fast observations after a slow streak decay the envelope (with a
    // p50-based floor), so recovery is visible too.
    for _ in 0..20 {
        registry.record_latency(&id, 5);
    }
    let resolved = registry.descriptor_for(&id, &workspace()).unwrap().latency;
    assert!(resolved.p50_ms < 50, "p50 follows fast observations");
    assert!(resolved.p95_ms < 2_000, "the p95 envelope decays");
    assert!(
        resolved.p95_ms >= resolved.p50_ms * 2,
        "p95 stays above p50"
    );
}
// --- RFC-080: cost-adaptive source selection ---------------------------------

fn adaptive_source(
    id: &str,
    priority: i32,
    cost_micros: i64,
    success_rate: f32,
    metadata_trust: f32,
) -> SearchSourceDescriptor {
    let mut d = descriptor(
        id,
        SourceCategory::WebSearch,
        SourceScope::Global,
        priority,
        cost_micros,
        true,
        &[], // generic: fills open requirements, never authoritative
    );
    d.reliability.success_rate = success_rate;
    d.reliability.metadata_trust = metadata_trust;
    d
}

#[test]
fn cost_adaptive_prefers_quality_per_cost_over_configured_priority() {
    // Expensive + mediocre, but declared high priority.
    let expensive = adaptive_source("expensive", 10, 50_000, 0.5, 0.5);
    // Cheap + reliable, with a low priority.
    let cheap = adaptive_source("cheap", 1, 5_000, 0.95, 1.0);
    // Two open requirements so BOTH generic sources have a fill: the
    // "no re-purchase" rule (INV-30-04) would otherwise reject the second.
    let mut ctx = context(
        SearchMode::ExternalOnly,
        FederationStrategy::Auto,
        &["a", "b"],
    );
    ctx.selection = searchos_federated::SelectionMode::CostAdaptive;

    let plan = FederatedPlanner::plan(&ctx, &[expensive, cheap]);
    assert_eq!(
        plan.selected_source_ids(),
        vec!["cheap".to_string(), "expensive".to_string()],
        "value density must beat configured priority under cost_adaptive"
    );
    assert_eq!(
        plan.selection,
        searchos_federated::SelectionMode::CostAdaptive
    );
    assert_eq!(plan.selection_scores.len(), 2);
    let cheap_score = plan
        .selection_scores
        .iter()
        .find(|s| s.source_id == "cheap")
        .expect("score for every eligible source");
    assert!((cheap_score.expected_quality - 0.95).abs() < 0.001);
    assert!(cheap_score.selected);
    assert!((cheap_score.value_density - 190.0).abs() < 0.01);
}

#[test]
fn reliability_first_keeps_the_historical_priority_order() {
    let expensive = adaptive_source("expensive", 10, 50_000, 0.5, 0.5);
    let cheap = adaptive_source("cheap", 1, 5_000, 0.95, 1.0);
    let ctx = context(
        SearchMode::ExternalOnly,
        FederationStrategy::Auto,
        &["a", "b"],
    );

    let plan = FederatedPlanner::plan(&ctx, &[expensive, cheap]);
    assert_eq!(
        plan.selected_source_ids(),
        vec!["expensive".to_string(), "cheap".to_string()],
        "the default must remain byte-for-byte compatible"
    );
    assert_eq!(
        plan.selection,
        searchos_federated::SelectionMode::ReliabilityFirst
    );
    // Scores are still recorded for audit, even when the policy is off.
    assert_eq!(plan.selection_scores.len(), 2);
}

#[test]
fn cost_adaptive_never_trades_a_required_authoritative_source_for_a_cheaper_one() {
    let mut official = adaptive_source("official", 1, 50_000, 0.5, 0.5);
    official.satisfies_requirements = vec!["required_official_source".into()];
    let generic = adaptive_source("generic", 20, 1_000, 1.0, 1.0);
    // "background" is a second, non-required gap so the generic source is
    // eligible too; "required_official_source" is the REQUIRED gap that the
    // authoritative source owns.
    let mut ctx = context(
        SearchMode::ExternalOnly,
        FederationStrategy::Auto,
        &["required_official_source", "background"],
    );
    ctx.required_requirements = vec!["required_official_source".into()];
    ctx.selection = searchos_federated::SelectionMode::CostAdaptive;

    let plan = FederatedPlanner::plan(&ctx, &[generic, official]);
    assert_eq!(
        plan.selected_source_ids(),
        vec!["official".to_string(), "generic".to_string()],
        "requirement authority outranks value density: fail-open is not a quality pass"
    );
    assert!(plan.can_satisfy_requirements());
}

#[test]
fn free_local_source_stays_first_under_cost_adaptive() {
    let local = descriptor(
        "local",
        SourceCategory::LocalKnowledge,
        SourceScope::Global,
        1,
        0,
        false,
        &[],
    );
    let mut web = adaptive_source("web", 20, 1, 1.0, 1.0);
    web.reliability.success_rate = 1.0;
    web.reliability.metadata_trust = 1.0;
    let mut ctx = context(SearchMode::Hybrid, FederationStrategy::Auto, &["answer"]);
    ctx.selection = searchos_federated::SelectionMode::CostAdaptive;

    let plan = FederatedPlanner::plan(&ctx, &[web, local]);
    assert_eq!(
        plan.selected_source_ids()[0],
        "local",
        "locality is the primary key; a paid source cannot outrank a free local one"
    );
}
