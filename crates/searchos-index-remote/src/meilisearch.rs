//! Meilisearch backend (REST API via `reqwest`).
//!
//! - Index created with `chunk_id` as primary key; settings configure
//!   searchable + filterable attributes so workspace/language/freshness
//!   filters push down server-side.
//! - Document writes are enqueued as tasks; SearchOS waits for task
//!   completion to preserve read-your-writes (Meilisearch indexing is
//!   asynchronous).
//! - `rebuild_from` deletes every document and re-adds SQLite payloads.

use async_trait::async_trait;
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, RetrievalQuery};
use searchos_domain::{ChunkId, DocumentId, SearchOsError};
use searchos_retrieval::SearchIndex;
use serde_json::{json, Value};

use crate::shared::{candidate_allowed, ChunkDoc, RemoteBackendConfig};

/// Meilisearch-backed index.
pub struct MeilisearchIndex {
    client: reqwest::Client,
    base_url: String,
    index_name: String,
    api_key: String,
    config: RemoteBackendConfig,
}

impl MeilisearchIndex {
    /// Opens the client, creates the index + settings when missing, and
    /// waits until Meilisearch reports the index available.
    pub async fn open(config: RemoteBackendConfig) -> Result<Self, SearchOsError> {
        config.validate("meilisearch")?;
        let base_url = config.url.trim_end_matches('/').to_string();
        let api_key = config.effective_api_key("SEARCHOS_MEILI_API_KEY");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                config.timeout_seconds.max(1),
            ))
            .build()
            .map_err(|e| SearchOsError::Storage(format!("meilisearch client: {e}")))?;
        let index_name = if config.index.trim().is_empty() {
            "searchos_chunks".to_string()
        } else {
            config.index.clone()
        };
        let idx = Self {
            client,
            base_url,
            index_name,
            api_key,
            config,
        };
        idx.ensure_index().await?;
        Ok(idx)
    }

    fn auth_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        req
    }

    async fn ensure_index(&self) -> Result<(), SearchOsError> {
        let url = format!("{}/indexes/{}", self.base_url, self.index_name);
        let resp = self
            .auth_headers(self.client.get(&url))
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("meilisearch get index: {e}")))?;
        if !resp.status().is_success() {
            // 404 → create with the primary key, then push settings.
            let create_url = format!("{}/indexes", self.base_url);
            let resp = self
                .auth_headers(self.client.post(&create_url))
                .json(&json!({
                    "uid": self.index_name,
                    "primaryKey": "chunk_id"
                }))
                .send()
                .await
                .map_err(|e| SearchOsError::Storage(format!("meilisearch create index: {e}")))?;
            if !resp.status().is_success() {
                return Err(SearchOsError::Storage(format!(
                    "meilisearch create index HTTP {}",
                    resp.status()
                )));
            }
            self.wait_task().await?;
            self.apply_settings().await?;
        }
        Ok(())
    }

    async fn apply_settings(&self) -> Result<(), SearchOsError> {
        let url = format!("{}/indexes/{}/settings", self.base_url, self.index_name);
        let resp = self
            .auth_headers(self.client.patch(&url))
            .json(&json!({
                "searchableAttributes": ["title", "body"],
                "filterableAttributes": ["workspace_id", "language", "document_id", "fetched_at"]
            }))
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("meilisearch settings: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchOsError::Storage(format!(
                "meilisearch settings HTTP {}",
                resp.status()
            )));
        }
        self.wait_task().await
    }

    /// Waits for all currently enqueued/processing tasks to settle so a
    /// write issued just before becomes visible (Meilisearch is async).
    async fn wait_task(&self) -> Result<(), SearchOsError> {
        let url = format!("{}/tasks", self.base_url);
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(self.config.timeout_seconds.max(1));
        loop {
            let resp = self
                .auth_headers(
                    self.client
                        .get(&url)
                        .query(&[("statuses", "enqueued,processing")])
                        .query(&[("limit", 1)]),
                )
                .send()
                .await
                .map_err(|e| SearchOsError::Storage(format!("meilisearch tasks: {e}")))?;
            let value: Value = resp
                .json()
                .await
                .map_err(|e| SearchOsError::Storage(format!("meilisearch tasks decode: {e}")))?;
            let pending = value.get("total").and_then(|t| t.as_u64()).unwrap_or(0);
            if pending == 0 {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(SearchOsError::Timeout(
                    "meilisearch task queue did not drain in time".into(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Meilisearch filter expression for workspace + language + freshness.
    fn build_filter(query: &RetrievalQuery) -> Option<String> {
        let mut clauses: Vec<String> = Vec::new();
        if let Some(ws) = &query.workspace_id {
            clauses.push(format!("(workspace_id = \"{ws}\" OR workspace_id = \"*\")"));
        }
        if let Some(lang) = &query.language {
            clauses.push(format!("language = \"{lang}\""));
        }
        if let Some(after) = query.fetched_after {
            if !query.include_stale {
                clauses.push(format!("fetched_at >= \"{}\"", after.to_rfc3339()));
            }
        }
        if clauses.is_empty() {
            None
        } else {
            Some(clauses.join(" AND "))
        }
    }

    /// Deletes a batch of chunk ids and waits for Meilisearch's async task.
    async fn delete_chunk_ids(&self, chunk_ids: &[String]) -> Result<(), SearchOsError> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        let url = format!(
            "{}/indexes/{}/documents/delete-batch",
            self.base_url, self.index_name
        );
        let resp = self
            .auth_headers(self.client.post(&url))
            .json(chunk_ids)
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("meilisearch delete: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchOsError::Storage(format!(
                "meilisearch delete HTTP {}",
                resp.status()
            )));
        }
        self.wait_task().await
    }
}

#[async_trait]
impl SearchIndex for MeilisearchIndex {
    async fn index(&self, document: IndexedDocument) -> Result<(), SearchOsError> {
        let wire = ChunkDoc::from_document(&document);
        let url = format!(
            "{}/indexes/{}/documents?primaryKey=chunk_id",
            self.base_url, self.index_name
        );
        let resp = self
            .auth_headers(self.client.post(&url))
            .json(&vec![serde_json::to_value(&wire)?])
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("meilisearch add: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchOsError::Storage(format!(
                "meilisearch add HTTP {}",
                resp.status()
            )));
        }
        self.wait_task().await
    }

    async fn search(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        let url = format!("{}/indexes/{}/search", self.base_url, self.index_name);
        let mut body = json!({
            "q": query.text,
            "limit": query.top_k.max(1)
        });
        if let Some(filter) = Self::build_filter(query) {
            body["filter"] = Value::String(filter);
        }
        let resp = self
            .auth_headers(self.client.post(&url))
            .json(&body)
            .send()
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("meilisearch search: {e}")))?;
        let status = resp.status();
        let value: Value = resp
            .json()
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("meilisearch search decode: {e}")))?;
        if !status.is_success() {
            return Err(SearchOsError::Retrieval(format!(
                "meilisearch search HTTP {status}"
            )));
        }
        let hits = value
            .get("hits")
            .and_then(|h| h.as_array())
            .ok_or_else(|| {
                SearchOsError::Retrieval("meilisearch search: malformed response".into())
            })?;
        let mut out = Vec::with_capacity(hits.len());
        for hit in hits {
            if let Ok(wire) = serde_json::from_value::<ChunkDoc>(hit.clone()) {
                // Meilisearch has no per-hit BM25 score exposed by default;
                // rank position is used as the score (1.0 for the top hit).
                let rank = out.len() as f32;
                let score = (query.top_k as f32 - rank).max(0.0);
                let candidate = wire.to_candidate(score, None);
                if candidate_allowed(query, &candidate) {
                    out.push(candidate);
                }
            }
        }
        out.truncate(query.top_k);
        Ok(out)
    }

    async fn delete(&self, document_id: &DocumentId) -> Result<(), SearchOsError> {
        // Meilisearch deletes by document id, so resolve chunk ids first.
        let search_url = format!("{}/indexes/{}/search", self.base_url, self.index_name);
        let resp = self
            .auth_headers(self.client.post(&search_url))
            .json(&json!({
                "q": "",
                "filter": format!("document_id = \"{}\"", document_id.as_str()),
                "limit": 10_000
            }))
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("meilisearch delete lookup: {e}")))?;
        let value: Value = resp.json().await.map_err(|e| {
            SearchOsError::Storage(format!("meilisearch delete lookup decode: {e}"))
        })?;
        let chunk_ids: Vec<String> = value
            .get("hits")
            .and_then(|h| h.as_array())
            .map(|hits| {
                hits.iter()
                    .filter_map(|h| {
                        h.get("chunk_id")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.delete_chunk_ids(&chunk_ids).await
    }

    async fn delete_by_chunk_ids(&self, chunk_ids: &[ChunkId]) -> Result<(), SearchOsError> {
        let ids: Vec<String> = chunk_ids.iter().map(|c| c.as_str().to_string()).collect();
        self.delete_chunk_ids(&ids).await
    }

    async fn count(&self) -> Result<u64, SearchOsError> {
        let url = format!("{}/indexes/{}/stats", self.base_url, self.index_name);
        let resp = self
            .auth_headers(self.client.get(&url))
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("meilisearch stats: {e}")))?;
        let value: Value = resp
            .json()
            .await
            .map_err(|e| SearchOsError::Storage(format!("meilisearch stats decode: {e}")))?;
        Ok(value
            .get("numberOfDocuments")
            .and_then(|n| n.as_u64())
            .unwrap_or(0))
    }

    async fn rebuild_from(&self, documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        let url = format!("{}/indexes/{}/documents", self.base_url, self.index_name);
        let resp = self
            .auth_headers(self.client.delete(&url))
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("meilisearch clear: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchOsError::Storage(format!(
                "meilisearch clear HTTP {}",
                resp.status()
            )));
        }
        self.wait_task().await?;
        for d in documents {
            self.index(d.clone()).await?;
        }
        Ok(documents.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_scopes_workspace_and_language() {
        let mut q = RetrievalQuery::new("x", 5);
        q.workspace_id = Some("ws_1".into());
        q.language = Some("en".into());
        let f = MeilisearchIndex::build_filter(&q).unwrap();
        assert!(f.contains("ws_1"));
        assert!(f.contains("language = \"en\""));
        assert!(f.contains('*'));
    }

    #[test]
    fn no_filter_when_unscoped() {
        let q = RetrievalQuery::new("x", 5);
        assert!(MeilisearchIndex::build_filter(&q).is_none());
    }
}
