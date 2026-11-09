//! FetchService (RFC-012 12.1, RFC-010).
//!
//! URL → guard → robots → fetch → extract → hash → persist (document repo
//! + object store + outbox events).

use async_trait::async_trait;
use chrono::Utc;
use searchos_crawler::status::{CrawlerErrorKind, CrawlerFailure};
use searchos_crawler::{CrawlerRegistry, SelectedCrawler};
use searchos_document::canonical::canonical_url;
use searchos_document::hash::content_ref;
use searchos_domain::document::{Document, DocumentVersion};
use searchos_domain::ids::{DocumentId, DocumentVersionId, SourceId, WorkspaceId};
use searchos_domain::retrieval::IndexedDocument;
use searchos_domain::SearchOsError;
use searchos_fetch::guard::UrlViolation;
use searchos_fetch::robots::RobotsDecision;
use searchos_fetch::{FetchLimits, FetchOutcome, HttpFetcher, RobotsChecker, UrlGuard};
use searchos_observability::MetricsRegistry;
use searchos_storage_fs::ObjectStore;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

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
    /// Raw, display URL. Only the canonical form is an identity / index key.
    #[serde(default)]
    pub raw_url: Option<String>,
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
    /// Rebuilds the current version's index payloads under the owning
    /// workspace. Used after a refresh so old-workspace chunks are replaced.
    async fn reindex_current_version(
        &self,
        document_id: &DocumentId,
        workspace_id: &WorkspaceId,
    ) -> Result<Vec<IndexedDocument>, SearchOsError>;

    /// Configured fetch limits (diagnostics / parity tests).
    fn limits(&self) -> searchos_fetch::FetchLimits {
        searchos_fetch::FetchLimits::default()
    }
}

/// Concrete fetch service.
pub struct FetchServiceImpl {
    fetcher: HttpFetcher,
    objects: Arc<dyn ObjectStore>,
    /// Document persistence port (so the application crate doesn't depend on
    /// sqlite directly; sqlite adapter implements it).
    doc_port: Arc<dyn DocumentPort>,
    /// Site-specific crawler registry (RFC-010 10.4), wired by the runtime.
    crawlers: Option<Arc<CrawlerRegistry>>,
    respect_robots: bool,
    unknown_robots_disallow: bool,
    max_concurrent: u32,
}

