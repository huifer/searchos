//! Reranker adapters (RFC-007 7.7).
//!
//! The reranker is the last ranking stage before the quality gate. Two
//! implementations ship:
//!
//! - [`FeatureReranker`] — deterministic, offline, dependency-free. Blends
//!   the retrieval score with query-term coverage in title/body, exact-phrase
//!   and freshness signals. This is the wired default so reranking works
//!   without any network dependency.
//! - [`RemoteReranker`] — an OpenAI/Cohere/Jina-compatible cross-encoder
//!   endpoint for higher-precision profiles. The API key is read from the
//!   environment, never from config files.
//!
//! Both keep `RetrievalSource::Reranked` on output and are pure: they do not
//! add or remove candidates, they only reorder them.

use super::rerank::{Reranker, RerankerPolicy};
use async_trait::async_trait;
use searchos_domain::retrieval::{RetrievalCandidate, RetrievalQuery, RetrievalSource};
use searchos_domain::SearchOsError;
use std::sync::Arc;

/// Lowercases and splits a query into comparable terms (CJK runs are kept
/// whole so the same bigram-ish text matches a title/body substring).
fn query_terms(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for token in text.split(|c: char| !c.is_alphanumeric()) {
        let t = token.to_lowercase();
        if !t.is_empty() && !terms.contains(&t) {
            terms.push(t);
        }
    }
    terms
}

/// Offline feature-based reranker. Weights are deliberately small and fixed:
/// it refines the fused order rather than replacing it, so a high-confidence
/// BM25/vector winner is never buried by a lexical tie.
#[derive(Debug, Clone)]
pub struct FeatureReranker {
    /// Weight of query terms found in the title.
    pub title_weight: f32,
    /// Weight of query terms found in the body/snippet.
    pub body_weight: f32,
    /// Bonus when the full normalized query appears in title or body.
    pub phrase_weight: f32,
    /// Mild recency preference (0 disables); capped so it cannot dominate.
    pub recency_weight: f32,
}

impl Default for FeatureReranker {
    fn default() -> Self {
        Self {
            title_weight: 0.35,
            body_weight: 0.15,
            phrase_weight: 0.20,
            recency_weight: 0.05,
        }
    }
}

#[async_trait]
impl Reranker for FeatureReranker {
    fn model_id(&self) -> &str {
        "feature-reranker-v1"
    }

    async fn rerank(
        &self,
        query: &RetrievalQuery,
        candidates: Vec<RetrievalCandidate>,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        let terms = query_terms(&query.text);
        let phrase = query.text.to_lowercase();
        let now = chrono::Utc::now();
        let mut scored: Vec<(f32, usize, RetrievalCandidate)> = candidates
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                let title = c.title.as_deref().unwrap_or("").to_lowercase();
                let body = c.snippet.to_lowercase();
                let mut bonus = 0.0f32;
                if !terms.is_empty() {
                    let t_hits = terms.iter().filter(|t| title.contains(t.as_str())).count();
                    let b_hits = terms.iter().filter(|t| body.contains(t.as_str())).count();
                    bonus += self.title_weight * (t_hits as f32 / terms.len() as f32);
                    bonus += self.body_weight * (b_hits as f32 / terms.len() as f32);
                }
                if phrase.len() >= 3 && (title.contains(&phrase) || body.contains(&phrase)) {
                    bonus += self.phrase_weight;
                }
                if self.recency_weight > 0.0 {
                    let age_days = (now - c.fetched_at).num_days().max(0) as f32;
                    bonus += self.recency_weight / (1.0 + age_days / 30.0);
                }
                // `score` stays the fused RRF score — the reranker ONLY
                // reorders, so a downstream quality gate sees the same
                // measurable relevance scale (no silent rescale).
                let rank_key = c.score + bonus;
                c.source = RetrievalSource::Reranked;
                (rank_key, i, c)
            })
            .collect();
        // Stable tie-break by original rank.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });
        Ok(scored.into_iter().map(|(_, _, c)| c).collect())
    }
}

