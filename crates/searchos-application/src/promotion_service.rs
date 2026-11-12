//! External evidence promotion (RFC-031).
//!
//! The "buy once, reuse forever" loop: record what an external search bought,
//! decide which items the quality gate accepted, capture the accepted pages
//! through the governed fetch path, persist them as first-class documents and
//! index them so the next equivalent query resolves locally.
//!
//! Two hard rules live here:
//!
//! 1. **Only gate-accepted items are ever indexed.** The caller assigns the
//!    state; this service persists it verbatim and enqueues capture only for
//!    [`PromotionState::Accepted`] items.
//! 2. **Capture never blocks a response.** `record_and_queue` writes rows and
//!    returns; `capture` runs on a dedicated outbox worker.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use searchos_domain::ids::{TaskId, TraceId, WorkspaceId};
use searchos_domain::promotion::{EvidenceItem, ExternalAcquisition, PromotionState, QueryOrigin};
use searchos_domain::retrieval::IndexedDocument;
use searchos_domain::{CostEvent, CostOperation, Money, SearchOsError};
use searchos_observability::MetricsRegistry;
use searchos_provider::embedding::EmbeddingProvider;
use searchos_retrieval::{SearchIndex, VectorIndex};

use crate::fetch_service::FetchService;
use crate::search_service::CostEventSink;

/// Persistence port for acquisitions, items, bindings and reuse events
/// (implemented by the storage adapter).
#[async_trait]
pub trait PromotionStore: Send + Sync {
    /// Persists one acquisition and its result items (states as given).
    async fn record_acquisition(
        &self,
        acquisition: &ExternalAcquisition,
        items: &[EvidenceItem],
    ) -> Result<(), SearchOsError>;

    /// Enqueues a capture outbox row for an accepted item. Returns `false`
    /// when an identical capture is already pending.
    async fn enqueue_capture(
        &self,
        item_id: &str,
        url: &str,
        workspace_id: &str,
        task_id: &str,
        acquisition_id: &str,
    ) -> Result<bool, SearchOsError>;

    /// Capture target for one item: canonical URL, owning workspace and
    /// current state.
    async fn capture_target(&self, item_id: &str) -> Result<Option<CaptureTarget>, SearchOsError>;

    /// Sets the promotion state of an item, with the reason when skipped.
    async fn update_item_state(
        &self,
        item_id: &str,
        state: PromotionState,
        document_id: Option<&str>,
        version_id: Option<&str>,
        chunk_count: Option<usize>,
        reason: Option<&str>,
    ) -> Result<(), SearchOsError>;

    /// Writes the workspace membership of a promoted document.
    async fn bind_document(
        &self,
        document_id: &str,
        workspace_id: &str,
        visibility: &str,
    ) -> Result<(), SearchOsError>;

    /// Records one reuse of external-sourced knowledge by a local resolution.
    async fn record_reuse(
        &self,
        task_id: &str,
        workspace_id: &str,
        document_id: &str,
    ) -> Result<(), SearchOsError>;

    /// Returns which of the given documents were promoted from external
    /// acquisitions.
    async fn promoted_document_ids(
        &self,
        document_ids: &[String],
    ) -> Result<HashSet<String>, SearchOsError>;
}

/// Target of a pending capture.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureTarget {
    pub url: String,
    pub workspace_id: String,
    pub task_id: String,
    pub trace_id: Option<String>,
    pub state: PromotionState,
}

/// Invalidates query/retrieval caches after a knowledge mutation (RFC-031 D4).
///
/// Implemented by the runtime; v1 invalidates the full cache family, matching
/// the document lifecycle path.
#[async_trait]
pub trait CacheInvalidator: Send + Sync {
    async fn invalidate_after_promotion(&self, workspace_id: &str, canonical_url: &str);
}

/// Promotion loop configuration (RFC-031 6.4 `knowledge.*`).
#[derive(Debug, Clone)]
pub struct PromotionConfig {
    pub enabled: bool,
    /// How many accepted external items may be captured per task.
    pub max_promotions_per_task: usize,
    /// Hard byte bound for captured bodies per task.
    pub max_promoted_bytes_per_task: u64,
    /// Capture the snippet when the body cannot be fetched. Default off: a
    /// snippet is not knowledge.
    pub promote_snippet_fallback: bool,
    /// Promote only items the final quality gate accepted.
    pub require_final_gate_pass: bool,
}

impl Default for PromotionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_promotions_per_task: 2,
            max_promoted_bytes_per_task: 10_485_760,
            promote_snippet_fallback: false,
            require_final_gate_pass: true,
        }
    }
}

