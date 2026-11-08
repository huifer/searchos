//! MemoryService trait (RFC-012 12.1, RFC-013 13.3).
//!
//! Memory search pipeline: Metadata Filter → BM25 + Vector → RRF →
//! Optional Reranker → Freshness Check → Evidence Return.
//! Storage implementations live in `searchos-storage-sqlite`.

use async_trait::async_trait;
use searchos_domain::memory::{MemoryEntry, MemoryNamespace, SharingPolicy};
use searchos_domain::SearchOsError;

/// One memory search hit with its relevance score.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemorySearchHit {
    pub entry: MemoryEntry,
    pub score: f32,
}

/// Memory persistence and search interface.
#[async_trait]
pub trait MemoryService: Send + Sync {
    /// Persists a memory entry (distilled evidence / task knowledge).
    async fn remember(
        &self,
        namespace: MemoryNamespace,
        sharing: SharingPolicy,
        title: &str,
        content: &str,
        url: Option<&str>,
        tags: &[String],
    ) -> Result<MemoryEntry, SearchOsError>;

    /// Memory search honoring namespace isolation (RFC-013 13.3).
    async fn search(
        &self,
        query: &str,
        reader: &MemoryNamespace,
        top_k: usize,
    ) -> Result<Vec<MemorySearchHit>, SearchOsError>;

    /// Deletes an entry (RFC-013 13.5 governance: Delete).
    async fn delete(&self, entry_id: &str) -> Result<(), SearchOsError>;

    /// Exports all entries visible to a reader (Export).
    async fn export(&self, reader: &MemoryNamespace) -> Result<Vec<MemoryEntry>, SearchOsError>;

    /// Aggregate stats (entry count per namespace) for `memory stats`.
    async fn stats(&self) -> Result<MemoryStats, SearchOsError>;
}

/// Memory statistics.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MemoryStats {
    pub total_entries: u64,
    pub by_workspace: Vec<(String, u64)>,
    pub with_embedding: u64,
}
