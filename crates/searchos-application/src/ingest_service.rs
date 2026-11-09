//! IngestService (RFC-029): local data import into the knowledge store.
//!
//! Text, single files and directories are validated, extracted, hashed and
//! persisted through the **same** [`DocumentPort`] as URL fetch, so
//! versioning, chunking, outbox indexing, workspace isolation and index
//! rebuild behave identically (RFC-029 4).
//!
//! Local sources are identified by the canonical URI scheme
//! `searchos-local://<workspace>/<source_key>` and never touch the network.

use async_trait::async_trait;
use chrono::Utc;
use searchos_document::canonical::canonical_url;
use searchos_document::hash::{content_hash, content_ref};
use searchos_domain::document::{normalize_source_key, Document, DocumentVersion};
use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId, SourceId, WorkspaceId};
use searchos_domain::retrieval::IndexedDocument;
use searchos_domain::SearchOsError;
use searchos_storage_fs::ObjectStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::fetch_service::DocumentPort;

/// Source type for pasted / typed text.
pub const LOCAL_SOURCE_TYPE_TEXT: &str = "local_text";
/// Source type for a single uploaded file.
pub const LOCAL_SOURCE_TYPE_FILE: &str = "local_file";
/// Source type for a directory import.
pub const LOCAL_SOURCE_TYPE_DIRECTORY: &str = "local_directory";
/// Explicit mixed batch source type (text + files in one call).
pub const LOCAL_SOURCE_TYPE_MIXED: &str = "local_mixed";

/// Default per-document size bound (20 MB, RFC-029 9.3).
pub const DEFAULT_MAX_BYTES_PER_DOCUMENT: usize = 20 * 1024 * 1024;
/// Default per-batch size bound (200 MB, RFC-029 9.3).
pub const DEFAULT_MAX_BYTES_PER_BATCH: usize = 200 * 1024 * 1024;

/// Caller-supplied metadata for one ingest (RFC-029 5.3).
#[derive(Debug, Clone)]
pub struct IngestRequest {
    /// Logical identity. When absent, the source is content-addressed
    /// (`sha256(content)[:16]`), so identical content dedupes.
    pub source_key: Option<String>,
    pub title: Option<String>,
    /// Hint only; the sniffer has the final say (RFC-029 9.5).
    pub content_type: Option<String>,
    pub language: Option<String>,
    pub workspace_id: WorkspaceId,
    /// Caller labels. Core never queries these.
    pub metadata: serde_json::Value,
}

impl IngestRequest {
    /// Creates a request for the `default` workspace.
    pub fn new(workspace_id: WorkspaceId) -> Self {
        Self {
            source_key: None,
            title: None,
            content_type: None,
            language: None,
            workspace_id,
            metadata: serde_json::Value::Null,
        }
    }
}

/// Payload variants (RFC-029 5.3).
#[derive(Debug, Clone)]
pub enum IngestPayload {
    Text(String),
    File(PathBuf),
    /// In-memory file bytes from an HTTP batch / virtual directory.
    /// `file_name` is a label only.
    Upload {
        file_name: String,
        bytes: Vec<u8>,
    },
}

/// One item of a batch import.
#[derive(Debug, Clone)]
pub struct IngestBatchItem {
    pub request: IngestRequest,
    pub payload: IngestPayload,
}

/// Result of a successful ingest (RFC-029 6.1).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestedDocument {
    pub document_id: DocumentId,
    pub version_id: DocumentVersionId,
    pub canonical_url: String,
    pub source_type: String,
    pub content_hash: String,
    pub changed: bool,
    pub title: Option<String>,
    pub chunk_count: usize,
    pub status: String,
}

/// One failed batch item.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestFailure {
    pub source_key: String,
    pub error: String,
}

/// Batch import result (RFC-029 6.1, 8).
#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestBatchResult {
    pub job_id: String,
    pub state: String,
    pub total_items: usize,
    pub ok_items: usize,
    pub failed_items: usize,
    pub documents: Vec<IngestedDocument>,
    pub failures: Vec<IngestFailure>,
}