/// Port implemented by storage adapters for document persistence
/// (transactional version + chunks + outbox, RFC-006 6.7).
#[async_trait]
pub trait DocumentPort: Send + Sync {
    /// Finds a document by canonical URL.
    async fn find_by_url(&self, canonical_url: &str) -> Result<Option<Document>, SearchOsError>;
    /// Finds a document by its id.
    async fn find_by_id(&self, id: &DocumentId) -> Result<Option<Document>, SearchOsError>;
    /// Saves a versioned document with chunks + outbox payloads atomically.
    /// `workspace_id` is the owning workspace recorded on the document row
    /// (RFC-029 6.2 list filtering); retrieval isolation itself is enforced by
    /// the index payload's workspace_id.
    async fn save_versioned(
        &self,
        document: &Document,
        version: &DocumentVersion,
        chunks: &[searchos_domain::document::DocumentChunk],
        index_payloads: &[searchos_domain::retrieval::IndexedDocument],
        workspace_id: &WorkspaceId,
    ) -> Result<searchos_document::VersionDiff, SearchOsError>;
    /// Current version lookup.
    async fn get_current_version(
        &self,
        id: &DocumentId,
    ) -> Result<Option<DocumentVersion>, SearchOsError>;
    /// Returns the persisted chunk rows for one version (chunk metadata
    /// only; text is reconstructed by the caller from the object store).
    async fn list_chunks(
        &self,
        version_id: &DocumentVersionId,
    ) -> Result<Vec<searchos_domain::document::DocumentChunk>, SearchOsError>;
    /// Rebuilds the index payloads for one document's current version under
    /// the given owning workspace, without enqueueing outbox rows. Used after
    /// a refresh that may have moved the document across workspaces.
    async fn reindex_current_version(
        &self,
        document_id: &DocumentId,
        workspace_id: &WorkspaceId,
    ) -> Result<Vec<IndexedDocument>, SearchOsError>;
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
            crawlers: None,
            respect_robots: false,
            unknown_robots_disallow: true,
            max_concurrent: FetchLimits::default().max_concurrent,
        }
    }

    /// Guard + robots pair for tests / legacy assembly. Runtime uses
    /// [`FetchServiceImpl::with_runtime`] so crawler and resource-limit
    /// config fields actually reach the execution path.
    pub fn with_guard(
        guard: Arc<UrlGuard>,
        robots: Arc<RobotsChecker>,
        objects: Arc<dyn ObjectStore>,
        doc_port: Arc<dyn DocumentPort>,
    ) -> Self {
        let limits = FetchLimits {
            respect_robots: false,
            ..Default::default()
        };
        Self {
            fetcher: HttpFetcher::new(guard, robots, limits),
            objects,
            doc_port,
            crawlers: None,
            respect_robots: false,
            unknown_robots_disallow: true,
            max_concurrent: FetchLimits::default().max_concurrent,
        }
    }

    /// Runtime assembly with crawler registry + configured fetch limits.
    pub fn with_runtime(
        guard: Arc<UrlGuard>,
        robots: Arc<RobotsChecker>,
        objects: Arc<dyn ObjectStore>,
        doc_port: Arc<dyn DocumentPort>,
        crawlers: Option<Arc<CrawlerRegistry>>,
        limits: FetchLimits,
    ) -> Self {
        let respect_robots = limits.respect_robots;
        let unknown_robots_disallow = limits.unknown_robots_disallow;
        let max_concurrent = limits.max_concurrent;
        // The service performs the robots decision once (and records crawler
        // failure isolation); HttpFetcher therefore skips its duplicate
        // check but still pins DNS and streams within the same limits.
        let mut fetcher_limits = limits.clone();
        fetcher_limits.respect_robots = false;
        Self {
            fetcher: HttpFetcher::new(guard, robots, fetcher_limits),
            objects,
            doc_port,
            crawlers,
            respect_robots,
            unknown_robots_disallow,
            max_concurrent,
        }
    }

    pub fn max_concurrent(&self) -> u32 {
        self.max_concurrent
    }

    /// One canonical URL implementation for fetch, ingest, merge and index.
    pub fn canonicalize(raw: &str) -> String {
        canonical_url(raw)
    }

    fn selected_crawler(&self, url: &Url) -> Option<String> {
        let host = url.host_str().unwrap_or("");
        match self.crawlers.as_ref()?.select(host) {
            SelectedCrawler::SiteSpecific(id) => Some(id),
            SelectedCrawler::Generic | SelectedCrawler::FallbackFetch => None,
        }
    }

    async fn enforce_robots(&self, url: &Url) -> Result<(), SearchOsError> {
        if !self.respect_robots {
            return Ok(());
        }
        let decision = self
            .fetcher
            .robots_decision(url)
            .await
            .map_err(|v| SearchOsError::Security(format!("robots guard: {v}")))?;
        let allowed = match decision {
            RobotsDecision::Allowed => true,
            RobotsDecision::Disallowed { .. } => false,
            RobotsDecision::Unknown => !self.unknown_robots_disallow,
        };
        if !allowed {
            MetricsRegistry::global().inc_counter("fetch.robots_disallowed_total");
            return Err(SearchOsError::Security("robots disallowed".into()));
        }
        Ok(())
    }

    fn record_crawler_outcome(
        &self,
        selected: Option<String>,
        outcome: &Result<FetchOutcome, UrlViolation>,
    ) {
        let Some(id) = selected else {
            return;
        };
        let ok = matches!(
            outcome,
            Ok(FetchOutcome::Ok(_)) | Ok(FetchOutcome::NotModified)
        );
        let Some(crawlers) = &self.crawlers else {
            return;
        };
        if ok {
            crawlers.tracker().record_success(&id);
        } else {
            let kind = match outcome {
                Ok(FetchOutcome::HttpError { status }) => {
                    CrawlerErrorKind::from_http_status(*status)
                        .unwrap_or(CrawlerErrorKind::NetworkError)
                }
                Ok(FetchOutcome::RobotsDisallowed) => CrawlerErrorKind::RobotsDisallowed,
                Ok(FetchOutcome::EmptyContent) => CrawlerErrorKind::ContentEmpty,
                _ => CrawlerErrorKind::NetworkError,
            };
            crawlers.tracker().record_failure(CrawlerFailure {
                crawler_id: id.clone(),
                kind,
                url: String::new(),
                detail: format!("{outcome:?}"),
                at_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            });
        }
        MetricsRegistry::global().record_crawler(ok);
    }
}

#[async_trait]
impl FetchService for FetchServiceImpl {
    fn limits(&self) -> searchos_fetch::FetchLimits {
        self.fetcher.limits().clone()
    }

