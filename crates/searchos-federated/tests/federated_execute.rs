//! Federated execution tests (RFC-062 §8): deadline isolation, bounded
//! fan-out, cancellation, and honest reporting of unexecuted sources.

use async_trait::async_trait;
use chrono::Utc;
use searchos_domain::ids::{RequestId, SourceId, WorkspaceId};
use searchos_domain::SearchMode;
use searchos_federated::execute::{
    ExecutionControls, ExecutionScope, FederatedExecutor, SourceExecutionResult,
};
use searchos_federated::planner::{
    FederatedPlanner, FederationStrategy, PlanningContext, StageExecutionMode,
};
use searchos_federated::registry::InMemorySourceRegistry;
use searchos_federated::source::{
    ReliabilityProfile, SearchSource, SearchSourceDescriptor, SourceCandidate, SourceCapabilities,
    SourceCategory, SourceCostProfile, SourceExecutionContext, SourceExecutionStatus,
    SourceLatencyProfile, SourceScope, SourceSearchRequest, SourceSearchResponse,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WS: &str = "ws_exec";

fn workspace() -> WorkspaceId {
    WorkspaceId::new(WS)
}

fn descriptor(id: &str, category: SourceCategory, timeout_ms: u64) -> SearchSourceDescriptor {
    SearchSourceDescriptor {
        source_id: SourceId::new(id),
        name: id.to_string(),
        category,
        version: "1".into(),
        enabled: true,
        priority: 1,
        capabilities: SourceCapabilities {
            lexical_search: true,
            ..Default::default()
        },
        reliability: ReliabilityProfile::default(),
        cost: SourceCostProfile::default(),
        latency: SourceLatencyProfile {
            p50_ms: 1,
            p95_ms: timeout_ms,
            timeout_ms,
        },
        scope: SourceScope::Global,
        satisfies_requirements: Vec::new(),
    }
}

fn request() -> SourceSearchRequest {
    SourceSearchRequest::new("q", SearchMode::Hybrid, workspace(), 5)
}

fn scope(max_parallel: usize) -> ExecutionScope {
    ExecutionScope {
        request_id: RequestId::generate(),
        workspace_id: workspace(),
        max_parallel_sources: max_parallel,
    }
}

fn candidate(source: &str, reference: &str) -> SourceCandidate {
    SourceCandidate {
        source_id: SourceId::new(source),
        canonical_ref: reference.into(),
        title: None,
        snippet: "snippet".into(),
        raw_score: Some(1.0),
        observed_at: Utc::now(),
        published_at: None,
        satisfies_requirements: Vec::new(),
        permission_verified: true,
    }
}

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

struct Sleeper {
    descriptor: SearchSourceDescriptor,
    delay: Duration,
    started: Arc<AtomicUsize>,
    concurrent: Arc<AtomicUsize>,
    max_observed_concurrency: Arc<AtomicUsize>,
}

#[async_trait]
impl SearchSource for Sleeper {
    fn descriptor(&self) -> SearchSourceDescriptor {
        self.descriptor.clone()
    }

    async fn search(
        &self,
        _request: SourceSearchRequest,
        _context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, searchos_federated::source::SourceError> {
        let running = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
        self.started.fetch_add(1, Ordering::SeqCst);
        self.max_observed_concurrency
            .fetch_max(running, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.concurrent.fetch_sub(1, Ordering::SeqCst);
        Ok(SourceSearchResponse::success(
            self.descriptor.source_id.clone(),
            vec![candidate(
                self.descriptor.source_id.as_str(),
                "https://slow.dev/x",
            )],
            self.delay.as_millis() as u64,
        ))
    }

    async fn health_check(&self) -> searchos_federated::source::SourceHealth {
        searchos_federated::source::SourceHealth::Healthy
    }
}

struct FailFast {
    descriptor: SearchSourceDescriptor,
}

#[async_trait]
impl SearchSource for FailFast {
    fn descriptor(&self) -> SearchSourceDescriptor {
        self.descriptor.clone()
    }

    async fn search(
        &self,
        _request: SourceSearchRequest,
        _context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, searchos_federated::source::SourceError> {
        Err(searchos_federated::source::SourceError::Unavailable(
            "provider down".into(),
        ))
    }

    async fn health_check(&self) -> searchos_federated::source::SourceHealth {
        searchos_federated::source::SourceHealth::Down
    }
}

struct Recorder {
    descriptor: SearchSourceDescriptor,
    seen: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl SearchSource for Recorder {
    fn descriptor(&self) -> SearchSourceDescriptor {
        self.descriptor.clone()
    }

    async fn search(
        &self,
        request: SourceSearchRequest,
        _context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, searchos_federated::source::SourceError> {
        self.seen
            .lock()
            .unwrap()
            .push((self.descriptor.name.clone(), request.query.clone()));
        Ok(SourceSearchResponse::success(
            self.descriptor.source_id.clone(),
            vec![candidate(self.descriptor.name.as_str(), "https://ok.dev/x")],
            1,
        ))
    }

    async fn health_check(&self) -> searchos_federated::source::SourceHealth {
        searchos_federated::source::SourceHealth::Healthy
    }
}

fn sleeper(
    id: &str,
    category: SourceCategory,
    delay_ms: u64,
    timeout_ms: u64,
    state: &SharedState,
) -> Arc<dyn SearchSource> {
    Arc::new(Sleeper {
        descriptor: descriptor(id, category, timeout_ms),
        delay: Duration::from_millis(delay_ms),
        started: Arc::clone(&state.started),
        concurrent: Arc::clone(&state.concurrent),
        max_observed_concurrency: Arc::clone(&state.max_concurrent),
    })
}

#[derive(Clone, Default)]
struct SharedState {
    started: Arc<AtomicUsize>,
    concurrent: Arc<AtomicUsize>,
    max_concurrent: Arc<AtomicUsize>,
}

fn plan_for(
    ids: &[&str],
    strategy: FederationStrategy,
    stage_timeout_ms: u64,
) -> searchos_federated::planner::FederatedSearchPlan {
    let mut plan = FederatedPlanner::plan(
        &PlanningContext {
            query: "q".into(),
            mode: SearchMode::Hybrid,
            strategy,
            workspace_id: WS.into(),
            requirements: Vec::new(),
            covered_requirements: Vec::new(),
            required_requirements: Vec::new(),
            max_sources: ids.len(),
            max_parallel_sources: ids.len(),
            max_estimated_cost_micros: 1_000_000,
            stop_when_covered: false,
            generic_sources_can_answer: false,
        },
        &[],
    );
    // Build the stage tasks directly: these tests exercise the executor,
    // and selection is covered by planner tests.
    plan.stages = vec![searchos_federated::planner::SearchStage {
        stage_id: "stage1".into(),
        execution_mode: match strategy {
            FederationStrategy::Sequential => StageExecutionMode::Sequential,
            FederationStrategy::Selective => StageExecutionMode::Adaptive,
            _ => StageExecutionMode::Parallel,
        },
        tasks: ids
            .iter()
            .map(|id| searchos_federated::planner::SourceTask {
                source_id: id.to_string(),
                category: SourceCategory::LocalKnowledge,
                requirements: Vec::new(),
                billable: false,
                estimated_cost_micros: 0,
                filler: false,
            })
            .collect(),
        timeout_ms: stage_timeout_ms,
    }];
    plan
}

// ---------------------------------------------------------------------------
// Deadline isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn slow_source_is_cut_off_at_its_deadline_without_blocking_the_request() {
    let registry = InMemorySourceRegistry::new();
    let state = SharedState::default();
    registry
        .register(sleeper("slow", SourceCategory::WebSearch, 400, 40, &state))
        .unwrap();
    registry
        .register(sleeper(
            "fast",
            SourceCategory::LocalKnowledge,
            10,
            1_000,
            &state,
        ))
        .unwrap();

    let started = std::time::Instant::now();
    let executor = FederatedExecutor::new(&registry, scope(4), ExecutionControls::new());
    let report = executor
        .execute(
            &plan_for(&["slow", "fast"], FederationStrategy::Auto, 50),
            &request(),
        )
        .await;
    let elapsed = started.elapsed();

    assert_eq!(report.executed_count(), 2);
    let slow = report
        .results
        .iter()
        .find(|r| r.source_id == "slow")
        .expect("slow reported");
    let fast = report
        .results
        .iter()
        .find(|r| r.source_id == "fast")
        .expect("fast reported");

    assert_eq!(slow.status, SourceExecutionStatus::Timeout);
    assert!(slow.timed_out);
    assert!(slow.response.is_none());
    assert_eq!(fast.status, SourceExecutionStatus::Success);
    assert_eq!(
        fast.candidates().len(),
        1,
        "a slow source must not cost us the results of a fast one"
    );
    // The slow source's 400 ms sleep must NOT have been awaited.
    assert!(
        elapsed < Duration::from_millis(300),
        "request blocked on the slow source for {elapsed:?}"
    );
}

#[tokio::test]
async fn source_failure_is_isolated_and_status_is_recorded() {
    let registry = InMemorySourceRegistry::new();
    let state = SharedState::default();
    registry
        .register(sleeper(
            "ok",
            SourceCategory::LocalKnowledge,
            1,
            1_000,
            &state,
        ))
        .unwrap();
    registry
        .register(Arc::new(FailFast {
            descriptor: descriptor("broken", SourceCategory::WebSearch, 1_000),
        }))
        .unwrap();

    let report = FederatedExecutor::new(&registry, scope(2), ExecutionControls::new())
        .execute(
            &plan_for(&["ok", "broken"], FederationStrategy::Auto, 50),
            &request(),
        )
        .await;

    assert_eq!(report.executed_count(), 2);
    assert_eq!(report.failed_source_ids(), vec!["broken".to_string()]);
    let broken = report
        .results
        .iter()
        .find(|r| r.source_id == "broken")
        .unwrap();
    assert_eq!(broken.status, SourceExecutionStatus::Unavailable);
    assert_eq!(
        broken.failure.as_ref().unwrap().status,
        SourceExecutionStatus::Unavailable
    );
    // A failed source still leaves the other results usable.
    assert!(!report.responses().is_empty());
}

#[tokio::test]
async fn unregistered_source_reports_unavailable_not_a_request_error() {
    let registry = InMemorySourceRegistry::new();
    let report = FederatedExecutor::new(&registry, scope(1), ExecutionControls::new())
        .execute(
            &plan_for(&["ghost"], FederationStrategy::Auto, 10),
            &request(),
        )
        .await;
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].status, SourceExecutionStatus::Unavailable);
    assert!(report.results[0]
        .failure
        .as_ref()
        .unwrap()
        .message
        .contains("not registered"));
}

// ---------------------------------------------------------------------------
// Bounded fan-out
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fan_out_never_exceeds_the_per_request_limit() {
    let registry = InMemorySourceRegistry::new();
    let state = SharedState::default();
    let ids: Vec<String> = (0..6).map(|i| format!("s{i}")).collect();
    for id in &ids {
        registry
            .register(sleeper(id, SourceCategory::WebSearch, 40, 1_000, &state))
            .unwrap();
    }
    let refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
    let report = FederatedExecutor::new(&registry, scope(2), ExecutionControls::new())
        .execute(
            &plan_for(&refs, FederationStrategy::Parallel, 200),
            &request(),
        )
        .await;

    assert_eq!(report.executed_count(), 6, "all sources eventually run");
    assert_eq!(state.started.load(Ordering::SeqCst), 6);
    assert!(
        state.max_concurrent.load(Ordering::SeqCst) <= 2,
        "observed concurrency {} exceeded the limit 2",
        state.max_concurrent.load(Ordering::SeqCst)
    );
}

// ---------------------------------------------------------------------------
// Cancellation + deadline propagation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelled_execution_starts_no_further_sources() {
    let registry = InMemorySourceRegistry::new();
    let state = SharedState::default();
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    for i in 0..3 {
        registry
            .register(Arc::new(Recorder {
                descriptor: descriptor(&format!("rec{i}"), SourceCategory::LocalKnowledge, 1_000),
                seen: Arc::clone(&seen),
            }))
            .unwrap();
    }

    let controls = ExecutionControls::new();
    controls.cancel();
    let report = FederatedExecutor::new(&registry, scope(1), controls)
        .execute(
            &plan_for(
                &["rec0", "rec1", "rec2"],
                FederationStrategy::Sequential,
                100,
            ),
            &request(),
        )
        .await;

    assert!(report.cancelled);
    assert!(report.results.is_empty());
    assert_eq!(report.not_executed.len(), 3);
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(state.started.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn expired_deadline_stops_new_work_and_marks_it() {
    let registry = InMemorySourceRegistry::new();
    registry
        .register(Arc::new(Recorder {
            descriptor: descriptor("rec", SourceCategory::LocalKnowledge, 1_000),
            seen: Arc::new(Mutex::new(Vec::new())),
        }))
        .unwrap();
    let controls =
        ExecutionControls::new().with_deadline(Utc::now() - chrono::Duration::seconds(1));
    let report = FederatedExecutor::new(&registry, scope(1), controls)
        .execute(
            &plan_for(&["rec"], FederationStrategy::Sequential, 100),
            &request(),
        )
        .await;
    assert!(report.deadline_reached);
    assert_eq!(report.not_executed, vec!["rec".to_string()]);
}

// ---------------------------------------------------------------------------
// Strategy shapes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sequential_stages_run_in_order() {
    let registry = InMemorySourceRegistry::new();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    struct Ordered {
        descriptor: SearchSourceDescriptor,
        name: String,
        seen: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl SearchSource for Ordered {
        fn descriptor(&self) -> SearchSourceDescriptor {
            self.descriptor.clone()
        }
        async fn search(
            &self,
            _r: SourceSearchRequest,
            _c: SourceExecutionContext,
        ) -> Result<SourceSearchResponse, searchos_federated::source::SourceError> {
            self.seen.lock().unwrap().push(self.name.clone());
            Ok(SourceSearchResponse::success(
                self.descriptor.source_id.clone(),
                Vec::new(),
                0,
            ))
        }
    }
    for name in ["a", "b", "c"] {
        registry
            .register(Arc::new(Ordered {
                descriptor: descriptor(name, SourceCategory::LocalKnowledge, 1_000),
                name: name.into(),
                seen: Arc::clone(&seen),
            }))
            .unwrap();
    }
    let report = FederatedExecutor::new(&registry, scope(1), ExecutionControls::new())
        .execute(
            &plan_for(&["a", "b", "c"], FederationStrategy::Sequential, 100),
            &request(),
        )
        .await;
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
    // Report order mirrors stage order even though results are awaited.
    assert_eq!(
        report
            .results
            .iter()
            .map(|r| r.source_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
}

#[tokio::test]
async fn latency_is_measured_per_source() {
    let registry = InMemorySourceRegistry::new();
    let state = SharedState::default();
    registry
        .register(sleeper(
            "slowish",
            SourceCategory::WebSearch,
            30,
            1_000,
            &state,
        ))
        .unwrap();
    let report = FederatedExecutor::new(&registry, scope(1), ExecutionControls::new())
        .execute(
            &plan_for(&["slowish"], FederationStrategy::Auto, 500),
            &request(),
        )
        .await;
    let result: &SourceExecutionResult = &report.results[0];
    assert_eq!(result.status, SourceExecutionStatus::Success);
    assert!(result.response.as_ref().unwrap().latency_ms >= 20);
}

#[tokio::test]
async fn request_query_is_forwarded_to_every_selected_source() {
    let registry = InMemorySourceRegistry::new();
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    for name in ["x", "y"] {
        registry
            .register(Arc::new(Recorder {
                descriptor: descriptor(name, SourceCategory::LocalKnowledge, 1_000),
                seen: Arc::clone(&seen),
            }))
            .unwrap();
    }
    let mut req = request();
    req.query = "rust edition 2024".into();
    FederatedExecutor::new(&registry, scope(2), ExecutionControls::new())
        .execute(&plan_for(&["x", "y"], FederationStrategy::Auto, 500), &req)
        .await;
    let calls = seen.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|(_, query)| query == "rust edition 2024"));
}