/// Configuration for a cross-encoder rerank endpoint.
#[derive(Debug, Clone)]
pub struct RemoteRerankerConfig {
    /// Full endpoint, e.g. `https://api.jina.ai/v1/rerank`.
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    pub timeout_seconds: u64,
    /// Maximum candidates sent per request.
    pub top_n: usize,
    /// RFC-087 per-source egress policy: whether WORKSPACE-OWNED content
    /// (the `searchos-*://` schemes: ingested documents, memory, session
    /// transcripts) may be sent to the remote endpoint. Fail-closed: the
    /// default never sends stored knowledge to an external service; public
    /// web content is unaffected. An operator opt-in re-enables remote
    /// reranking for workspace content.
    pub allow_workspace_content: bool,
}

impl Default for RemoteRerankerConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            model: "cross-encoder".into(),
            api_key_env: "SEARCHOS_RERANKER_API_KEY".into(),
            timeout_seconds: 15,
            top_n: 30,
            allow_workspace_content: false,
        }
    }
}

/// Cross-encoder reranker. Wire contract:
/// `{model, query, documents[], top_n}` → `{results[{index, relevance_score}]}`.
/// Any failure is returned as an error so the caller can keep the fused order
/// (never silently degrade to an unranked list).
/// Whether a candidate URL carries WORKSPACE-OWNED content (RFC-087).
///
/// All local canonical schemes share the `searchos-` prefix: ingested
/// documents (`searchos-local://`), memory entries (`searchos-memory://`)
/// and session transcripts (`searchos-session://`). Web/provider content is
/// public by origin and unaffected.
pub fn is_workspace_owned(url: &str) -> bool {
    url.starts_with("searchos-")
}

pub struct RemoteReranker {
    client: reqwest::Client,
    config: RemoteRerankerConfig,
}

impl RemoteReranker {
    pub fn new(config: RemoteRerankerConfig) -> Result<Self, SearchOsError> {
        if config.endpoint.trim().is_empty() || config.top_n == 0 {
            return Err(SearchOsError::Internal(
                "reranker endpoint/top_n must be set".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                config.timeout_seconds.max(1),
            ))
            .build()
            .map_err(|e| SearchOsError::Internal(format!("http client: {e}")))?;
        Ok(Self { client, config })
    }
}

#[async_trait]
impl Reranker for RemoteReranker {
    fn model_id(&self) -> &str {
        &self.config.model
    }

    async fn rerank(
        &self,
        query: &RetrievalQuery,
        candidates: Vec<RetrievalCandidate>,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        if candidates.is_empty() {
            return Ok(candidates);
        }
        // RFC-087 per-source egress policy (fail-closed): workspace-owned
        // content never reaches the remote endpoint unless the operator
        // opted in. Reranking is an optimisation — skipping it preserves the
        // fused order and the privacy guarantee together.
        if !self.config.allow_workspace_content
            && candidates[..candidates.len().min(self.config.top_n)]
                .iter()
                .any(|c| is_workspace_owned(&c.url))
        {
            searchos_observability::MetricsRegistry::global()
                .inc_counter("privacy.egress_blocked_total");
            tracing::debug!(
                "remote rerank skipped: workspace-owned content may not leave (RFC-087)"
            );
            return Ok(candidates);
        }
        let take = candidates.len().min(self.config.top_n);
        let documents: Vec<String> = candidates[..take]
            .iter()
            .map(|c| match &c.title {
                Some(t) => format!("{t}\n{}", c.snippet),
                None => c.snippet.clone(),
            })
            .collect();
        let mut req = self
            .client
            .post(&self.config.endpoint)
            .json(&serde_json::json!({
                "model": self.config.model,
                "query": query.text,
                "documents": documents,
                "top_n": take,
            }));
        if !self.config.api_key_env.is_empty() {
            if let Ok(key) = std::env::var(&self.config.api_key_env) {
                if !key.trim().is_empty() {
                    req = req.header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"));
                }
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("rerank request: {e}")))?;
        let status = resp.status();
        let payload: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("rerank response: {e}")))?;
        if !status.is_success() {
            return Err(SearchOsError::Retrieval(format!(
                "rerank endpoint returned HTTP {status}"
            )));
        }
        let results = payload
            .get("results")
            .and_then(|v| v.as_array())
            .ok_or_else(|| SearchOsError::Retrieval("missing results[]".into()))?;
        let mut order: Vec<(f32, usize)> = Vec::new();
        for item in results {
            let idx = item
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or(u64::MAX) as usize;
            let score = item
                .get("relevance_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            if idx < take {
                order.push((score, idx));
            }
        }
        order.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut out: Vec<RetrievalCandidate> = Vec::with_capacity(candidates.len());
        let mut seen = std::collections::HashSet::new();
        for (score, idx) in order {
            if !seen.insert(idx) {
                continue;
            }
            let mut c = candidates[idx].clone();
            // Keep the fused score: the cross-encoder score is the ORDERING
            // key only (`score` local variable above), not a new relevance.
            let _ = score;
            c.source = RetrievalSource::Reranked;
            out.push(c);
        }
        // Missing indices keep their fused order (never drop a candidate).
        for (i, c) in candidates.into_iter().enumerate() {
            if !seen.contains(&i) {
                out.push(c);
            }
        }
        Ok(out)
    }
}