/// Batch job tracking port (RFC-029 7). Implemented by the storage adapter;
/// the application crate stays storage-agnostic.
#[async_trait]
pub trait IngestJobPort: Send + Sync {
    async fn create(
        &self,
        id: &str,
        workspace_id: &str,
        source_type: &str,
        total_items: i64,
    ) -> Result<(), SearchOsError>;
    async fn mark_running(&self, id: &str) -> Result<(), SearchOsError>;
    async fn finish(
        &self,
        id: &str,
        state: &str,
        ok_items: i64,
        failed_items: i64,
        error_detail: Option<&str>,
    ) -> Result<(), SearchOsError>;
    async fn insert_item(
        &self,
        id: &str,
        job_id: &str,
        source_key: &str,
        state: &str,
        document_id: Option<&str>,
        error_detail: Option<&str>,
    ) -> Result<(), SearchOsError>;
}

/// Ingest use case (RFC-029).
#[async_trait]
pub trait IngestService: Send + Sync {
    /// Ingests pasted / typed text.
    async fn ingest_text(
        &self,
        req: IngestRequest,
        text: &str,
    ) -> Result<IngestedDocument, SearchOsError>;

    /// Reads and ingests a single file.
    async fn ingest_file(
        &self,
        req: IngestRequest,
        path: &Path,
    ) -> Result<IngestedDocument, SearchOsError>;

    /// Ingests raw file bytes (HTTP multipart / SDK). `file_name` is a label
    /// only — it never touches the server filesystem (RFC-029 9.4).
    async fn ingest_upload(
        &self,
        req: IngestRequest,
        bytes: &[u8],
        file_name: Option<&str>,
    ) -> Result<IngestedDocument, SearchOsError>;

    /// Ingests a batch, tracking a job and per-item outcomes.
    async fn ingest_batch(
        &self,
        items: Vec<IngestBatchItem>,
    ) -> Result<IngestBatchResult, SearchOsError>;
}

/// Concrete ingest service.
pub struct IngestServiceImpl {
    objects: Arc<dyn ObjectStore>,
    doc_port: Arc<dyn DocumentPort>,
    jobs: Option<Arc<dyn IngestJobPort>>,
    max_bytes_per_document: usize,
    max_bytes_per_batch: usize,
}

impl IngestServiceImpl {
    pub fn new(objects: Arc<dyn ObjectStore>, doc_port: Arc<dyn DocumentPort>) -> Self {
        Self {
            objects,
            doc_port,
            jobs: None,
            max_bytes_per_document: DEFAULT_MAX_BYTES_PER_DOCUMENT,
            max_bytes_per_batch: DEFAULT_MAX_BYTES_PER_BATCH,
        }
    }

    /// Attaches batch job tracking.
    pub fn with_job_port(mut self, jobs: Arc<dyn IngestJobPort>) -> Self {
        self.jobs = Some(jobs);
        self
    }

    /// Overrides the size bounds (tests / configuration).
    pub fn with_limits(mut self, per_document: usize, per_batch: usize) -> Self {
        self.max_bytes_per_document = per_document;
        self.max_bytes_per_batch = per_batch;
        self
    }

