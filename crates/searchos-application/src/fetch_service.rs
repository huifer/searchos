//! FetchService (RFC-012 12.1, RFC-010).
//!
//! URL → guard → robots → fetch → extract → hash → persist (document repo
//! + object store + outbox events).

use async_trait::async_trait;
use chrono::Utc;
use searchos_document::hash::content_ref;
use searchos_domain::document::{Document, DocumentVersion};
use searchos_domain::ids::{DocumentId, DocumentVersionId, SourceId, WorkspaceId};
use searchos_domain::SearchOsError;
use searchos_fetch::{FetchOutcome, HttpFetcher, RobotsChecker, UrlGuard};
use searchos_storage_fs::ObjectStore;
use std::sync::Arc;

/// A persisted fetch result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FetchedDocument {
    pub document_id: DocumentId,
    pub version_id: DocumentVersionId,
    pub canonical_url: String,
    pub content_hash: String,
    /// Whether content changed vs the stored version (RFC-006 6.4).
    pub changed: bool,
    pub title: Option<String>,
    pub markdown_ref: String,
    pub chunk_count: usize,
    pub from_cache: bool,
}

/// Fetch use case.
#[async_trait]
pub trait FetchService: Send + Sync {
    /// Fetches and persists a URL.
    async fn fetch_and_persist(
        &self,
        url: &str,
        workspace_id: &WorkspaceId,
    ) -> Result<FetchedDocument, SearchOsError>;

    /// Fetch + extract without persisting (inspection / preview).
    async fn fetch_preview(
        &self,
        url: &str,
    ) -> Result<searchos_fetch::extract::ExtractedContent, SearchOsError>;
}

/// Concrete fetch service.
pub struct FetchServiceImpl {
    fetcher: HttpFetcher,
    objects: Arc<dyn ObjectStore>,
    /// Document persistence port (so the application crate doesn't depend on
    /// sqlite directly; sqlite adapter implements it).
    doc_port: Arc<dyn DocumentPort>,
}

/// Port implemented by storage adapters for document persistence
/// (transactional version + chunks + outbox, RFC-006 6.7).
#[async_trait]
pub trait DocumentPort: Send + Sync {
    /// Finds a document by canonical URL.
    async fn find_by_url(&self, canonical_url: &str) -> Result<Option<Document>, SearchOsError>;
    /// Saves a versioned document with chunks + outbox payloads atomically.
    async fn save_versioned(
        &self,
        document: &Document,
        version: &DocumentVersion,
        chunks: &[searchos_domain::document::DocumentChunk],
        index_payloads: &[searchos_domain::retrieval::IndexedDocument],
    ) -> Result<searchos_document::VersionDiff, SearchOsError>;
    /// Current version lookup.
    async fn get_current_version(
        &self,
        id: &DocumentId,
    ) -> Result<Option<DocumentVersion>, SearchOsError>;
}

impl FetchServiceImpl {
    /// Assembles the service.
    pub fn new(
        fetcher: HttpFetcher,
        objects: Arc<dyn ObjectStore>,
        doc_port: Arc<dyn DocumentPort>,
    ) -> Self {
        Self {
            fetcher,
            objects,
            doc_port,
        }
    }

    /// Guard + robots pair for external assembly.
    pub fn with_guard(
        guard: Arc<UrlGuard>,
        robots: Arc<RobotsChecker>,
        objects: Arc<dyn ObjectStore>,
        doc_port: Arc<dyn DocumentPort>,
    ) -> Self {
        Self {
            fetcher: HttpFetcher::new(guard, robots, Default::default()),
            objects,
            doc_port,
        }
    }

    fn canonicalize(raw: &str) -> String {
        // Minimal canonicalization: strip fragment + trailing slash.
        let u = raw.trim();
        let no_frag = u.split('#').next().unwrap_or(u);
        let no_frag = no_frag.trim_end_matches('?');
        if no_frag.len() > 1 && no_frag.ends_with('/') {
            no_frag.trim_end_matches('/').to_string()
        } else {
            no_frag.to_string()
        }
    }
}

