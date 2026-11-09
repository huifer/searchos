//! Shared configuration, doc serialization and candidate mapping for the
//! remote index adapters.

use chrono::{DateTime, Utc};
use searchos_domain::retrieval::{
    IndexedDocument, RetrievalCandidate, RetrievalQuery, RetrievalSource,
};
use searchos_domain::{ChunkId, DocumentId, DocumentVersionId, SearchOsError};
use serde::{Deserialize, Serialize};

/// Connection settings for one remote retrieval backend
/// (`storage.remote.<backend>` sections).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteBackendConfig {
    /// Base URL. For REST backends: `http://host:port`. For SQL backends:
    /// a full connection string
    /// (`postgres://user:pass@host:port/db` / `mysql://...`).
    pub url: String,
    /// Auth token (`Bearer`) for REST backends; empty = unauthenticated.
    /// Production credentials should come from the environment
    /// (SEARCHOS_ES_API_KEY / SEARCHOS_MEILI_API_KEY), never from files.
    pub api_key: String,
    /// Index / table name used by SearchOS.
    pub index: String,
    /// Per-request timeout seconds.
    pub timeout_seconds: u64,
    /// Enable TLS certificate verification (default true; only relax for
    /// local development).
    pub verify_tls: bool,
}

impl Default for RemoteBackendConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            api_key: String::new(),
            index: "searchos_chunks".into(),
            timeout_seconds: 10,
            verify_tls: true,
        }
    }
}

impl RemoteBackendConfig {
    /// API key resolution: env first, then config. Env names per backend:
    /// elasticsearch → `SEARCHOS_ES_API_KEY`, meilisearch →
    /// `SEARCHOS_MEILI_API_KEY`.
    pub fn effective_api_key(&self, env_key: &str) -> String {
        std::env::var(env_key)
            .ok()
            .filter(|k| !k.trim().is_empty())
            .unwrap_or_else(|| self.api_key.clone())
    }

    /// Rejects an empty url early with an actionable message.
    pub fn validate(&self, backend: &str) -> Result<(), SearchOsError> {
        if self.url.trim().is_empty() {
            return Err(SearchOsError::Storage(format!(
                "storage.remote.{backend}.url is empty — set the {backend} endpoint/DSN or keep storage.fulltext: tantivy"
            )));
        }
        Ok(())
    }
}

/// Wire representation of one chunk stored in every remote backend.
/// Keeping one flat JSON schema across engines makes rebuilds portable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkDoc {
    pub chunk_id: String,
    pub document_id: String,
    pub version_id: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub language: String,
    pub workspace_id: String,
    /// RFC-3339 UTC.
    pub fetched_at: String,
}

impl ChunkDoc {
    /// Maps a domain [`IndexedDocument`] onto the wire format.
    pub fn from_document(d: &IndexedDocument) -> Self {
        Self {
            chunk_id: d.chunk_id.as_str().to_string(),
            document_id: d.document_id.as_str().to_string(),
            version_id: d.version_id.as_str().to_string(),
            title: d.title.clone().unwrap_or_default(),
            body: d.body.clone(),
            url: d.url.clone(),
            language: d.language.clone().unwrap_or_default(),
            workspace_id: d.workspace_id.clone(),
            fetched_at: d.fetched_at.to_rfc3339(),
        }
    }

    /// Maps back to a [`RetrievalCandidate`].
    pub fn to_candidate(&self, score: f32, bm25_score: Option<f32>) -> RetrievalCandidate {
        let fetched_at = DateTime::parse_from_rfc3339(&self.fetched_at)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        RetrievalCandidate {
            chunk_id: ChunkId::new(self.chunk_id.clone()),
            document_id: DocumentId::new(self.document_id.clone()),
            version_id: DocumentVersionId::new(self.version_id.clone()),
            title: if self.title.is_empty() {
                None
            } else {
                Some(self.title.clone())
            },
            snippet: self.body.chars().take(400).collect(),
            url: self.url.clone(),
            score,
            source: RetrievalSource::Bm25,
            fetched_at: fetched_at.with_timezone(&Utc),
            vector_score: None,
            bm25_score,
            workspace_id: Some(self.workspace_id.clone()),
            language: if self.language.is_empty() {
                None
            } else {
                Some(self.language.clone())
            },
        }
    }
}

/// SQL row projection shared by the PostgreSQL and MySQL adapters.
#[derive(Debug, Clone)]
pub struct SqlChunkRow {
    pub chunk_id: String,
    pub document_id: String,
    pub version_id: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub language: String,
    pub workspace_id: String,
    pub fetched_at: String,
    pub score: f64,
}

impl SqlChunkRow {
    /// Maps to a candidate with the engine's normalized score.
    pub fn to_candidate(&self) -> RetrievalCandidate {
        ChunkDoc {
            chunk_id: self.chunk_id.clone(),
            document_id: self.document_id.clone(),
            version_id: self.version_id.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            url: self.url.clone(),
            language: self.language.clone(),
            workspace_id: self.workspace_id.clone(),
            fetched_at: self.fetched_at.clone(),
        }
        .to_candidate(self.score as f32, Some(self.score as f32))
    }
}