/// Builds the runtime reranker from config. `local` is always available;
/// `remote` falls back to local if the endpoint is not configured so a
/// partially configured deployment still ranks deterministically.
pub fn build_reranker(provider: &str, config: &RemoteRerankerConfig) -> Arc<dyn Reranker> {
    if provider == "remote" {
        if let Ok(r) = RemoteReranker::new(config.clone()) {
            return Arc::new(r);
        }
        tracing::warn!("remote reranker not configured; using the local feature reranker");
    }
    Arc::new(FeatureReranker::default())
}

/// Default trigger policy: rerank when the fused set is large or the two
/// retrieval legs disagree (RFC-007 7.7).
pub fn default_policy() -> RerankerPolicy {
    RerankerPolicy::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};

    fn cand(chunk: &str, title: &str, body: &str, score: f32) -> RetrievalCandidate {
        RetrievalCandidate {
            chunk_id: ChunkId::new(chunk),
            document_id: DocumentId::new("doc"),
            version_id: DocumentVersionId::new("docv"),
            title: Some(title.into()),
            snippet: body.into(),
            url: "https://example.com".into(),
            score,
            source: RetrievalSource::Hybrid,
            fetched_at: Utc::now(),
            vector_score: None,
            bm25_score: None,
            workspace_id: None,
            language: None,
            highlight: None,
        }
    }

    #[tokio::test]
    async fn title_match_outranks_body_only_match() {
        let r = FeatureReranker::default();
        let q = RetrievalQuery::new("tokio runtime", 5);
        let out = r
            .rerank(
                &q,
                vec![
                    cand("body", "unrelated", "tokio runtime appears in body", 1.0),
                    cand("title", "tokio runtime", "unrelated body", 1.0),
                ],
            )
            .await
            .unwrap();
        assert_eq!(out[0].chunk_id.as_str(), "title");
        assert!(out.iter().all(|c| c.source == RetrievalSource::Reranked));
    }

    #[tokio::test]
    async fn rerank_preserves_candidate_set() {
        let r = FeatureReranker::default();
        let q = RetrievalQuery::new("anything", 5);
        let out = r
            .rerank(&q, vec![cand("a", "a", "a", 0.1), cand("b", "b", "b", 0.9)])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].chunk_id.as_str(), "b", "fused winner stays ahead");
    }

    #[test]
    fn local_is_the_default_reranker() {
        assert_eq!(
            build_reranker("local", &RemoteRerankerConfig::default()).model_id(),
            "feature-reranker-v1"
        );
        // A remote provider without an endpoint degrades to local, never to NeverReranker.
        assert_eq!(
            build_reranker("remote", &RemoteRerankerConfig::default()).model_id(),
            "feature-reranker-v1"
        );
    }
}