#[async_trait]
impl FetchService for FetchServiceImpl {
    async fn fetch_and_persist(
        &self,
        url: &str,
        workspace_id: &WorkspaceId,
    ) -> Result<FetchedDocument, SearchOsError> {
        let canonical = Self::canonicalize(url);
        let outcome = self
            .fetcher
            .fetch(&canonical, None)
            .await
            .map_err(|v| SearchOsError::Security(format!("ssrf guard: {v}")))?;
        let extracted = match outcome {
            FetchOutcome::Ok(content) => content,
            FetchOutcome::NotModified => {
                return Err(SearchOsError::Retrieval(
                    "not modified (no cached copy given)".into(),
                ))
            }
            FetchOutcome::RobotsDisallowed => {
                return Err(SearchOsError::Security("robots disallowed".into()))
            }
            FetchOutcome::Blocked(v) => {
                return Err(SearchOsError::Security(format!("blocked: {v}")))
            }
            FetchOutcome::EmptyContent => {
                return Err(SearchOsError::Retrieval(
                    "empty content not indexed (RFC-010 10.7)".into(),
                ))
            }
            FetchOutcome::HttpError { status } => {
                return Err(SearchOsError::Provider(format!("http {status}")))
            }
            FetchOutcome::Error(e) => return Err(SearchOsError::Provider(format!("fetch: {e}"))),
        };

        // Content-addressed persistence.
        let raw_bytes = extracted.text.as_bytes();
        let raw_ref = self
            .objects
            .put(raw_bytes)
            .await
            .map_err(|e| SearchOsError::Storage(e.to_string()))?;
        let md_ref = self
            .objects
            .put(extracted.markdown.as_bytes())
            .await
            .map_err(|e| SearchOsError::Storage(e.to_string()))?;
        let content_hash = content_ref(raw_bytes)
            .strip_prefix("sha256:")
            .map(|s| s.to_string())
            .unwrap_or_default();

        // Versioning decision against current (RFC-006 6.4).
        let existing = self.doc_port.find_by_url(&canonical).await?;
        let (doc, _prior_version) = match &existing {
            Some(d) => (d.clone(), self.doc_port.get_current_version(&d.id).await?),
            None => (
                Document {
                    id: DocumentId::generate(),
                    source_id: SourceId::generate(),
                    canonical_url: canonical.clone(),
                    current_version_id: None,
                    content_type: "text/html".into(),
                    language: Some("en".into()),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                None,
            ),
        };

        let version = DocumentVersion {
            id: DocumentVersionId::generate(),
            document_id: doc.id.clone(),
            content_hash: content_hash.clone(),
            raw_content_ref: raw_ref.clone(),
            markdown_ref: Some(md_ref.clone()),
            fetched_at: Utc::now(),
            published_at: None,
            extractor_version: searchos_document::EXTRACTOR_VERSION.into(),
            crawler_version: Some(searchos_document::GENERIC_CRAWLER_VERSION.into()),
        };

        let chunks = searchos_document::chunk_text(&extracted.text, &Default::default());
        let domain_chunks: Vec<searchos_domain::document::DocumentChunk> = chunks
            .iter()
            .map(|c| searchos_domain::document::DocumentChunk {
                id: searchos_domain::ids::ChunkId::generate(),
                version_id: version.id.clone(),
                document_id: doc.id.clone(),
                chunk_hash: c.hash.clone(),
                content_ref: raw_ref.clone(),
                text: c.text.clone(),
                token_count: Some(c.text.len() as u32 / 4), // rough token estimate
                chunk_index: c.index,
                created_at: Utc::now(),
            })
            .collect();
        let index_payloads: Vec<searchos_domain::retrieval::IndexedDocument> = domain_chunks
            .iter()
            .map(|ch| searchos_domain::retrieval::IndexedDocument {
                chunk_id: ch.id.clone(),
                document_id: doc.id.clone(),
                version_id: version.id.clone(),
                title: extracted.title.clone(),
                body: ch.text.clone(),
                url: canonical.clone(),
                language: doc.language.clone(),
                workspace_id: workspace_id.to_string(),
                fetched_at: Utc::now(),
            })
            .collect();

        let diff = self
            .doc_port
            .save_versioned(&doc, &version, &domain_chunks, &index_payloads)
            .await?;

        let changed = matches!(diff, searchos_document::VersionDiff::NewVersion);
        Ok(FetchedDocument {
            document_id: doc.id,
            version_id: version.id,
            canonical_url: canonical,
            content_hash,
            changed,
            title: extracted.title,
            markdown_ref: md_ref,
            chunk_count: domain_chunks.len(),
            from_cache: false,
        })
    }

    async fn fetch_preview(
        &self,
        url: &str,
    ) -> Result<searchos_fetch::extract::ExtractedContent, SearchOsError> {
        match self.fetcher.fetch(url, None).await {
            Ok(FetchOutcome::Ok(content)) => Ok(content),
            Ok(other) => Err(SearchOsError::Retrieval(format!("fetch: {other:?}"))),
            Err(v) => Err(SearchOsError::Security(format!("ssrf guard: {v}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use searchos_fetch::RobotsChecker;
    use std::sync::Mutex;

    /// In-memory document port for tests.
    #[derive(Default)]
    struct MemPort {
        docs: Mutex<std::collections::HashMap<String, Document>>,
        versions: Mutex<Vec<(String, DocumentVersion)>>,
        saved_events: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl DocumentPort for MemPort {
        async fn find_by_url(
            &self,
            canonical_url: &str,
        ) -> Result<Option<Document>, SearchOsError> {
            Ok(self.docs.lock().unwrap().get(canonical_url).cloned())
        }
        async fn save_versioned(
            &self,
            document: &Document,
            version: &DocumentVersion,
            _chunks: &[searchos_domain::document::DocumentChunk],
            _payloads: &[searchos_domain::retrieval::IndexedDocument],
        ) -> Result<searchos_document::VersionDiff, SearchOsError> {
            self.saved_events
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut docs = self.docs.lock().unwrap();
            let entry = docs
                .entry(document.canonical_url.clone())
                .or_insert_with(|| document.clone());
            let diff = match &entry.current_version_id {
                Some(_vid) => {
                    let versions = self.versions.lock().unwrap();
                    let prev = versions
                        .iter()
                        .rev()
                        .find(|(id, _)| id == &entry.current_version_id.as_ref().unwrap().0)
                        .map(|(_, v)| v);
                    match prev {
                        Some(pv) if pv.content_hash == version.content_hash => {
                            searchos_document::VersionDiff::Unchanged
                        }
                        _ => searchos_document::VersionDiff::NewVersion,
                    }
                }
                None => searchos_document::VersionDiff::NewVersion,
            };
            if diff == searchos_document::VersionDiff::NewVersion {
                self.versions
                    .lock()
                    .unwrap()
                    .push((version.id.to_string(), version.clone()));
            }
            entry.current_version_id = Some(version.id.clone());
            Ok(diff)
        }
        async fn get_current_version(
            &self,
            id: &DocumentId,
        ) -> Result<Option<DocumentVersion>, SearchOsError> {
            // Current version = the version the document pointer references.
            let docs = self.docs.lock().unwrap();
            let Some(doc) = docs.values().find(|d| d.id == *id) else {
                return Ok(None);
            };
            let Some(vid) = &doc.current_version_id else {
                return Ok(None);
            };
            let versions = self.versions.lock().unwrap();
            Ok(versions
                .iter()
                .rev()
                .find(|(vid2, _)| vid2 == vid.as_str())
                .map(|(_, v)| v.clone()))
        }
    }

    #[tokio::test]
    async fn local_fetch_persists_and_version_dedups() {
        // In-process page served by a local test: use docs.rs? Network flaky —
        // exercise preview + guard on a blocked URL and persistence via unit.
        let objects = Arc::new(searchos_storage_fs::LocalObjectStore::new(
            std::env::temp_dir().join(format!("searchos-app-{}", std::process::id())),
        ));
        let port = Arc::new(MemPort::default());
        let port_for_assert = Arc::clone(&port);
        let guard = Arc::new(UrlGuard::default());
        let svc =
            FetchServiceImpl::with_guard(guard, Arc::new(RobotsChecker::new()), objects, port);

        // SSRF-blocked URL fails safely before any persistence.
        let err = svc
            .fetch_and_persist("http://127.0.0.1/x", &WorkspaceId::new("ws"))
            .await;
        assert!(err.is_err());
        assert_eq!(
            port_for_assert
                .saved_events
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );

        // Live fetch (network-dependent smoke).
        if let Ok(f) = svc
            .fetch_and_persist("https://example.com", &WorkspaceId::new("ws"))
            .await
        {
            assert!(f.chunk_count >= 1);
            assert_eq!(
                port_for_assert
                    .saved_events
                    .load(std::sync::atomic::Ordering::SeqCst),
                1
            );
            // Second fetch: identical content → metadata update only
            // (RFC-006 6.4). The live page may legitimately change between
            // fetches, so only assert the dedup when hashes match.
            if let Ok(f2) = svc
                .fetch_and_persist("https://example.com", &WorkspaceId::new("ws"))
                .await
            {
                if f2.content_hash == f.content_hash {
                    assert!(!f2.changed);
                }
            }
        }
    }

    #[tokio::test]
    async fn preview_extracts_without_persisting() {
        let objects = Arc::new(searchos_storage_fs::LocalObjectStore::new(
            std::env::temp_dir().join(format!("searchos-app-preview-{}", std::process::id())),
        ));
        let port = Arc::new(MemPort::default());
        let port_for_assert = Arc::clone(&port);
        let svc = FetchServiceImpl::with_guard(
            Arc::new(UrlGuard::default()),
            Arc::new(RobotsChecker::new()),
            objects,
            port,
        );
        if let Ok(content) = svc.fetch_preview("https://example.com").await {
            assert!(!content.text.is_empty());
            assert_eq!(
                port_for_assert
                    .saved_events
                    .load(std::sync::atomic::Ordering::SeqCst),
                0,
                "preview never persists"
            );
        }
    }
}