    /// Reads a file, enforcing the size bound before decoding.
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, SearchOsError> {
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|e| SearchOsError::Validation(validation("path", &e.to_string())))?;
        if !metadata.is_file() {
            return Err(SearchOsError::Validation(validation(
                "path",
                "not a regular file",
            )));
        }
        if metadata.len() as usize > self.max_bytes_per_document {
            return Err(SearchOsError::Validation(validation(
                "content",
                "CONTENT_TOO_LARGE",
            )));
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| SearchOsError::Validation(validation("path", &e.to_string())))?;
        if bytes.len() > self.max_bytes_per_document {
            return Err(SearchOsError::Validation(validation(
                "content",
                "CONTENT_TOO_LARGE",
            )));
        }
        Ok(bytes)
    }

    /// Core ingest path shared by text and file payloads.
    async fn ingest_bytes(
        &self,
        req: IngestRequest,
        bytes: &[u8],
        source_type: &str,
        file_name: Option<&str>,
    ) -> Result<IngestedDocument, SearchOsError> {
        if bytes.is_empty() {
            return Err(SearchOsError::Validation(validation(
                "content",
                "EMPTY_CONTENT",
            )));
        }
        if bytes.len() > self.max_bytes_per_document {
            return Err(SearchOsError::Validation(validation(
                "content",
                "CONTENT_TOO_LARGE",
            )));
        }

        let normalized = normalize_content(bytes, req.content_type.as_deref(), file_name)?;
        let text = normalized.text;
        if text.trim().is_empty() {
            return Err(SearchOsError::Validation(validation(
                "content",
                "EMPTY_CONTENT",
            )));
        }

        // Content-addressed persistence: raw = plain text, md = markdown.
        let raw_ref = self
            .objects
            .put(text.as_bytes())
            .await
            .map_err(|e| SearchOsError::Storage(e.to_string()))?;
        let md_ref = self
            .objects
            .put(normalized.markdown.as_bytes())
            .await
            .map_err(|e| SearchOsError::Storage(e.to_string()))?;
        let hash_hex = content_hash(text.as_bytes());
        let content_hash = hash_hex.clone();

        // Source identity: caller key, else content-addressed.
        let key = match req.source_key.as_deref() {
            Some(k) if !k.trim().is_empty() => normalize_source_key(k),
            _ => hash_hex[..16].to_string(),
        };
        // All index writes / find_by_url calls use the one canonical URL
        // implementation (Part 30 INV-30-15).
        let canonical = canonical_url(&Document::local_canonical_uri(
            req.workspace_id.as_str(),
            &key,
        ));

        let existing = self.doc_port.find_by_url(&canonical).await?;
        let doc = match &existing {
            Some(d) => d.clone(),
            None => Document {
                id: DocumentId::generate(),
                source_id: SourceId::generate(),
                canonical_url: canonical.clone(),
                current_version_id: None,
                content_type: normalized.content_type.clone(),
                language: req.language.clone().or_else(|| detected_language(&text)),
                source_type: source_type.to_string(),
                title: req
                    .title
                    .clone()
                    .or_else(|| normalized.title.clone())
                    .or_else(|| file_name.map(|s| s.to_string())),
                workspace_id: req.workspace_id.as_str().to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        };

        let version = DocumentVersion {
            id: DocumentVersionId::generate(),
            document_id: doc.id.clone(),
            content_hash: content_hash.clone(),
            raw_content_ref: raw_ref,
            markdown_ref: Some(md_ref),
            fetched_at: Utc::now(),
            published_at: None,
            extractor_version: searchos_document::EXTRACTOR_VERSION.into(),
            crawler_version: None,
        };

        let chunks = searchos_document::chunk_text(&text, &Default::default());
        let domain_chunks: Vec<searchos_domain::document::DocumentChunk> = chunks
            .iter()
            .map(|c| searchos_domain::document::DocumentChunk {
                id: ChunkId::generate(),
                version_id: version.id.clone(),
                document_id: doc.id.clone(),
                chunk_hash: c.hash.clone(),
                content_ref: content_ref(text.as_bytes()),
                text: c.text.clone(),
                token_count: Some(c.text.len() as u32 / 4),
                chunk_index: c.index,
                created_at: Utc::now(),
            })
            .collect();
        let title = doc.title.clone();
        let index_payloads: Vec<IndexedDocument> = domain_chunks
            .iter()
            .map(|ch| IndexedDocument {
                chunk_id: ch.id.clone(),
                document_id: doc.id.clone(),
                version_id: version.id.clone(),
                title: title.clone(),
                body: ch.text.clone(),
                url: canonical.clone(),
                language: doc.language.clone(),
                workspace_id: req.workspace_id.to_string(),
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
                &req.workspace_id,
            )
            .await?;
        let changed = matches!(diff, searchos_document::VersionDiff::NewVersion);
        // On an unchanged save the stored version is the existing current one;
        // the freshly generated id was never persisted (RFC-006 6.4).
        let version_id = if changed {
            version.id
        } else {
            doc.current_version_id.clone().unwrap_or(version.id)
        };

        Ok(IngestedDocument {
            document_id: doc.id,
            version_id,
            canonical_url: canonical,
            source_type: source_type.to_string(),
            content_hash,
            changed,
            title,
            chunk_count: domain_chunks.len(),
            status: if changed { "indexed" } else { "unchanged" }.to_string(),
        })
    }
}

#[async_trait]
impl IngestService for IngestServiceImpl {
    async fn ingest_text(
        &self,
        req: IngestRequest,
        text: &str,
    ) -> Result<IngestedDocument, SearchOsError> {
        self.ingest_bytes(req, text.as_bytes(), LOCAL_SOURCE_TYPE_TEXT, None)
            .await
    }

    async fn ingest_file(
        &self,
        req: IngestRequest,
        path: &Path,
    ) -> Result<IngestedDocument, SearchOsError> {
        let bytes = self.read_file(path).await?;
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
        self.ingest_bytes(req, &bytes, LOCAL_SOURCE_TYPE_FILE, file_name.as_deref())
            .await
    }

    async fn ingest_upload(
        &self,
        req: IngestRequest,
        bytes: &[u8],
        file_name: Option<&str>,
    ) -> Result<IngestedDocument, SearchOsError> {
        self.ingest_bytes(req, bytes, LOCAL_SOURCE_TYPE_FILE, file_name)
            .await
    }

    async fn ingest_batch(
        &self,
        items: Vec<IngestBatchItem>,
    ) -> Result<IngestBatchResult, SearchOsError> {
        let total = items.len();
        let job_id = searchos_domain::new_id("ing");
        let workspace = items
            .first()
            .map(|i| i.request.workspace_id.to_string())
            .unwrap_or_else(|| "default".to_string());
        let source_type = if items
            .iter()
            .all(|i| matches!(i.payload, IngestPayload::Text(_)))
        {
            LOCAL_SOURCE_TYPE_TEXT
        } else if items
            .iter()
            .all(|i| matches!(i.payload, IngestPayload::File(_)))
        {
            LOCAL_SOURCE_TYPE_FILE
        } else if items
            .iter()
            .all(|i| matches!(i.payload, IngestPayload::Upload { .. }))
        {
            LOCAL_SOURCE_TYPE_DIRECTORY
        } else {
            LOCAL_SOURCE_TYPE_MIXED
        };

        if let Some(jobs) = &self.jobs {
            jobs.create(&job_id, &workspace, source_type, total as i64)
                .await?;
            jobs.mark_running(&job_id).await?;
        }

        // Pre-check the WHOLE batch before writing any document or object
        // (RFC-029 9.3 / AC7).
        let mut total_bytes = 0usize;
        for item in &items {
            total_bytes += ingest_payload_len(item).await;
        }
        if total_bytes > self.max_bytes_per_batch {
            let failures: Vec<IngestFailure> = items
                .iter()
                .map(|item| IngestFailure {
                    source_key: ingest_item_key(&item.request, &item.payload),
                    error: "CONTENT_TOO_LARGE".into(),
                })
                .collect();
            if let Some(jobs) = &self.jobs {
                for item in &items {
                    let _ = jobs
                        .insert_item(
                            &searchos_domain::new_id("iji"),
                            &job_id,
                            &ingest_item_key(&item.request, &item.payload),
                            "failed",
                            None,
                            Some("CONTENT_TOO_LARGE"),
                        )
                        .await;
                }
                jobs.finish(
                    &job_id,
                    "failed",
                    0,
                    failures.len() as i64,
                    Some("batch content too large"),
                )
                .await?;
            }
            return Ok(IngestBatchResult {
                job_id,
                state: "failed".into(),
                total_items: total,
                ok_items: 0,
                failed_items: failures.len(),
                documents: Vec::new(),
                failures,
            });
        }

        let mut documents = Vec::new();
        let mut failures = Vec::new();
        for item in &items {
            let key = ingest_item_key(&item.request, &item.payload);
            let result = match &item.payload {
                IngestPayload::Text(t) => self.ingest_text(item.request.clone(), t).await,
                IngestPayload::File(p) => self.ingest_file(item.request.clone(), p).await,
                IngestPayload::Upload { bytes, file_name } => {
                    self.ingest_upload(item.request.clone(), bytes, Some(file_name))
                        .await
                }
            };
            match result {
                Ok(doc) => {
                    if let Some(jobs) = &self.jobs {
                        if jobs
                            .insert_item(
                                &searchos_domain::new_id("iji"),
                                &job_id,
                                &key,
                                "ok",
                                Some(doc.document_id.as_str()),
                                None,
                            )
                            .await
                            .is_err()
                        {
                            failures.push(IngestFailure {
                                source_key: key,
                                error: "job_item_write_failed".into(),
                            });
                            continue;
                        }
                    }
                    documents.push(doc);
                }
                Err(e) => {
                    if let Some(jobs) = &self.jobs {
                        let _ = jobs
                            .insert_item(
                                &searchos_domain::new_id("iji"),
                                &job_id,
                                &key,
                                "failed",
                                None,
                                Some(&e.to_string()),
                            )
                            .await;
                    }
                    failures.push(IngestFailure {
                        source_key: key,
                        error: e.to_string(),
                    });
                }
            }
        }

        let ok = documents.len();
        let failed = failures.len();
        let state = if failed == 0 {
            "completed"
        } else if ok == 0 {
            "failed"
        } else {
            "partial"
        };
        if let Some(jobs) = &self.jobs {
            jobs.finish(&job_id, state, ok as i64, failed as i64, None)
                .await?;
        }
        Ok(IngestBatchResult {
            job_id,
            state: state.to_string(),
            total_items: total,
            ok_items: ok,
            failed_items: failed,
            documents,
            failures,
        })
    }
}

fn ingest_item_key(req: &IngestRequest, payload: &IngestPayload) -> String {
    req.source_key.clone().unwrap_or_else(|| match payload {
        IngestPayload::Text(_) => "<content-addressed>".to_string(),
        IngestPayload::File(path) => path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<file>")
            .to_string(),
        IngestPayload::Upload { file_name, .. } => file_name.clone(),
    })
}

async fn ingest_payload_len(item: &IngestBatchItem) -> usize {
    match &item.payload {
        IngestPayload::Text(text) => text.len(),
        IngestPayload::File(path) => tokio::fs::metadata(path)
            .await
            .map(|m| m.len() as usize)
            .unwrap_or(0),
        IngestPayload::Upload { bytes, .. } => bytes.len(),
    }
}

fn validation(field: &str, message: &str) -> searchos_domain::ValidationError {
    searchos_domain::ValidationError {
        field: field.to_string(),
        message: message.to_string(),
    }
}

/// Normalized content ready for persistence.
#[derive(Debug)]
struct NormalizedContent {
    text: String,
    markdown: String,
    title: Option<String>,
    content_type: String,
}

/// Validates, sniffs and extracts content (RFC-029 9).
fn normalize_content(
    bytes: &[u8],
    declared: Option<&str>,
    file_name: Option<&str>,
) -> Result<NormalizedContent, SearchOsError> {
    let sniffed = sniff_content_type(bytes, declared, file_name);
    let text = match sniffed.as_str() {
        "text/html" => {
            let decoded = decode_utf8(bytes)?;
            let extracted = searchos_fetch::extract::extract_html(&decoded);
            return Ok(NormalizedContent {
                text: extracted.text,
                markdown: extracted.markdown,
                title: extracted.title,
                content_type: "text/html".into(),
            });
        }
        "application/octet-stream"
        | "application/pdf"
        | "application/msword"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            return Err(SearchOsError::Validation(validation(
                "content_type",
                "UNSUPPORTED_CONTENT_TYPE",
            )));
        }
        _ => decode_utf8(bytes)?,
    };

    // Strip a UTF-8 BOM if present.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_string();
    let title = extract_text_title(&text, file_name);
    Ok(NormalizedContent {
        text: text.clone(),
        markdown: text,
        title,
        content_type: sniffed,
    })
}