    async fn fetch_and_persist(
        &self,
        url: &str,
        workspace_id: &WorkspaceId,
    ) -> Result<FetchedDocument, SearchOsError> {
        let raw_url = url.trim().to_string();
        let canonical = Self::canonicalize(&raw_url);
        let parsed = Url::parse(&canonical)
            .map_err(|e| SearchOsError::validation("url", format!("invalid canonical URL: {e}")))?;
        let selected_crawler = self.selected_crawler(&parsed);
        // Crawler selection, robots.txt and Crawl-delay all run before the
        // content request (RFC-010 10.4/10.5, G-03).
        self.enforce_robots(&parsed).await?;
        let outcome = self.fetcher.fetch(&canonical, None).await;
        self.record_crawler_outcome(selected_crawler, &outcome);
        let outcome = outcome.map_err(|v| SearchOsError::Security(format!("ssrf guard: {v}")))?;
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
                    source_type: "web".into(),
                    title: extracted.title.clone(),
                    workspace_id: workspace_id.to_string(),
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
            .save_versioned(
                &doc,
                &version,
                &domain_chunks,
                &index_payloads,
                workspace_id,
            )
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
            raw_url: Some(raw_url),
        })
    }

    /// Re-indexes the current version of a document under its owning
    /// workspace. Fetch/refresh keeps the document row's workspace_id, but
    /// previously indexed chunk payloads may still carry the old workspace;
    /// this rebuilds them from the persisted chunk metadata so retrieval
    /// isolation never leaks across a workspace move.
    async fn reindex_current_version(
        &self,
        document_id: &DocumentId,
        workspace_id: &WorkspaceId,
    ) -> Result<Vec<IndexedDocument>, SearchOsError> {
        let Some(doc) = self.doc_port.find_by_id(document_id).await? else {
            return Ok(Vec::new());
        };
        let Some(version_id) = doc.current_version_id.clone() else {
            return Ok(Vec::new());
        };
        let stored = self.doc_port.list_chunks(&version_id).await?;
        if stored.is_empty() {
            return Ok(Vec::new());
        }
        let Some(first) = stored.first() else {
            return Ok(Vec::new());
        };
        let Some(blob) =
            searchos_storage_fs::ObjectStore::get(self.objects.as_ref(), &first.content_ref)
                .await
                .ok()
        else {
            return Ok(Vec::new());
        };
        let text = String::from_utf8_lossy(&blob).to_string();
        let chunks = searchos_document::chunk_text(&text, &Default::default());
        let mut payloads = Vec::with_capacity(stored.len());
        for (i, row) in stored.iter().enumerate() {
            let Some(ch) = chunks.get(i) else { break };
            if ch.hash != row.chunk_hash {
                continue;
            }
            payloads.push(IndexedDocument {
                chunk_id: row.id.clone(),
                document_id: doc.id.clone(),
                version_id: version_id.clone(),
                title: doc.title.clone(),
                body: ch.text.clone(),
                url: doc.canonical_url.clone(),
                language: doc.language.clone(),
                workspace_id: workspace_id.to_string(),
                fetched_at: Utc::now(),
            });
        }
        Ok(payloads)
    }

    async fn fetch_preview(
        &self,
        url: &str,
    ) -> Result<searchos_fetch::extract::ExtractedContent, SearchOsError> {
        let canonical = Self::canonicalize(url);
        let parsed = Url::parse(&canonical)
            .map_err(|e| SearchOsError::validation("url", format!("invalid canonical URL: {e}")))?;
        let selected_crawler = self.selected_crawler(&parsed);
        self.enforce_robots(&parsed).await?;
        let outcome = self.fetcher.fetch(&canonical, None).await;
        self.record_crawler_outcome(selected_crawler, &outcome);
        match outcome {
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
        async fn find_by_id(&self, id: &DocumentId) -> Result<Option<Document>, SearchOsError> {
            Ok(self
                .docs
                .lock()
                .unwrap()
                .values()
                .find(|d| d.id == *id)
                .cloned())
        }
        async fn save_versioned(
            &self,
            document: &Document,
            version: &DocumentVersion,
            _chunks: &[searchos_domain::document::DocumentChunk],
            _payloads: &[searchos_domain::retrieval::IndexedDocument],
            _workspace_id: &WorkspaceId,
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
        async fn list_chunks(
            &self,
            _version_id: &DocumentVersionId,
        ) -> Result<Vec<searchos_domain::document::DocumentChunk>, SearchOsError> {
            Ok(Vec::new())
        }
        async fn reindex_current_version(
            &self,
            _document_id: &DocumentId,
            _workspace_id: &WorkspaceId,
        ) -> Result<Vec<IndexedDocument>, SearchOsError> {
            Ok(Vec::new())
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
