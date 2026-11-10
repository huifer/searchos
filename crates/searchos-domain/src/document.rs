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

/// Default [`Document::source_type`] for content fetched from a URL.
fn default_source_type() -> String {
    "web".to_string()
}

fn default_workspace_id() -> String {
    "default".to_string()
}

/// A logical document identified by canonical URL (RFC-006 6.3).
///
/// `source_type` distinguishes URL-fetched content (`"web"`) from locally
/// ingested content (`"local_text"` / `"local_file"` / `local_directory"`,
/// RFC-029). Local documents use the canonical URI scheme
/// `searchos-local://<workspace>/<source_key>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: DocumentId,
    pub source_id: SourceId,
    pub canonical_url: String,
    pub current_version_id: Option<DocumentVersionId>,
    pub content_type: String,
    pub language: Option<String>,
    #[serde(default = "default_source_type")]
    pub source_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Owning workspace row (RFC-017/RFC-029). Persisted on
    /// `documents.workspace_id` and used by refresh/rebuild paths so they
    /// never move a document into another workspace.
    #[serde(default = "default_workspace_id")]
    pub workspace_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Document {
    /// Canonical URI for a locally ingested source (RFC-029 5.1).
    pub fn local_canonical_uri(workspace_id: &str, source_key: &str) -> String {
        format!(
            "searchos-local://{workspace_id}/{}",
            normalize_source_key(source_key)
        )
    }

    /// Whether this document came from local ingestion.
    pub fn is_local(&self) -> bool {
        self.source_type.starts_with("local_")
            || self.canonical_url.starts_with("searchos-local://")
    }
}

/// Normalizes a caller-supplied `source_key` (RFC-029 5.1): trimmed,
/// lower-cased, path separators collapsed to `/`, leading `/` removed.
pub fn normalize_source_key(key: &str) -> String {
    let trimmed = key.trim();
    let lowered = trimmed.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_slash = false;
    for ch in lowered.chars() {
        if ch == '\\' || ch == '/' {
            if !last_slash {
                out.push('/');
            }
            last_slash = true;
        } else {
            out.push(ch);
            last_slash = false;
        }
    }
    out.trim_matches('/').to_string()
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

/// Console-facing document summary (RFC-029 document lifecycle).
///
/// The [`Document`] model intentionally omits the lifecycle `status` column:
/// the domain only cares about identity and provenance. An operator console
/// does care, so the storage layer projects the row plus its version and
/// chunk counts into this summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub id: DocumentId,
    pub source_id: SourceId,
    pub canonical_url: String,
    pub current_version_id: Option<DocumentVersionId>,
    pub title: Option<String>,
    pub content_type: String,
    pub language: Option<String>,
    /// `web` for fetched URLs, `local_*` for ingested content.
    pub source_type: String,
    /// Lifecycle status column: `discovered` / `fetched` / `normalized` /
    /// `indexed` / `deleted`.
    pub status: String,
    pub workspace_id: String,
    pub version_count: i64,
    pub chunk_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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

    #[test]
    fn local_source_key_is_normalized() {
        assert_eq!(
            normalize_source_key("  Notes/Alpha\\Beta  "),
            "notes/alpha/beta"
        );
        assert_eq!(normalize_source_key("/lead//slash/"), "lead/slash");
    }

    #[test]
    fn local_canonical_uri_is_stable() {
        let a = Document::local_canonical_uri("ws", "Notes/Alpha");
        let b = Document::local_canonical_uri("ws", "notes/alpha");
        assert_eq!(a, b);
        assert!(a.starts_with("searchos-local://ws/"));
    }

    #[test]
    fn document_defaults_to_web_source_type() {
        // Backward compatible: JSON without source_type deserializes as web.
        let json = r#"{
            "id": "doc_1", "source_id": "src_1",
            "canonical_url": "https://example.com",
            "current_version_id": null, "content_type": "text/html",
            "language": null, "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }"#;
        let doc: Document = serde_json::from_str(json).unwrap();
        assert_eq!(doc.source_type, "web");
        assert!(doc.title.is_none());
        assert!(!doc.is_local());
    }
}
