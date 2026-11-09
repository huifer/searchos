//! Elasticsearch backend (plain REST via `reqwest`).
//!
//! Kept dependency-light on purpose: the official client crate only has
//! alpha releases, and the SearchOS provider adapters already speak REST
//! with `reqwest` + rustls.
//!
//! - Index created on open with a fixed mapping (`chunk_id` keyword id).
//! - Writes use `refresh=wait_for` so outbox-driven indexing keeps the
//!   read-your-writes guarantee of the local adapters.
//! - Queries are `multi_match` BM25 scoped by workspace (`requester OR *`),
//!   language, and fetched-after cutoff.
//! - `rebuild_from` deletes all documents and re-bulks from SQLite payloads.

use async_trait::async_trait;
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, RetrievalQuery};
use searchos_domain::{ChunkId, DocumentId, SearchOsError};
use searchos_retrieval::SearchIndex;
use serde_json::{json, Value};

use crate::shared::{candidate_allowed, ChunkDoc, RemoteBackendConfig};

/// Elasticsearch-backed BM25 index (REST).
pub struct ElasticsearchIndex {
    client: reqwest::Client,
    base_url: String,
    index_name: String,
    api_key: String,
}

impl ElasticsearchIndex {
    /// Opens the client, creates the index when missing.
    pub async fn open(config: RemoteBackendConfig) -> Result<Self, SearchOsError> {
        config.validate("elasticsearch")?;
        let base_url = config.url.trim_end_matches('/').to_string();
        let api_key = config.effective_api_key("SEARCHOS_ES_API_KEY");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                config.timeout_seconds.max(1),
            ))
            .build()
            .map_err(|e| SearchOsError::Storage(format!("elasticsearch client: {e}")))?;
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
        };
        idx.ensure_index().await?;
        Ok(idx)
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if !self.api_key.is_empty() {
            req.bearer_auth(&self.api_key)
        } else {
            req
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    async fn ensure_index(&self) -> Result<(), SearchOsError> {
        let exists = self
            .client
            .head(self.url(&self.index_name))
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("elasticsearch head index: {e}")))?;
        if exists.status().is_success() {
            return Ok(());
        }
        let body = json!({
            "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
            "mappings": {
                "properties": {
                    "chunk_id": { "type": "keyword" },
                    "document_id": { "type": "keyword" },
                    "version_id": { "type": "keyword" },
                    "title": { "type": "text" },
                    "body": { "type": "text" },
                    "url": { "type": "keyword" },
                    "language": { "type": "keyword" },
                    "workspace_id": { "type": "keyword" },
                    "fetched_at": { "type": "date" }
                }
            }
        });
        let resp = self
            .auth(self.client.put(self.url(&self.index_name)))
            .json(&body)
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("elasticsearch create index: {e}")))?;
        // 400 with resource_already_exists_exception is fine (race with
        // another writer); anything else is fatal.
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success()
            && !(status == reqwest::StatusCode::BAD_REQUEST
                && text.contains("resource_already_exists_exception"))
        {
            return Err(SearchOsError::Storage(format!(
                "elasticsearch create index HTTP {status}: {text}"
            )));
        }
        Ok(())
    }

    fn build_search_body(query: &RetrievalQuery) -> Value {
        let must = vec![json!({
            "multi_match": {
                "query": query.text,
                "fields": ["title^2", "body"],
                "type": "best_fields"
            }
        })];
        let mut filter = Vec::new();
        if let Some(ws) = &query.workspace_id {
            filter.push(json!({
                "bool": {
                    "should": [
                        { "term": { "workspace_id": ws } },
                        { "term": { "workspace_id": "*" } }
                    ],
                    "minimum_should_match": 1
                }
            }));
        }
        if let Some(lang) = &query.language {
            filter.push(json!({ "term": { "language": lang } }));
        }
        if let Some(after) = query.fetched_after {
            if !query.include_stale {
                filter.push(json!({
                    "range": { "fetched_at": { "gte": after.to_rfc3339() } }
                }));
            }
        }
        let mut bool_query = serde_json::Map::new();
        bool_query.insert("must".into(), Value::Array(must));
        if !filter.is_empty() {
            bool_query.insert("filter".into(), Value::Array(filter));
        }
        json!({
            "size": query.top_k.max(1),
            "query": { "bool": Value::Object(bool_query) }
        })
    }
}