#[cfg(test)]
mod egress_tests {
    use super::*;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};
    use searchos_domain::retrieval::RetrievalSource;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn candidate(url: &str) -> RetrievalCandidate {
        RetrievalCandidate {
            chunk_id: ChunkId::new(url),
            document_id: DocumentId::new("doc"),
            version_id: DocumentVersionId::new("docv"),
            title: Some("title".into()),
            snippet: "snippet body".into(),
            url: url.into(),
            score: 1.0,
            source: RetrievalSource::Hybrid,
            fetched_at: chrono::Utc::now(),
            vector_score: None,
            bm25_score: None,
            workspace_id: None,
            language: None,
            highlight: None,
        }
    }

    fn config(endpoint: String, allow_workspace_content: bool) -> RemoteRerankerConfig {
        RemoteRerankerConfig {
            endpoint,
            model: "test-cross-encoder".into(),
            api_key_env: String::new(),
            timeout_seconds: 2,
            top_n: 8,
            allow_workspace_content,
        }
    }

    /// Minimal HTTP/1.1 rerank endpoint that records every received
    /// document body and returns a pass-through ranking.
    async fn mock_rerank_endpoint(
        listener: tokio::net::TcpListener,
        hits: Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let hits = Arc::clone(&hits);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 32768];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = req.split("\r\n\r\n").nth(1).unwrap_or_default();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                    if let Some(docs) = v.get("documents").and_then(|d| d.as_array()) {
                        for d in docs {
                            if let Some(text) = d.as_str() {
                                hits.lock().unwrap().push(text.to_string());
                            }
                        }
                    }
                }
                let payload = serde_json::json!({
                    "results": [{ "index": 0, "relevance_score": 0.9 }]
                });
                let http = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    payload.to_string().len(),
                    payload
                );
                let _ = socket.write_all(http.as_bytes()).await;
            });
        }
    }

    fn endpoint_of(listener: &tokio::net::TcpListener) -> String {
        format!("http://{}/rerank", listener.local_addr().unwrap())
    }

    #[tokio::test]
    async fn workspace_content_never_reaches_the_remote_endpoint_by_default() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = endpoint_of(&listener);
        let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
        tokio::spawn(mock_rerank_endpoint(listener, Arc::clone(&hits)));

        let reranker = RemoteReranker::new(config(endpoint, false)).unwrap();
        let query = RetrievalQuery::new("q", 5);
        let out = reranker
            .rerank(
                &query,
                vec![candidate("searchos-local://default/notes/private")],
            )
            .await
            .unwrap();
        // Candidates are returned UNCHANGED (no rerank, no reorder).
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "searchos-local://default/notes/private");
        // Give the mock a beat, then assert NOTHING was sent.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            hits.lock().unwrap().is_empty(),
            "workspace content must not egress by default"
        );
        assert!(
            searchos_observability::MetricsRegistry::global()
                .counter("privacy.egress_blocked_total")
                > 0,
            "the block is observable"
        );
    }

    #[tokio::test]
    async fn operator_opt_in_allows_workspace_content_to_egress() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = endpoint_of(&listener);
        let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
        tokio::spawn(mock_rerank_endpoint(listener, Arc::clone(&hits)));

        let reranker = RemoteReranker::new(config(endpoint, true)).unwrap();
        let query = RetrievalQuery::new("q", 5);
        let out = reranker
            .rerank(
                &query,
                vec![candidate("searchos-local://default/notes/private")],
            )
            .await
            .unwrap();
        // Pass-through ranking: index 0 → the first candidate is kept.
        assert_eq!(out.len(), 1);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !hits.lock().unwrap().is_empty(),
            "an explicit opt-in sends the content"
        );
    }

    #[tokio::test]
    async fn public_web_content_egresses_without_opt_in() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = endpoint_of(&listener);
        let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
        tokio::spawn(mock_rerank_endpoint(listener, Arc::clone(&hits)));

        let reranker = RemoteReranker::new(config(endpoint, false)).unwrap();
        let query = RetrievalQuery::new("q", 5);
        let out = reranker
            .rerank(&query, vec![candidate("https://public.dev/article")])
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !hits.lock().unwrap().is_empty(),
            "public web content is not gated"
        );
    }

    #[test]
    fn workspace_owned_detection_covers_every_local_scheme() {
        assert!(is_workspace_owned("searchos-local://default/notes/a"));
        assert!(is_workspace_owned("searchos-memory://mem_1"));
        assert!(is_workspace_owned("searchos-session://s1/0"));
        assert!(!is_workspace_owned("https://public.dev/article"));
    }
}
