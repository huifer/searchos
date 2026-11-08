//! Tavily adapter (RFC-009 9.5 MVP provider 1).
//!
//! Maps SearchOS internal IR ⇄ Tavily API. Tavily DTOs never cross into
//! the core (RFC-012 12.7) — this module is the only place that knows the
//! Tavily wire format. API key is injected, never logged (RFC-014 14.3).

use async_trait::async_trait;
use chrono::DateTime;
use searchos_domain::provider::ProviderCapabilities;
use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::error::ProviderError;
use crate::traits::{
    ExtractDepth, FetchProvider, FetchRequest, FetchResponse, ProviderResult, SearchParams,
    SearchProvider, SearchResultEntry,
};

/// Tavily client.
pub struct TavilyProvider {
    api_key: String,
    http: reqwest::Client,
    base_url: String,
}

impl TavilyProvider {
    /// Client with API key and a shared HTTP client.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("searchos/0.1 (+https://github.com/searchos/searchos)")
                .build()
                .expect("reqwest client"),
            base_url: "https://api.tavily.com".into(),
        }
    }

    /// Overrides the base URL (tests / gateways).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    fn auth_configured(&self) -> bool {
        !self.api_key.trim().is_empty()
    }
}

#[derive(Debug, Serialize)]
struct TavilySearchRequest {
    api_key: String,
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_depth: Option<&'static str>,
    max_results: u32,
    include_raw_content: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    include_domains: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    exclude_domains: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TavilySearchResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    title: Option<String>,
    url: String,
    content: Option<String>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    published_date: Option<String>,
    #[serde(default)]
    raw_content: Option<String>,
}

#[async_trait]
impl SearchProvider for TavilyProvider {
    fn id(&self) -> &str {
        "tavily"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            search: true,
            extract: true,
            crawl: false,
            map: false,
            semantic_search: true,
            supports_domain_filter: true,
            supports_date_filter: false,
        }
    }

    async fn search(&self, params: &SearchParams) -> Result<ProviderResult, ProviderError> {
        if !self.auth_configured() {
            return Err(ProviderError::auth(
                "tavily",
                "missing API key (set TAVILY_API_KEY)",
            ));
        }
        let body = TavilySearchRequest {
            api_key: self.api_key.clone(),
            query: params.query.clone(),
            search_depth: if params.include_raw_content {
                Some("advanced")
            } else {
                Some("basic")
            },
            max_results: params.max_results.clamp(1, 20),
            include_raw_content: params.include_raw_content,
            include_domains: params.required_domains.clone(),
            exclude_domains: params.excluded_domains.clone(),
        };
        let started = Instant::now();
        let url = format!("{}/search", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network("tavily", e.to_string()))?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::network("tavily", e.to_string()))?;
        let bytes = text.len() as u64;
        if status != 200 {
            let mut err = ProviderError::from_status("tavily", status, &text);
            if let Some(ra) = retry_after {
                err = err.with_retry_after(ra);
            }
            return Err(err);
        }
        let parsed: TavilySearchResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::invalid_response("tavily", format!("decode: {e}")))?;
        let duration_ms = started.elapsed().as_millis() as u64;
        let entries = parsed
            .results
            .into_iter()
            .map(|r| SearchResultEntry {
                title: r.title,
                url: r.url,
                content: r.content.unwrap_or_default(),
                score: r.score.unwrap_or(0.0).clamp(0.0, 1.0) as f32,
                published_at: r
                    .published_date
                    .and_then(|d| DateTime::parse_from_rfc3339(&d).ok().map(|dt| dt.into())),
                raw_content: r.raw_content,
            })
            .collect();
        Ok(ProviderResult {
            entries,
            duration_ms,
            bytes_transferred: bytes,
        })
    }
}

#[async_trait]
impl FetchProvider for TavilyProvider {
    fn id(&self) -> &str {
        "tavily"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            extract: true,
            ..Default::default()
        }
    }

    async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, ProviderError> {
        if !self.auth_configured() {
            return Err(ProviderError::auth("tavily", "missing API key"));
        }
        let started = Instant::now();
        let body = serde_json::json!({
            "api_key": self.api_key,
            "urls": [request.url],
            "extract_depth": match request.extract_depth {
                ExtractDepth::Basic => "basic",
                ExtractDepth::Advanced => "advanced",
            },
        });
        let url = format!("{}/extract", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network("tavily", e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::network("tavily", e.to_string()))?;
        if status != 200 {
            return Err(ProviderError::from_status("tavily", status, &text));
        }
        #[derive(Deserialize)]
        struct TavilyExtractResponse {
            #[serde(default)]
            results: Vec<TavilyExtractResult>,
        }
        #[derive(Deserialize)]
        struct TavilyExtractResult {
            url: String,
            #[serde(default)]
            raw_content: Option<String>,
        }
        let parsed: TavilyExtractResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::invalid_response("tavily", format!("decode: {e}")))?;
        let first = parsed
            .results
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::invalid_response("tavily", "no extract results"))?;
        Ok(FetchResponse {
            url: first.url,
            content: first.raw_content.unwrap_or_default(),
            title: None,
            duration_ms: started.elapsed().as_millis() as u64,
            bytes_transferred: text.len() as u64,
            from_cache: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_declared_by_adapter() {
        let p = TavilyProvider::new("test-key");
        let c = crate::traits::SearchProvider::capabilities(&p);
        assert!(c.search && c.extract && c.supports_domain_filter);
        assert!(!c.crawl);
    }

    #[test]
    fn missing_key_is_auth_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let p = TavilyProvider::new("");
        let err = rt
            .block_on(p.search(&SearchParams::simple("q", 5)))
            .unwrap_err();
        assert_eq!(err.kind.code(), "authentication_error");
    }

    #[test]
    fn wire_format_has_api_key() {
        let body = TavilySearchRequest {
            api_key: "k".into(),
            query: "q".into(),
            search_depth: Some("basic"),
            max_results: 5,
            include_raw_content: false,
            include_domains: vec!["github.com".into()],
            exclude_domains: vec![],
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["api_key"], "k");
        assert_eq!(json["include_domains"][0], "github.com");
        assert!(
            json.get("exclude_domains").is_none(),
            "empty fields omitted"
        );
    }
}