/// Decodes UTF-8 strictly (RFC-029 8).
fn decode_utf8(bytes: &[u8]) -> Result<String, SearchOsError> {
    String::from_utf8(bytes.to_vec()).map_err(|e| {
        SearchOsError::Validation(validation(
            "content",
            &format!("INVALID_ENCODING at byte {}", e.utf8_error().valid_up_to()),
        ))
    })
}

/// Content-type sniffing: magic bytes → declared hint → extension (RFC-029 9.5).
fn sniff_content_type(bytes: &[u8], declared: Option<&str>, file_name: Option<&str>) -> String {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_lowercase();
    let looks_html = head.contains("<!doctype html")
        || head.contains("<html")
        || head.contains("<head")
        || head.contains("<body");
    if looks_html {
        return "text/html".into();
    }

    if let Some(decl) = declared {
        let d = decl.to_lowercase();
        if d.contains("html") {
            return "text/html".into();
        }
        if d.contains("markdown") {
            return "text/markdown".into();
        }
        if d.contains("json") {
            return "application/json".into();
        }
        if d.contains("csv") {
            return "text/csv".into();
        }
        if d.starts_with("text/") {
            return d;
        }
    }

    let ext = file_name
        .and_then(|n| n.rsplit('.').next())
        .map(|e| e.to_lowercase());
    match ext.as_deref() {
        Some("html") | Some("htm") => "text/html".into(),
        Some("md") | Some("markdown") => "text/markdown".into(),
        Some("json") => "application/json".into(),
        Some("csv") => "text/csv".into(),
        Some("txt") | Some("text") | Some("log") | Some("rs") | Some("py") | Some("js")
        | Some("ts") | Some("yaml") | Some("yml") | Some("toml") | Some("xml") | Some("sql")
        | Some("sh") => "text/plain".into(),
        Some("pdf") => "application/pdf".into(),
        Some(_) => "application/octet-stream".into(),
        None => "text/plain".into(),
    }
}

