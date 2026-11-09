//! Provider traits (RFC-009 9.3).
//!
//! Capabilities must be declared by the adapter, never inferred from the
//! provider name.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use searchos_domain::provider::ProviderCapabilities;
use serde::{Deserialize, Serialize};

use crate::error::ProviderError;
use crate::keyring::KeyStatus;

/// Parameters for one web search (SearchOS internal IR, RFC-012 12.7: Tavily
/// DTOs must not penetrate the core).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchParams {
    pub query: String,
    pub max_results: u32,
    pub required_domains: Vec<String>,
    pub excluded_domains: Vec<String>,
    pub language: Option<String>,
    pub freshness_days: Option<u32>,
    /// Include raw page content in results when the provider supports it.
    pub include_raw_content: bool,
}

impl SearchParams {
    /// Simple params for a query.
    pub fn simple(query: impl Into<String>, max_results: u32) -> Self {
        Self {
            query: query.into(),
            max_results,
            required_domains: Vec::new(),
            excluded_domains: Vec::new(),
            language: None,
            freshness_days: None,
            include_raw_content: false,
        }
    }
}

/// One provider search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultEntry {
    pub title: Option<String>,
    pub url: String,
    pub content: String,
    /// Provider relevance score (0..=1 normalized by the adapter).
    pub score: f32,
    pub published_at: Option<DateTime<Utc>>,
    /// Raw page content when requested and available.
    pub raw_content: Option<String>,
}

/// Provider call result with duration for cost accounting.
#[derive(Debug, Clone)]
pub struct ProviderResult {
    pub entries: Vec<SearchResultEntry>,
    pub duration_ms: u64,
    /// Bytes transferred (request + response), for network cost events.
    pub bytes_transferred: u64,
}

/// Search provider trait (RFC-002 2.4 style).
#[async_trait]
pub trait SearchProvider: Send + Sync {
    /// Stable provider id, e.g. `tavily`.
    fn id(&self) -> &str;

    /// Declared capabilities (RFC-009 9.3).
    fn capabilities(&self) -> ProviderCapabilities;

    /// Executes one web search.
    async fn search(&self, params: &SearchParams) -> Result<ProviderResult, ProviderError>;

    /// Health check (RFC-014 14.4).
    async fn health_check(&self) -> Result<bool, ProviderError> {
        Ok(true)
    }

    /// Per-key rotation status for multi-key providers (RFC-009 9.7).
    /// `None` = single-key adapter (no rotation). Keys are masked.
    fn key_status(&self) -> Option<Vec<KeyStatus>> {
        let _ = self;
        None
    }
}

/// Fetch (extract) provider trait.
#[async_trait]
pub trait FetchProvider: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;

    /// Fetches and extracts content for one URL.
    async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, ProviderError>;
}

/// Fetch request (core IR).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchRequest {
    pub url: String,
    pub include_images: bool,
    pub extract_depth: ExtractDepth,
}

/// Basic vs advanced extraction (Tavily-compatible knob).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractDepth {
    Basic,
    Advanced,
}

/// Fetch response (core IR).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResponse {
    pub url: String,
    /// Extracted main content (markdown-ish text).
    pub content: String,
    pub title: Option<String>,
    pub duration_ms: u64,
    pub bytes_transferred: u64,
    /// Whether the fetch came from a cache layer.
    pub from_cache: bool,
}
