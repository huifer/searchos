//! Federated execution (RFC-062 §8).
//!
//! Runs a [`FederatedSearchPlan`] against a registry: per-source deadline,
//! isolated failure, bounded fan-out, deterministic aggregation.
//!
//! The two properties this module exists to guarantee:
//!
//! * **Isolation.** Every source runs as its own task under its own deadline.
//!   A timeout, a panic-free error, or a slow source cannot fail the request
//!   and cannot block the others until the global deadline (RFC-062 §11).
//! * **Honest partiality.** Executed-but-failed sources are reported, and the
//!   aggregation boundary keeps the fail-open invariant: results are returned,
//!   but `required_failure` is set whenever a required requirement has no
//!   answer (ADR-062-004 / INV-FED-01).
//!
//! Cancellation: [`ExecutionControls`] carries a shared flag that the
//! executor checks before launching each stage and that a caller can flip to
//! stop the remaining stages. It is a plain atomic rather than a channel: v1
//! handles requests inline, so "do not start the next thing" is the whole
//! requirement, and the flag is observable from every transport without a
//! runtime dependency.

use crate::planner::{FederatedSearchPlan, SearchStage, StageExecutionMode};
use crate::registry::InMemorySourceRegistry;
use crate::source::{
    SearchSource, SourceError, SourceExecutionContext, SourceExecutionStatus, SourceFailure,
    SourceSearchRequest, SourceSearchResponse,
};
use chrono::{DateTime, Utc};
use searchos_domain::ids::{RequestId, SourceId, WorkspaceId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Cooperative cancellation for one federated execution.
#[derive(Debug, Default, Clone)]
pub struct ExecutionControls {
    cancelled: Arc<AtomicBool>,
    deadline: Option<DateTime<Utc>>,
}

impl ExecutionControls {
    pub fn new() -> Self {
        Self::default()
    }

    /// Absolute request deadline; execution stops starting new work past it.
    pub fn with_deadline(mut self, deadline: DateTime<Utc>) -> Self {
        self.deadline = Some(deadline);
        self
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Whether no further work may start.
    pub fn should_stop(&self) -> bool {
        if self.is_cancelled() {
            return true;
        }
        match self.deadline {
            Some(deadline) => Utc::now() >= deadline,
            None => false,
        }
    }

    /// Milliseconds left before the request deadline, if one was declared.
    /// `Some(0)` means already expired; `None` means unbounded.
    pub fn remaining_ms(&self) -> Option<u64> {
        self.deadline
            .map(|deadline| (deadline - Utc::now()).num_milliseconds().max(0) as u64)
    }
}

/// Per-execution identity, independent of any single source.
#[derive(Debug, Clone)]
pub struct ExecutionScope {
    pub request_id: RequestId,
    pub workspace_id: WorkspaceId,
    /// Total fan-out limit across the whole plan (per-request, RFC-062 8.2).
    pub max_parallel_sources: usize,
}

/// One executed source task with its outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceExecutionResult {
    pub source_id: String,
    pub stage_id: String,
    pub status: SourceExecutionStatus,
    pub response: Option<SourceSearchResponse>,
    pub failure: Option<SourceFailure>,
    pub timed_out: bool,
}

impl SourceExecutionResult {
    /// Candidates this execution contributed (empty when it failed).
    pub fn candidates(&self) -> Vec<crate::source::SourceCandidate> {
        self.response
            .as_ref()
            .map(|r| r.candidates.clone())
            .unwrap_or_default()
    }
}

/// The outcome of a whole plan.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionReport {
    pub results: Vec<SourceExecutionResult>,
    /// Sources that were never started (cancellation / deadline / fan-out).
    pub not_executed: Vec<String>,
    pub cancelled: bool,
    pub deadline_reached: bool,
}

impl ExecutionReport {
    pub fn responses(&self) -> Vec<SourceSearchResponse> {
        self.results
            .iter()
            .filter_map(|r| r.response.clone())
            .collect()
    }

    pub fn executed_count(&self) -> usize {
        self.results.len()
    }

    pub fn failed_source_ids(&self) -> Vec<String> {
        self.results
            .iter()
            .filter(|r| r.status.is_failure())
            .map(|r| r.source_id.clone())
            .collect()
    }
}