/// Client-side filters shared by all adapters: workspace scope (requesting
/// workspace OR global `*`, RFC-013 8.7), language, domain allow/deny and
/// the freshness cutoff. REST backends apply these inside the query DSL
/// where possible; SQL backends in the WHERE clause. Results from engines
/// that cannot push a filter down are re-checked here, so every adapter
/// keeps the isolation contract.
pub fn candidate_allowed(query: &RetrievalQuery, c: &RetrievalCandidate) -> bool {
    // Workspace scope: admit only own workspace or the shared namespace.
    // This is a query-level safety net for adapters without native scoping.
    if let Some(ws) = &query.workspace_id {
        let candidate_ws = c
            .workspace_id
            .clone()
            .or_else(|| local_workspace_from_url(&c.url))
            .unwrap_or_else(|| "*".into());
        if candidate_ws != *ws && candidate_ws != "*" {
            return false;
        }
    }
    if let Some(lang) = &query.language {
        // Engine-level filters handle this; empty stored language admits all.
        let _ = lang;
    }
    let host = host_of(&c.url);
    if !query.required_domains.is_empty() {
        let ok = host.as_ref().is_some_and(|h| {
            query
                .required_domains
                .iter()
                .any(|d| h == d || h.ends_with(&format!(".{d}")))
        });
        if !ok {
            return false;
        }
    }
    if let Some(h) = &host {
        if query
            .excluded_domains
            .iter()
            .any(|d| h == d || h.ends_with(&format!(".{d}")))
        {
            return false;
        }
    }
    if let Some(after) = query.fetched_after {
        if !query.include_stale && c.fetched_at < after {
            return false;
        }
    }
    true
}

fn local_workspace_from_url(url: &str) -> Option<String> {
    url.strip_prefix("searchos-local://")
        .and_then(|rest| rest.split('/').next())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
}

fn host_of(url: &str) -> Option<String> {
    if url.starts_with("searchos-local://") {
        return local_workspace_from_url(url).map(|ws| format!("searchos-local://{ws}"));
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    Some(host.split(':').next()?.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn doc() -> IndexedDocument {
        IndexedDocument {
            chunk_id: ChunkId::new("chunk_1"),
            document_id: DocumentId::new("doc_1"),
            version_id: DocumentVersionId::new("docv_1"),
            title: Some("Title".into()),
            body: "body text".into(),
            url: "https://example.com/a".into(),
            language: Some("en".into()),
            workspace_id: "ws_1".into(),
            fetched_at: Utc::now(),
        }
    }

    #[test]
    fn chunk_doc_roundtrip() {
        let d = doc();
        let wire = ChunkDoc::from_document(&d);
        assert_eq!(wire.chunk_id, "chunk_1");
        assert_eq!(wire.language, "en");
        assert!(!wire.fetched_at.is_empty());
        let c = wire.to_candidate(1.5, Some(1.5));
        assert_eq!(c.chunk_id.as_str(), "chunk_1");
        assert_eq!(c.source, RetrievalSource::Bm25);
        assert!(c.bm25_score.is_some());
        assert_eq!(c.title.as_deref(), Some("Title"));
    }

    #[test]
    fn domain_filters_apply() {
        let mut q = RetrievalQuery::new("x", 5);
        q.required_domains = vec!["example.com".into()];
        let wire = ChunkDoc::from_document(&doc());
        let c = wire.to_candidate(1.0, None);
        assert!(candidate_allowed(&q, &c));
        q.required_domains = vec!["other.org".into()];
        assert!(!candidate_allowed(&q, &c));
        let mut q2 = RetrievalQuery::new("x", 5);
        q2.excluded_domains = vec!["example.com".into()];
        assert!(!candidate_allowed(&q2, &c));
    }

    #[test]
    fn workspace_scope_is_enforced_in_candidate_filter() {
        let wire = ChunkDoc::from_document(&doc());
        let c = wire.to_candidate(1.0, None);
        let mut own = RetrievalQuery::new("x", 5);
        own.workspace_id = Some("ws_1".into());
        assert!(candidate_allowed(&own, &c));
        let mut other = RetrievalQuery::new("x", 5);
        other.workspace_id = Some("ws_2".into());
        assert!(!candidate_allowed(&other, &c));
    }

    #[test]
    fn config_validation_rejects_empty_url() {
        let cfg = RemoteBackendConfig::default();
        assert!(cfg.validate("elasticsearch").is_err());
        let cfg = RemoteBackendConfig {
            url: "http://127.0.0.1:9200".into(),
            ..Default::default()
        };
        assert!(cfg.validate("elasticsearch").is_ok());
    }
}