/// External evidence promotion use case (RFC-031 6.1).
#[async_trait]
pub trait EvidencePromotionService: Send + Sync {
    /// Persists the acquisition and its items, then enqueues capture for the
    /// accepted ones. The item states are assigned by the caller from the
    /// final quality gate verdict and are persisted verbatim.
    async fn record_and_queue(
        &self,
        acquisition: ExternalAcquisition,
        items: Vec<EvidenceItem>,
    ) -> Result<(), SearchOsError>;

    /// Capture worker entry point: governed fetch → document/version/chunks →
    /// index → cache invalidation.
    async fn capture(&self, item_id: &str) -> Result<(), SearchOsError>;

    /// Marks a local resolution as reusing external-sourced knowledge.
    async fn record_reuse(
        &self,
        task_id: &TaskId,
        workspace_id: &WorkspaceId,
        document_id: &str,
    ) -> Result<(), SearchOsError>;

    /// Which of the given documents came from an external acquisition.
    async fn promoted_document_ids(
        &self,
        document_ids: &[String],
    ) -> Result<HashSet<String>, SearchOsError>;

    /// Per-task capture cap, used to size the budget reservation (RFC-031 G9).
    fn max_promotions_per_task(&self) -> usize {
        0
    }
}

/// Concrete promotion service over the store / fetch / index ports.
pub struct EvidencePromotionServiceImpl {
    store: Arc<dyn PromotionStore>,
    fetch: Arc<dyn FetchService>,
    index: Option<Arc<dyn SearchIndex>>,
    vector: Option<Arc<dyn VectorIndex>>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    invalidator: Option<Arc<dyn CacheInvalidator>>,
    cost_sink: Option<Arc<dyn CostEventSink>>,
    estimator: searchos_cost::CostEstimator,
    config: PromotionConfig,
}

impl EvidencePromotionServiceImpl {
    /// Builds the service over injected ports.
    pub fn new(
        store: Arc<dyn PromotionStore>,
        fetch: Arc<dyn FetchService>,
        index: Option<Arc<dyn SearchIndex>>,
        invalidator: Option<Arc<dyn CacheInvalidator>>,
        cost_sink: Option<Arc<dyn CostEventSink>>,
        cost_config: &searchos_cost::CostModelConfig,
        config: PromotionConfig,
    ) -> Self {
        Self {
            store,
            fetch,
            index,
            vector: None,
            embedder: None,
            invalidator,
            cost_sink,
            estimator: searchos_cost::CostEstimator::from_config(cost_config),
            config,
        }
    }

    /// Attaches the dense-vector port and embedder so captured evidence is
    /// vector-indexed (hybrid recall) as well as full-text indexed. The same
    /// [`EmbeddingProvider`] instance that produces query embeddings must be
    /// used here, otherwise stored vectors and query vectors come from
    /// different models and nearest-neighbor search degrades silently.
    pub fn with_vector_index(
        mut self,
        vector: Option<Arc<dyn VectorIndex>>,
        embedder: Option<Arc<dyn EmbeddingProvider>>,
    ) -> Self {
        self.vector = vector;
        self.embedder = embedder;
        self
    }

    /// Embeds one chunk payload with the runtime embedder.
    async fn embed_chunk(&self, payload: &IndexedDocument) -> Result<Vec<f32>, SearchOsError> {
        let embedder = self.embedder.as_ref().ok_or_else(|| {
            SearchOsError::Retrieval("vector index attached without an embedder".into())
        })?;
        let text = match &payload.title {
            Some(title) => format!("{title} {}", payload.body),
            None => payload.body.clone(),
        };
        let mut vectors = embedder
            .embed(std::slice::from_ref(&text))
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("embed chunk: {e}")))?;
        vectors
            .pop()
            .ok_or_else(|| SearchOsError::Retrieval("empty embedding".into()))
    }

    /// Maps a capture failure to a stable skip reason.
    fn skip_reason(err: &SearchOsError) -> String {
        match err {
            SearchOsError::Security(msg) => {
                if msg.contains("robots") {
                    "robots_disallowed".into()
                } else {
                    "blocked".into()
                }
            }
            SearchOsError::Retrieval(msg) => {
                if msg.contains("empty content") {
                    "empty_content".into()
                } else {
                    "capture_failed".into()
                }
            }
            SearchOsError::Provider(_) => "fetch_failed".into(),
            SearchOsError::Budget(_) => "budget_exhausted".into(),
            SearchOsError::Validation(_) => "invalid_url".into(),
            _ => "capture_failed".into(),
        }
    }
}

