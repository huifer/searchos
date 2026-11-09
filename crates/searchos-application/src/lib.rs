//! Application layer (RFC-012 12.1).
//!
//! `SearchOsApplication` bundles the four use-case services:
//! `search_service`, `fetch_service`, `memory_service`, `cost_service`.
//! Both Local Mode and Gateway Mode call these — handlers never implement
//! business logic (RFC-012 12.3).
//!
//! Search pipeline: normalize → fingerprint → exact cache → singleflight →
//! decision → plan → execute (local / external) → persist → final quality
//! gate → response with cost + trace.

pub mod analytics;
pub mod fetch_service;
pub mod ingest_service;
pub mod merge;
pub mod search_service;
pub mod singleflight;

pub use analytics::{AnalyticsQuery, AnalyticsQueryService};
pub use fetch_service::{FetchService, FetchServiceImpl};
pub use ingest_service::{
    IngestBatchItem, IngestBatchResult, IngestFailure, IngestJobPort, IngestPayload, IngestRequest,
    IngestService, IngestServiceImpl, IngestedDocument, LOCAL_SOURCE_TYPE_DIRECTORY,
    LOCAL_SOURCE_TYPE_FILE, LOCAL_SOURCE_TYPE_MIXED, LOCAL_SOURCE_TYPE_TEXT,
};
pub use search_service::{SearchService, SearchServiceImpl};
pub use singleflight::SingleflightCoordinator;

use async_trait::async_trait;
use searchos_domain::memory::MemoryEntry;
use searchos_domain::SearchOsError;

use std::sync::Arc;

/// The application facade (RFC-012 12.1).
pub struct SearchOsApplication {
    pub search_service: Arc<dyn SearchService>,
    pub fetch_service: Arc<dyn FetchService>,
    /// RFC-029: local data import (text / file / directory).
    pub ingest_service: Arc<dyn IngestService>,
    pub memory_service: Arc<dyn MemoryUseCase>,
    pub cost_service: Arc<dyn CostUseCase>,
    /// RFC-026: single Analytics Query Service behind the Dashboard API.
    /// The frontend never reads operational tables directly.
    pub analytics_service: Arc<dyn analytics::AnalyticsQueryService>,
}

/// Memory use case (thin facade over MemoryService with namespace guard).
#[async_trait]
pub trait MemoryUseCase: Send + Sync {
    async fn remember(
        &self,
        namespace: searchos_domain::memory::MemoryNamespace,
        sharing: searchos_domain::memory::SharingPolicy,
        title: &str,
        content: &str,
        url: Option<&str>,
        tags: &[String],
    ) -> Result<MemoryEntry, SearchOsError>;
    async fn search(
        &self,
        query: &str,
        reader: &searchos_domain::memory::MemoryNamespace,
        top_k: usize,
    ) -> Result<Vec<searchos_memory::MemorySearchHit>, SearchOsError>;
    async fn stats(&self) -> Result<searchos_memory::MemoryStats, SearchOsError>;
    async fn delete(&self, entry_id: &str) -> Result<(), SearchOsError>;
    async fn export(
        &self,
        reader: &searchos_domain::memory::MemoryNamespace,
    ) -> Result<Vec<MemoryEntry>, SearchOsError>;
}

/// Cost use case (ledger append + summaries).
#[async_trait]
pub trait CostUseCase: Send + Sync {
    async fn append_event(&self, ev: searchos_domain::cost::CostEvent)
        -> Result<(), SearchOsError>;
    async fn summary(
        &self,
        q: &searchos_domain::cost::CostQuery,
    ) -> Result<searchos_domain::cost::CostSummary, SearchOsError>;
    async fn singleflight_saved_micros(&self) -> Result<i64, SearchOsError>;

    /// Savings / CPQPA report against an equivalent-quality external-only
    /// baseline (RFC-006 6.10, Appendix C).
    async fn savings(
        &self,
        _baseline_per_query_micros: i64,
    ) -> Result<searchos_domain::cost::SavingsReport, SearchOsError> {
        Ok(searchos_domain::cost::SavingsReport {
            currency: "USD".into(),
            is_estimate: true,
            ..Default::default()
        })
    }
    /// Budget reservation ledger view (RFC-004 v1.1 15).
    async fn reservations(&self, _limit: u32) -> Result<serde_json::Value, SearchOsError> {
        Ok(serde_json::json!({ "reservations": [] }))
    }

    /// Reservations scoped at the SQL level to one workspace (INV-30-10).
    async fn reservations_scoped(
        &self,
        limit: u32,
        _workspace: Option<&str>,
    ) -> Result<serde_json::Value, SearchOsError> {
        self.reservations(limit).await
    }
    /// Provider-bill reconciliation: estimated cost is never rewritten
    /// (RFC-006 6.7).
    async fn reconcile(
        &self,
        _cost_event_id: &str,
        _actual_cost_micros: i64,
        _source: &str,
        _currency: &str,
    ) -> Result<(), SearchOsError> {
        Ok(())
    }
}

/// Memory facade delegating to a [`searchos_memory::MemoryService`].
pub struct MemoryFacade {
    inner: Arc<dyn searchos_memory::MemoryService>,
}

impl MemoryFacade {
    /// Wraps a memory service.
    pub fn new(inner: Arc<dyn searchos_memory::MemoryService>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl MemoryUseCase for MemoryFacade {
    async fn remember(
        &self,
        namespace: searchos_domain::memory::MemoryNamespace,
        sharing: searchos_domain::memory::SharingPolicy,
        title: &str,
        content: &str,
        url: Option<&str>,
        tags: &[String],
    ) -> Result<MemoryEntry, SearchOsError> {
        self.inner
            .remember(namespace, sharing, title, content, url, tags)
            .await
    }

    async fn search(
        &self,
        query: &str,
        reader: &searchos_domain::memory::MemoryNamespace,
        top_k: usize,
    ) -> Result<Vec<searchos_memory::MemorySearchHit>, SearchOsError> {
        self.inner.search(query, reader, top_k).await
    }

    async fn stats(&self) -> Result<searchos_memory::MemoryStats, SearchOsError> {
        self.inner.stats().await
    }

    async fn delete(&self, entry_id: &str) -> Result<(), SearchOsError> {
        self.inner.delete(entry_id).await
    }

    async fn export(
        &self,
        reader: &searchos_domain::memory::MemoryNamespace,
    ) -> Result<Vec<MemoryEntry>, SearchOsError> {
        let entries = self.inner.export(reader).await?;
        // Namespace isolation applies on export too (RFC-013 13.5).
        Ok(searchos_memory::NamespaceGuard::visible_entries(
            &entries, reader,
        ))
    }
}