/// Registry-backed executor for one plan.
pub struct FederatedExecutor<'a> {
    registry: &'a InMemorySourceRegistry,
    scope: ExecutionScope,
    controls: ExecutionControls,
}

impl<'a> FederatedExecutor<'a> {
    pub fn new(
        registry: &'a InMemorySourceRegistry,
        scope: ExecutionScope,
        controls: ExecutionControls,
    ) -> Self {
        Self {
            registry,
            scope,
            controls,
        }
    }

    /// Executes the plan. Never returns an error for a source failure: an
    /// unreachable source is a *result*, not a request failure.
    pub async fn execute(
        &self,
        plan: &FederatedSearchPlan,
        request: &SourceSearchRequest,
    ) -> ExecutionReport {
        let mut results: Vec<SourceExecutionResult> = Vec::new();
        let mut not_executed: Vec<String> = Vec::new();
        let mut cancelled = false;
        let mut deadline_reached = false;

        for stage in &plan.stages {
            if self.controls.should_stop() {
                cancelled = self.controls.is_cancelled();
                deadline_reached = !cancelled;
                not_executed.extend(stage_task_ids(stage));
                continue;
            }
            match stage.execution_mode {
                StageExecutionMode::Sequential => {
                    for task in &stage.tasks {
                        if self.controls.should_stop() {
                            cancelled = self.controls.is_cancelled();
                            deadline_reached = !cancelled;
                            not_executed.push(task.source_id.clone());
                            continue;
                        }
                        results.push(self.run_one(stage, task.source_id.clone(), request).await);
                    }
                }
                StageExecutionMode::Parallel | StageExecutionMode::Adaptive => {
                    let mut set = tokio::task::JoinSet::new();
                    let mut queue: std::collections::VecDeque<String> =
                        stage_task_ids(stage).into_iter().collect();
                    let total = queue.len();
                    let limit = self.scope.max_parallel_sources.max(1).min(total.max(1));
                    for _ in 0..limit.min(queue.len()) {
                        if let Some(id) = queue.pop_front() {
                            self.spawn_into(&mut set, &stage.stage_id, id, request);
                        }
                    }
                    let mut finished: Vec<SourceExecutionResult> = Vec::new();
                    while let Some(joined) = set.join_next().await {
                        match joined {
                            Ok(result) => finished.push(result),
                            Err(error) => {
                                // A panicked adapter task is isolated: it
                                // becomes a failed source, never a failed
                                // request (ADR-062-007). The id is unknown
                                // here because the task died before naming
                                // itself, so the stage order pass below
                                // back-fills it as Unavailable.
                                tracing::warn!(%error, stage = %stage.stage_id, "federated source task failed");
                            }
                        }
                        if let Some(id) = queue.pop_front() {
                            self.spawn_into(&mut set, &stage.stage_id, id, request);
                        }
                    }
                    // Preserve stage order for the audit trail, and account
                    // for every task the stage declared.
                    for id in stage_task_ids(stage) {
                        match finished.iter().find(|r| r.source_id == id) {
                            Some(result) => results.push(result.clone()),
                            None => results.push(SourceExecutionResult {
                                source_id: id,
                                stage_id: stage.stage_id.clone(),
                                status: SourceExecutionStatus::Failed,
                                response: None,
                                failure: Some(SourceFailure::new(
                                    SourceExecutionStatus::Failed,
                                    "source task did not complete (panicked or was cancelled)",
                                )),
                                timed_out: false,
                            }),
                        }
                    }
                }
            }
        }

        ExecutionReport {
            results,
            not_executed,
            cancelled,
            deadline_reached,
        }
    }

    /// Effective per-source deadline: the source's declared timeout capped by
    /// the REMAINING request budget. A slow source can never overrun the
    /// wall-clock budget the caller paid for (RFC-080 latency budget); the
    /// declared timeout stays an upper bound when no deadline is set.
    fn bounded_timeout_ms(&self, declared_ms: u64) -> u64 {
        match self.controls.remaining_ms() {
            Some(remaining) => declared_ms.min(remaining),
            None => declared_ms,
        }
    }

