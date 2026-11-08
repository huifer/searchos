//! Index traits (RFC-002 2.4, RFC-007 7.5).

use async_trait::async_trait;
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, RetrievalQuery};
use searchos_domain::{DocumentId, SearchOsError};

/// Full-text search index (rebuildable, RFC-002 2.4 SearchIndex).
#[async_trait]
pub trait SearchIndex: Send + Sync {
    /// Indexes one chunk-level document.
    async fn index(&self, document: IndexedDocument) -> Result<(), SearchOsError>;

    /// BM25 search.
    async fn search(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError>;

    /// Deletes all entries of a document.
    async fn delete(&self, document_id: &DocumentId) -> Result<(), SearchOsError>;

    /// Number of indexed documents (chunks), for status/diagnostics.
    async fn count(&self) -> Result<u64, SearchOsError>;
}

/// Dense vector index (ADR-009; rebuildable).
#[async_trait]
pub trait VectorIndex: Send + Sync {
    /// Stores one chunk embedding.
    async fn upsert(
        &self,
        chunk_id: &str,
        document_id: &str,
        vector: Vec<f32>,
        model_id: &str,
    ) -> Result<(), SearchOsError>;

    /// Nearest-neighbour search.
    async fn search(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError>;

    /// Deletes all vectors of a document.
    async fn delete_document(&self, document_id: &str) -> Result<(), SearchOsError>;

    /// Vector count for diagnostics.
    async fn count(&self) -> Result<u64, SearchOsError>;
}