/// Derives a title from the first non-empty line when the caller gave none.
fn extract_text_title(text: &str, file_name: Option<&str>) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        // Markdown heading.
        if let Some(rest) = t.strip_prefix('#') {
            let h = rest.trim();
            if !h.is_empty() {
                return Some(h.to_string());
            }
        }
        if t.len() <= 200 {
            return Some(t.to_string());
        }
        return Some(t.chars().take(200).collect());
    }
    file_name.map(|s| s.to_string())
}

/// Best-effort language detection (RFC-003 3.4): CJK vs Latin heuristic.
fn detected_language(text: &str) -> Option<String> {
    let cjk = text
        .chars()
        .filter(|c| matches!(*c, '\u{4e00}'..='\u{9fff}' | '\u{3040}'..='\u{30ff}' | '\u{ac00}'..='\u{d7af}'))
        .count();
    let total = text.chars().count().max(1);
    if cjk * 4 > total {
        Some("zh".into())
    } else {
        Some("en".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_html_by_magic_bytes() {
        assert_eq!(
            sniff_content_type(b"<!DOCTYPE html><html></html>", None, None),
            "text/html"
        );
        assert_eq!(
            sniff_content_type(b"# Title\n\nbody", None, Some("notes.md")),
            "text/markdown"
        );
        assert_eq!(
            sniff_content_type(b"plain text", None, Some("notes.txt")),
            "text/plain"
        );
        assert_eq!(
            sniff_content_type(b"binary\x00\x01", None, Some("blob.bin")),
            "application/octet-stream"
        );
    }

    #[test]
    fn rejects_unknown_binary_type() {
        let err = normalize_content(b"%PDF-1.4 binary", None, Some("a.pdf")).unwrap_err();
        assert!(err.to_string().contains("UNSUPPORTED_CONTENT_TYPE"));
    }

    #[test]
    fn rejects_invalid_utf8_with_offset() {
        let err = normalize_content(&[0xff, 0xfe, 0x00], None, Some("a.txt")).unwrap_err();
        assert!(err.to_string().contains("INVALID_ENCODING"));
    }

    #[test]
    fn html_is_extracted_and_sanitized() {
        let n = normalize_content(
            b"<html><head><title>T</title><script>bad()</script></head><body><p>Hello</p></body></html>",
            None,
            None,
        )
        .unwrap();
        assert_eq!(n.title.as_deref(), Some("T"));
        assert!(n.text.contains("Hello"));
        assert!(!n.text.contains("bad()"));
    }

    #[test]
    fn text_title_comes_from_first_line_or_heading() {
        let n = normalize_content(b"# Real Title\nbody", None, Some("f.md")).unwrap();
        assert_eq!(n.title.as_deref(), Some("Real Title"));
        let n2 = normalize_content(b"first line\nsecond", None, Some("f.txt")).unwrap();
        assert_eq!(n2.title.as_deref(), Some("first line"));
    }

    #[test]
    fn language_detection_heuristic() {
        assert_eq!(detected_language("这是一段中文"), Some("zh".into()));
        assert_eq!(detected_language("this is english"), Some("en".into()));
    }

    // ---- service-level tests with in-memory adapters ----

    use super::{IngestBatchItem, IngestPayload, IngestService, IngestServiceImpl};
    use searchos_domain::document::{Document, DocumentVersion};
    use searchos_domain::retrieval::IndexedDocument;
    use searchos_storage_fs::{ObjectStore, StoreError};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemObjects {
        files: Mutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl ObjectStore for MemObjects {
        async fn put(&self, bytes: &[u8]) -> Result<String, StoreError> {
            let r = content_ref(bytes);
            self.files.lock().unwrap().insert(r.clone(), bytes.to_vec());
            Ok(r)
        }
        async fn get(&self, r: &str) -> Result<Vec<u8>, StoreError> {
            self.files
                .lock()
                .unwrap()
                .get(r)
                .cloned()
                .ok_or_else(|| StoreError::InvalidRef(r.to_string()))
        }
        async fn delete(&self, r: &str) -> Result<(), StoreError> {
            self.files.lock().unwrap().remove(r);
            Ok(())
        }
        async fn exists(&self, r: &str) -> Result<bool, StoreError> {
            Ok(self.files.lock().unwrap().contains_key(r))
        }
    }

    #[derive(Default)]
    struct MemDocPort {
        saves: AtomicUsize,
        docs: Mutex<HashMap<String, Document>>,
    }

    #[async_trait::async_trait]
    impl DocumentPort for MemDocPort {
        async fn find_by_url(&self, url: &str) -> Result<Option<Document>, SearchOsError> {
            Ok(self.docs.lock().unwrap().get(url).cloned())
        }
        async fn find_by_id(
            &self,
            id: &searchos_domain::ids::DocumentId,
        ) -> Result<Option<Document>, SearchOsError> {
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
            _version: &DocumentVersion,
            _chunks: &[searchos_domain::document::DocumentChunk],
            _payloads: &[IndexedDocument],
            _workspace_id: &searchos_domain::ids::WorkspaceId,
        ) -> Result<searchos_document::VersionDiff, SearchOsError> {
            self.saves.fetch_add(1, Ordering::SeqCst);
            self.docs
                .lock()
                .unwrap()
                .insert(document.canonical_url.clone(), document.clone());
            Ok(searchos_document::VersionDiff::NewVersion)
        }
        async fn get_current_version(
            &self,
            _id: &searchos_domain::ids::DocumentId,
        ) -> Result<Option<DocumentVersion>, SearchOsError> {
            Ok(None)
        }
        async fn list_chunks(
            &self,
            _version_id: &searchos_domain::ids::DocumentVersionId,
        ) -> Result<Vec<searchos_domain::document::DocumentChunk>, SearchOsError> {
            Ok(Vec::new())
        }
        async fn reindex_current_version(
            &self,
            _document_id: &searchos_domain::ids::DocumentId,
            _workspace_id: &searchos_domain::ids::WorkspaceId,
        ) -> Result<Vec<IndexedDocument>, SearchOsError> {
            Ok(Vec::new())
        }
    }

    fn service() -> IngestServiceImpl {
        IngestServiceImpl::new(
            Arc::new(MemObjects::default()),
            Arc::new(MemDocPort::default()),
        )
    }

    #[tokio::test]
    async fn oversize_content_is_rejected_before_any_write() {
        let objects = Arc::new(MemObjects::default());
        let port = Arc::new(MemDocPort::default());
        let svc = IngestServiceImpl::new(objects.clone(), port.clone()).with_limits(64, 1024);
        let req = IngestRequest::new(searchos_domain::ids::WorkspaceId::new("default"));
        let err = svc.ingest_text(req, &"x".repeat(65)).await.unwrap_err();
        assert!(err.to_string().contains("CONTENT_TOO_LARGE"));
        assert_eq!(
            port.saves.load(Ordering::SeqCst),
            0,
            "no persistence attempted"
        );
        assert!(objects.files.lock().unwrap().is_empty(), "no object write");
    }

    #[tokio::test]
    async fn whitespace_only_content_is_rejected() {
        let svc = service();
        let req = IngestRequest::new(searchos_domain::ids::WorkspaceId::new("default"));
        let err = svc.ingest_text(req, "   \n\t  ").await.unwrap_err();
        assert!(err.to_string().contains("EMPTY_CONTENT"));
    }

    #[tokio::test]
    async fn batch_bound_precheck_rejects_without_partial_writes() {
        let svc = IngestServiceImpl::new(
            Arc::new(MemObjects::default()),
            Arc::new(MemDocPort::default()),
        )
        .with_limits(100, 150);
        let mk = |key: &str, body: &str| IngestBatchItem {
            request: {
                let mut r = IngestRequest::new(searchos_domain::ids::WorkspaceId::new("default"));
                r.source_key = Some(key.to_string());
                r
            },
            payload: IngestPayload::Text(body.to_string()),
        };
        let result = svc
            .ingest_batch(vec![mk("a", &"a".repeat(80)), mk("b", &"b".repeat(80))])
            .await
            .unwrap();
        assert_eq!(result.ok_items, 0, "pre-check rejects the whole batch");
        assert_eq!(result.failed_items, 2);
        assert_eq!(result.state, "failed");
        assert!(result.documents.is_empty());
    }

    #[tokio::test]
    async fn oversize_upload_rejected_before_object_write() {
        let mem_objects = Arc::new(MemObjects::default());
        let objects: Arc<dyn ObjectStore> = Arc::clone(&mem_objects) as Arc<dyn ObjectStore>;
        let svc =
            IngestServiceImpl::new(objects, Arc::new(MemDocPort::default())).with_limits(10, 100);
        let req = IngestRequest::new(searchos_domain::ids::WorkspaceId::new("default"));
        let err = svc
            .ingest_upload(req, b"this content is too large", Some("big.txt"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("CONTENT_TOO_LARGE"));
        assert!(mem_objects.files.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn oversize_batch_precheck_rejects_without_partial_writes() {
        let mem_objects = Arc::new(MemObjects::default());
        let objects: Arc<dyn ObjectStore> = Arc::clone(&mem_objects) as Arc<dyn ObjectStore>;
        let svc =
            IngestServiceImpl::new(objects, Arc::new(MemDocPort::default())).with_limits(100, 150);
        let mk = |key: &str, body: &str| IngestBatchItem {
            request: {
                let mut r = IngestRequest::new(searchos_domain::ids::WorkspaceId::new("default"));
                r.source_key = Some(key.to_string());
                r
            },
            payload: IngestPayload::Text(body.to_string()),
        };
        let result = svc
            .ingest_batch(vec![mk("a", &"a".repeat(80)), mk("b", &"b".repeat(80))])
            .await
            .unwrap();
        assert_eq!(result.ok_items, 0);
        assert_eq!(result.failed_items, 2);
        assert!(result.documents.is_empty());
        assert!(mem_objects.files.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn content_addressed_dedup_when_no_source_key() {
        let svc = service();
        let req = IngestRequest::new(searchos_domain::ids::WorkspaceId::new("default"));
        let first = svc.ingest_text(req.clone(), "same body").await.unwrap();
        assert!(first.changed);
        assert!(first.canonical_url.starts_with("searchos-local://default/"));
        // Same content without a key maps to the same canonical URI.
        let second = svc.ingest_text(req, "same body").await.unwrap();
        assert_eq!(first.canonical_url, second.canonical_url);
    }
}