#[async_trait]
impl EvidencePromotionService for EvidencePromotionServiceImpl {
    async fn record_and_queue(
        &self,
        acquisition: ExternalAcquisition,
        items: Vec<EvidenceItem>,
    ) -> Result<(), SearchOsError> {
        let metrics = MetricsRegistry::global();
        if !self.config.enabled || items.is_empty() {
            return Ok(());
        }
        // Ensure the foreign key is present even when callers construct the
        // items directly (the search pipeline uses `build_acquisition`, but
        // the application port must be safe on its own).
        let items: Vec<EvidenceItem> = items
            .into_iter()
            .map(|mut item| {
                item.acquisition_id = acquisition.id.clone();
                item
            })
            .collect();
        // Persist acquisition + items verbatim; the caller assigned the
        // states from the final quality gate.
        self.store.record_acquisition(&acquisition, &items).await?;
        metrics.inc_external_acquisition();

        let mut queued = 0usize;
        for item in &items {
            match item.state {
                PromotionState::Accepted => {
                    if queued >= self.config.max_promotions_per_task {
                        // Over the per-task cap: the item stays accepted but
                        // is not captured this time.
                        continue;
                    }
                    let enqueued = self
                        .store
                        .enqueue_capture(
                            &item.id,
                            &item.canonical_url,
                            &acquisition.workspace_id,
                            &acquisition.task_id,
                            &acquisition.id,
                        )
                        .await?;
                    if enqueued {
                        queued += 1;
                        metrics.inc_external_promotion_fetch();
                    } else {
                        metrics.inc_refresh_coalesced();
                    }
                }
                PromotionState::Rejected => {
                    metrics.inc_external_promotion("rejected");
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn capture(&self, item_id: &str) -> Result<(), SearchOsError> {
        let metrics = MetricsRegistry::global();
        let Some(target) = self.store.capture_target(item_id).await? else {
            return Ok(());
        };
        // Only accepted items may be captured; a re-run after a processed
        // row is a no-op (idempotency through the state machine).
        if target.state != PromotionState::Accepted {
            return Ok(());
        }
        let workspace = WorkspaceId::new(&target.workspace_id);
        let task = TaskId::new(target.task_id.clone());
        let trace = target.trace_id.as_ref().map(|t| TraceId::new(t.clone()));
        let started = std::time::Instant::now();
        match self.fetch.fetch_and_persist(&target.url, &workspace).await {
            Ok(doc) => {
                record_capture_cost(
                    self.cost_sink.as_ref(),
                    &task,
                    trace.as_ref(),
                    &workspace,
                    self.estimator.estimate_request("web_fetch").amount_micros,
                    "USD",
                    true,
                    None,
                    started.elapsed().as_millis() as u64,
                )
                .await;
                // Index the current version directly (mirrors the refresh
                // path); the save_versioned outbox rows stay as the durable
                // rebuild source (ADR-022).
                if let Some(index) = &self.index {
                    let payloads: Vec<IndexedDocument> = self
                        .fetch
                        .reindex_current_version(&doc.document_id, &workspace)
                        .await
                        .unwrap_or_default();
                    // Vector upsert is idempotent (delete-by-chunk + add), so
                    // it runs before the full-text add: a retry after a
                    // partial failure re-upserts vectors without duplicating
                    // them, mirroring the outbox ordering.
                    for payload in &payloads {
                        if let (Some(vector), Some(embedder)) = (&self.vector, &self.embedder) {
                            match self.embed_chunk(payload).await {
                                Ok(v) => {
                                    if let Err(e) = vector
                                        .upsert(
                                            payload.chunk_id.as_str(),
                                            payload.document_id.as_str(),
                                            v,
                                            embedder.model_id(),
                                        )
                                        .await
                                    {
                                        tracing::warn!(
                                            code = %e.class().code,
                                            chunk = %payload.chunk_id.as_str(),
                                            "capture vector upsert failed"
                                        );
                                    }
                                }
                                Err(e) => tracing::warn!(
                                    code = %e.class().code,
                                    "capture embed failed"
                                ),
                            }
                        }
                        let _ = index.index(payload.clone()).await;
                    }
                    let _ = index.commit().await;
                }
                self.store
                    .bind_document(doc.document_id.as_str(), &target.workspace_id, "workspace")
                    .await?;
                self.store
                    .update_item_state(
                        item_id,
                        PromotionState::Promoted,
                        Some(doc.document_id.as_str()),
                        Some(doc.version_id.as_str()),
                        Some(doc.chunk_count),
                        None,
                    )
                    .await?;
                metrics.inc_external_promotion("promoted");
                if let Some(invalidator) = &self.invalidator {
                    invalidator
                        .invalidate_after_promotion(&target.workspace_id, &target.url)
                        .await;
                }
                Ok(())
            }
            Err(err) => {
                let reason = Self::skip_reason(&err);
                metrics.inc_external_promotion("skipped");
                record_capture_cost(
                    self.cost_sink.as_ref(),
                    &task,
                    trace.as_ref(),
                    &workspace,
                    0,
                    "USD",
                    false,
                    Some(&reason),
                    started.elapsed().as_millis() as u64,
                )
                .await;
                self.store
                    .update_item_state(
                        item_id,
                        PromotionState::Skipped,
                        None,
                        None,
                        None,
                        Some(&reason),
                    )
                    .await?;
                Ok(())
            }
        }
    }

    async fn record_reuse(
        &self,
        task_id: &TaskId,
        workspace_id: &WorkspaceId,
        document_id: &str,
    ) -> Result<(), SearchOsError> {
        self.store
            .record_reuse(task_id.as_str(), workspace_id.as_str(), document_id)
            .await?;
        MetricsRegistry::global().inc_external_evidence_reuse();
        Ok(())
    }

    async fn promoted_document_ids(
        &self,
        document_ids: &[String],
    ) -> Result<HashSet<String>, SearchOsError> {
        self.store.promoted_document_ids(document_ids).await
    }

    fn max_promotions_per_task(&self) -> usize {
        self.config.max_promotions_per_task
    }
}

/// Builds one acquisition + its items from an external execution outcome
/// (helper shared by the search pipeline).
#[allow(clippy::too_many_arguments)]
pub fn build_acquisition(
    task_id: &TaskId,
    request_id: &str,
    workspace_id: &WorkspaceId,
    provider_id: &str,
    query: &str,
    query_origin: QueryOrigin,
    requirement_id: Option<String>,
    strategy_id: Option<String>,
    created_at: chrono::DateTime<Utc>,
    items: Vec<EvidenceItem>,
) -> (ExternalAcquisition, Vec<EvidenceItem>) {
    let acquisition = ExternalAcquisition {
        id: searchos_domain::new_id("acq"),
        task_id: task_id.to_string(),
        request_id: request_id.to_string(),
        workspace_id: workspace_id.to_string(),
        provider_id: provider_id.to_string(),
        query: query.to_string(),
        query_origin,
        requirement_id,
        strategy: strategy_id
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        created_at,
    };
    let items: Vec<EvidenceItem> = items
        .into_iter()
        .map(|mut item| {
            item.acquisition_id = acquisition.id.clone();
            item.created_at = created_at;
            item
        })
        .collect();
    (acquisition, items)
}

/// Charges one capture fetch to the owning task (post-settlement, RFC-031 9.7).
#[allow(clippy::too_many_arguments)]
pub async fn record_capture_cost(
    cost_sink: Option<&Arc<dyn CostEventSink>>,
    task_id: &TaskId,
    trace_id: Option<&TraceId>,
    workspace_id: &WorkspaceId,
    estimated_micros: i64,
    currency: &str,
    succeeded: bool,
    error_code: Option<&str>,
    duration_ms: u64,
) {
    let Some(sink) = cost_sink else { return };
    let ev = CostEvent {
        id: searchos_domain::ids::CostEventId::generate(),
        task_id: task_id.clone(),
        workspace_id: workspace_id.clone(),
        agent_id: None,
        operation: CostOperation::WebFetch,
        provider_id: None,
        input_units: if succeeded { 1 } else { 0 },
        output_units: 0,
        estimated_cost: Money {
            amount_micros: estimated_micros,
            currency: currency.to_string(),
        },
        actual_cost: None,
        duration_ms,
        occurred_at: Utc::now(),
        trace_id: trace_id.cloned(),
        shared: false,
        pricing_model_version: "1".into(),
        status: if succeeded {
            searchos_domain::CostAttemptStatus::Succeeded
        } else {
            searchos_domain::CostAttemptStatus::Failed
        },
        error_code: error_code.map(|s| s.to_string()),
    };
    let _ = sink.append(ev).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_reasons_are_stable() {
        assert_eq!(
            EvidencePromotionServiceImpl::skip_reason(&SearchOsError::Security(
                "robots disallowed".into()
            )),
            "robots_disallowed"
        );
        assert_eq!(
            EvidencePromotionServiceImpl::skip_reason(&SearchOsError::Security(
                "blocked: private ip".into()
            )),
            "blocked"
        );
        assert_eq!(
            EvidencePromotionServiceImpl::skip_reason(&SearchOsError::Retrieval(
                "empty content not indexed".into()
            )),
            "empty_content"
        );
        assert_eq!(
            EvidencePromotionServiceImpl::skip_reason(&SearchOsError::Provider("http 500".into())),
            "fetch_failed"
        );
        assert_eq!(
            EvidencePromotionServiceImpl::skip_reason(&SearchOsError::Budget("over".into())),
            "budget_exhausted"
        );
        assert_eq!(
            EvidencePromotionServiceImpl::skip_reason(&SearchOsError::Internal("x".into())),
            "capture_failed"
        );
    }

    #[test]
    fn config_defaults_match_rfc() {
        let c = PromotionConfig::default();
        assert!(c.enabled);
        assert_eq!(c.max_promotions_per_task, 2);
        assert!(!c.promote_snippet_fallback);
        assert!(c.require_final_gate_pass);
    }
}