#[async_trait]
impl SearchIndex for ElasticsearchIndex {
    async fn index(&self, document: IndexedDocument) -> Result<(), SearchOsError> {
        let wire = ChunkDoc::from_document(&document);
        let resp = self
            .auth(self.client.put(self.url(&format!(
                "{}/_doc/{}?refresh=wait_for",
                self.index_name, wire.chunk_id
            ))))
            .json(&serde_json::to_value(&wire)?)
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("elasticsearch index: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SearchOsError::Storage(format!(
                "elasticsearch index HTTP {status}: {text}"
            )));
        }
        Ok(())
    }

    async fn search(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        let body = Self::build_search_body(query);
        let resp = self
            .auth(
                self.client
                    .post(self.url(&format!("{}/_search", self.index_name))),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("elasticsearch search: {e}")))?;
        let status = resp.status();
        let value: Value = resp
            .json()
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("elasticsearch search decode: {e}")))?;
        if !status.is_success() {
            return Err(SearchOsError::Retrieval(format!(
                "elasticsearch search HTTP {status}"
            )));
        }
        let hits = value
            .get("hits")
            .and_then(|h| h.get("hits"))
            .and_then(|h| h.as_array())
            .ok_or_else(|| {
                SearchOsError::Retrieval("elasticsearch search: malformed response".into())
            })?;
        let mut out = Vec::with_capacity(hits.len());
        for hit in hits {
            let source = hit.get("_source").cloned().unwrap_or(Value::Null);
            let score = hit.get("_score").and_then(|s| s.as_f64()).unwrap_or(0.0) as f32;
            if let Ok(wire) = serde_json::from_value::<ChunkDoc>(source) {
                let candidate = wire.to_candidate(score, Some(score));
                if candidate_allowed(query, &candidate) {
                    out.push(candidate);
                }
            }
        }
        out.truncate(query.top_k);
        Ok(out)
    }

    async fn delete(&self, document_id: &DocumentId) -> Result<(), SearchOsError> {
        let resp = self
            .auth(self.client.post(self.url(&format!(
                "{}/_delete_by_query?refresh=true",
                self.index_name
            ))))
            .json(&json!({
                "query": { "term": { "document_id": document_id.as_str() } }
            }))
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("elasticsearch delete: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SearchOsError::Storage(format!(
                "elasticsearch delete HTTP {status}: {text}"
            )));
        }
        Ok(())
    }

    async fn delete_by_chunk_ids(&self, chunk_ids: &[ChunkId]) -> Result<(), SearchOsError> {
        let ids: Vec<&str> = chunk_ids.iter().map(|c| c.as_str()).collect();
        let resp = self
            .auth(self.client.post(self.url(&format!(
                "{}/_delete_by_query?refresh=true",
                self.index_name
            ))))
            .json(&json!({
                "query": { "terms": { "chunk_id": ids } }
            }))
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("elasticsearch chunk delete: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(SearchOsError::Storage(format!(
                "elasticsearch chunk delete HTTP {status}"
            )));
        }
        Ok(())
    }

    async fn count(&self) -> Result<u64, SearchOsError> {
        let resp = self
            .auth(
                self.client
                    .get(self.url(&format!("{}/_count", self.index_name))),
            )
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("elasticsearch count: {e}")))?;
        let value: Value = resp
            .json()
            .await
            .map_err(|e| SearchOsError::Storage(format!("elasticsearch count decode: {e}")))?;
        Ok(value.get("count").and_then(|c| c.as_u64()).unwrap_or(0))
    }

    async fn rebuild_from(&self, documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        // Clear via match_all delete, then bulk re-add in batches of 512.
        let resp = self
            .auth(self.client.post(self.url(&format!(
                "{}/_delete_by_query?refresh=true",
                self.index_name
            ))))
            .json(&json!({ "query": { "match_all": {} } }))
            .send()
            .await
            .map_err(|e| SearchOsError::Storage(format!("elasticsearch clear: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(SearchOsError::Storage(format!(
                "elasticsearch clear HTTP {status}"
            )));
        }
        for batch in documents.chunks(512) {
            let mut ndjson = String::new();
            for d in batch {
                let wire = ChunkDoc::from_document(d);
                ndjson.push_str(
                    &json!({ "index": { "_index": self.index_name, "_id": wire.chunk_id } })
                        .to_string(),
                );
                ndjson.push('\n');
                ndjson.push_str(&serde_json::to_string(&wire)?);
                ndjson.push('\n');
            }
            let resp = self
                .auth(
                    self.client
                        .post(self.url("_bulk?refresh=wait_for"))
                        .header("Content-Type", "application/x-ndjson")
                        .body(ndjson),
                )
                .send()
                .await
                .map_err(|e| SearchOsError::Storage(format!("elasticsearch bulk: {e}")))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(SearchOsError::Storage(format!(
                    "elasticsearch bulk HTTP {status}: {text}"
                )));
            }
        }
        Ok(documents.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::ids::{ChunkId, DocumentVersionId};

    fn query() -> RetrievalQuery {
        RetrievalQuery::new("tokio runtime", 5)
    }

    #[test]
    fn search_body_scopes_workspace() {
        let mut q = query();
        q.workspace_id = Some("ws_1".into());
        let body = ElasticsearchIndex::build_search_body(&q);
        let filters = body["query"]["bool"]["filter"].as_array().unwrap();
        assert!(filters.iter().any(|f| f.to_string().contains("ws_1")));
        assert!(filters.iter().any(|f| f.to_string().contains('*')));
    }

    #[test]
    fn search_body_without_workspace_has_no_workspace_filter() {
        let q = query();
        let body = ElasticsearchIndex::build_search_body(&q);
        assert!(body["query"]["bool"].get("filter").is_none());
    }

    #[test]
    fn doc_roundtrip_through_json() {
        let doc = IndexedDocument {
            chunk_id: ChunkId::new("c1"),
            document_id: searchos_domain::DocumentId::new("d1"),
            version_id: DocumentVersionId::new("v1"),
            title: Some("t".into()),
            body: "b".into(),
            url: "https://a.b/c".into(),
            language: Some("en".into()),
            workspace_id: "ws".into(),
            fetched_at: Utc::now(),
        };
        let wire = ChunkDoc::from_document(&doc);
        let value = serde_json::to_value(&wire).unwrap();
        let back: ChunkDoc = serde_json::from_value(value).unwrap();
        assert_eq!(back.chunk_id, "c1");
        let c = back.to_candidate(0.5, Some(0.5));
        assert_eq!(c.chunk_id.as_str(), "c1");
    }
}
