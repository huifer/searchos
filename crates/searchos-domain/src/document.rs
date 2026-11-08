//! Document domain model (RFC-006 6.3, Part 17 schema baseline).

use crate::ids::{ChunkId, DocumentId, DocumentVersionId, SourceId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Document lifecycle status (RFC-006 6.2, 6.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus {
    Discovered,
    Fetched,
    Normalized,
    Indexed,
    Deleted,
}

/// A logical document identified by canonical URL (RFC-006 6.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: DocumentId,
    pub source_id: SourceId,
    pub canonical_url: String,
    pub current_version_id: Option<DocumentVersionId>,
    pub content_type: String,
    pub language: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An immutable content version of a document (RFC-006 6.3/6.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentVersion {
    pub id: DocumentVersionId,
    pub document_id: DocumentId,
    /// SHA-256 hex of normalized content.
    pub content_hash: String,
    /// Object-store reference of raw content (`sha256:<hex>`).
    pub raw_content_ref: String,
    /// Object-store reference of extracted markdown, if produced.
    pub markdown_ref: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub extractor_version: String,
    pub crawler_version: Option<String>,
}

/// A chunk produced from a document version (RFC-006 6.2, Part 17 17.8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentChunk {
    pub id: ChunkId,
    pub version_id: DocumentVersionId,
    pub document_id: DocumentId,
    pub chunk_hash: String,
    /// Object-store reference of chunk content.
    pub content_ref: String,
    /// Chunk text (kept in domain for indexing; adapters may drop it).
    pub text: String,
    pub token_count: Option<u32>,
    pub chunk_index: u32,
    pub created_at: DateTime<Utc>,
}

/// Result of comparing a freshly fetched content hash against the current
/// version (RFC-006 6.4 Versioning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionDiff {
    /// Content unchanged: update fetch metadata only, no re-chunk/re-index.
    Unchanged,
    /// Content changed: create new version, new chunks, new index events.
    NewVersion,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&DocumentStatus::Normalized).unwrap(),
            "\"normalized\""
        );
    }
}
