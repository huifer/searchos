//! Brave Search adapter (RFC-009 9.5 MVP provider 2).

use async_trait::async_trait;
use chrono::DateTime;
use searchos_domain::provider::ProviderCapabilities;
use serde::Deserialize;
use std::time::Instant;

use crate::error::ProviderError;
use crate::traits::{ProviderResult, SearchParams, SearchProvider, SearchResultEntry};

/// Brave Search client.
pub struct BraveProvider {
    api_key: String,
    http: reqwest::Client,
    base_url: String,
}

impl BraveProvider {
    /// Client with API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("searchos/0.1 (+https://github.com/searchos/searchos)")
                .build()
                .expect("reqwest client"),
            base_url: "https://api.search.brave.com/res".into(),
        }
    }

    /// Overrides the base URL (tests / gateways).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

#[derive(Debug, Deserialize)]
struct BraveWebResponse {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    title: Option<String>,
    url: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    page_age: Option<String>,
}

#[async_trait]
impl SearchProvider for BraveProvider {
    fn id(&self) -> &str {
        "brave"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            search: true,
            extract: false,
            crawl: false,
            map: false,
            semantic_search: false,
            supports_domain_filter: false,
            supports_date_filter: true,
        }
    }

    async fn search(&self, params: &SearchParams) -> Result<ProviderResult, ProviderError> {
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::auth(
                "brave",
                "missing API key (set BRAVE_API_KEY)",
            ));
        }
        let started = Instant::now();
        let url = format!("{}/web/search", self.base_url);
        let mut req = self
            .http
            .get(&url)
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&[
                ("q", &params.query),
                ("count", &params.max_results.clamp(1, 20).to_string()),
            ]);
        if let Some(fresh) = params.freshness_days {
            let freshness = if fresh <= 1 {
                "pd"
            } else if fresh <= 7 {
                "pw"
            } else if fresh <= 31 {
                "pm"
            } else {
                "py"
            };
            req = req.query(&[("freshness", freshness)]);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ProviderError::network("brave", e.to_string()))?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::network("brave", e.to_string()))?;
        if status != 200 {
            let mut err = ProviderError::from_status("brave", status, &text);
            if let Some(ra) = retry_after {
                err = err.with_retry_after(ra);
            }
            return Err(err);
        }
        let parsed: BraveWebResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::invalid_response("brave", format!("decode: {e}")))?;
        let duration_ms = started.elapsed().as_millis() as u64;
        let entries = parsed
            .results
            .into_iter()
            .take(params.max_results as usize)
            .map(|r| SearchResultEntry {
                score: 0.5, // Brave does not expose per-result relevance scores
                title: r.title,
                url: r.url,
                content: r.description.unwrap_or_default(),
                published_at: r
                    .page_age
                    .and_then(|d| DateTime::parse_from_rfc3339(&d).ok().map(|dt| dt.into())),
                raw_content: None,
            })
            .collect();
        Ok(ProviderResult {
            entries,
            duration_ms,
            bytes_transferred: text.len() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_declared_by_adapter() {
        let p = BraveProvider::new("k");
        let c = p.capabilities();
        assert!(c.search && c.supports_date_filter);
        assert!(!c.extract);
    }

    #[test]
    fn missing_key_is_auth_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let p = BraveProvider::new("");
        let err = rt
            .block_on(p.search(&SearchParams::simple("q", 5)))
            .unwrap_err();
        assert_eq!(err.kind.code(), "authentication_error");
    }
}