    /// Spawns one source execution into a bounded JoinSet.
    fn spawn_into(
        &self,
        set: &mut tokio::task::JoinSet<SourceExecutionResult>,
        stage_id: &str,
        source_id: String,
        request: &SourceSearchRequest,
    ) {
        let descriptor = self
            .registry
            .descriptor_for(&SourceId::new(&source_id), &self.scope.workspace_id);
        let timeout_ms = self.bounded_timeout_ms(
            descriptor
                .as_ref()
                .map(|d| d.latency.timeout_ms)
                .unwrap_or(5_000),
        );
        let prepared = self.prepare(source_id.clone(), stage_id.to_string(), request, timeout_ms);
        set.spawn(async move { prepared.run().await });
    }

    fn prepare(
        &self,
        source_id: String,
        stage_id: String,
        request: &SourceSearchRequest,
        timeout_ms: u64,
    ) -> PreparedExecution {
        let source = self.resolve(&source_id);
        let context = SourceExecutionContext {
            request_id: self.scope.request_id.clone(),
            source_id: SourceId::new(source_id.clone()),
            deadline: Utc::now() + chrono::Duration::milliseconds(timeout_ms as i64),
            max_results: request.max_results,
            max_estimated_cost_micros: i64::MAX,
            started_at: Utc::now(),
        };
        PreparedExecution {
            source,
            source_id,
            stage_id,
            request: request.clone(),
            context,
            timeout_ms,
        }
    }

    async fn run_one(
        &self,
        stage: &SearchStage,
        source_id: String,
        request: &SourceSearchRequest,
    ) -> SourceExecutionResult {
        let timeout_ms = self.bounded_timeout_ms(
            self.registry
                .descriptor_for(&SourceId::new(&source_id), &self.scope.workspace_id)
                .map(|d| d.latency.timeout_ms)
                .unwrap_or_else(|| stage.timeout_ms.max(1)),
        );
        self.prepare(source_id, stage.stage_id.clone(), request, timeout_ms)
            .run()
            .await
    }

    fn resolve(&self, source_id: &str) -> Option<Arc<dyn SearchSource>> {
        self.registry
            .get(&SourceId::new(source_id), &self.scope.workspace_id)
    }
}

/// A source execution that has been resolved and bounded, ready to run.
struct PreparedExecution {
    source: Option<Arc<dyn SearchSource>>,
    source_id: String,
    stage_id: String,
    request: SourceSearchRequest,
    context: SourceExecutionContext,
    timeout_ms: u64,
}

impl PreparedExecution {
    async fn run(self) -> SourceExecutionResult {
        let Some(source) = self.source else {
            return SourceExecutionResult {
                source_id: self.source_id,
                stage_id: self.stage_id,
                status: SourceExecutionStatus::Unavailable,
                response: None,
                failure: Some(SourceFailure::new(
                    SourceExecutionStatus::Unavailable,
                    "source is not registered for this workspace",
                )),
                timed_out: false,
            };
        };
        run_with_deadline(
            source,
            self.source_id,
            self.stage_id,
            self.request,
            self.context,
            self.timeout_ms,
        )
        .await
    }
}

async fn run_with_deadline(
    source: Arc<dyn SearchSource>,
    source_id: String,
    stage_id: String,
    request: SourceSearchRequest,
    context: SourceExecutionContext,
    timeout_ms: u64,
) -> SourceExecutionResult {
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_millis(timeout_ms.max(1)),
        source.search(request, context),
    )
    .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let (status, response, failure, timed_out) = match outcome {
        Err(_elapsed) => (
            SourceExecutionStatus::Timeout,
            None,
            Some(SourceFailure::new(
                SourceExecutionStatus::Timeout,
                format!("source exceeded its {timeout_ms} ms deadline"),
            )),
            true,
        ),
        Ok(Ok(mut response)) => {
            response.latency_ms = latency_ms;
            let status = response.status;
            let failure = response.failure.clone();
            (status, Some(response), failure, false)
        }
        Ok(Err(error)) => {
            let status = error.status();
            (
                status,
                None,
                Some(SourceFailure::new(status, error.message())),
                false,
            )
        }
    };
    SourceExecutionResult {
        source_id,
        stage_id,
        status,
        response,
        failure,
        timed_out,
    }
}

fn stage_task_ids(stage: &SearchStage) -> Vec<String> {
    stage.tasks.iter().map(|t| t.source_id.clone()).collect()
}

/// Error text for a [`SourceError`] without leaking provider internals.
pub trait SourceErrorText {
    fn message(&self) -> String;
}

impl SourceErrorText for SourceError {
    fn message(&self) -> String {
        self.to_string()
    }
}
