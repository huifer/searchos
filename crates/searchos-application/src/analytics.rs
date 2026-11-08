//! Analytics Query Service contract (RFC-026 4.3, RFC-028 2).
//!
//! The Web Dashboard calls this service through the Dashboard API. It returns
//! standardized Read Model payloads; it never exposes internal table names,
//! SQL, or raw private document bodies.

use async_trait::async_trait;
use searchos_domain::SearchOsError;
use serde::{Deserialize, Serialize};

/// Common Dashboard query contract (RFC-028 5).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalyticsQuery {
    pub workspace_id: Option<String>,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub provider_id: Option<String>,
    pub search_mode: Option<String>,
    pub quality_status: Option<String>,
    /// RFC3339 / unix-second filters are accepted by the HTTP layer and
    /// normalized here. Empty means "all available history".
    pub from: Option<String>,
    pub to: Option<String>,
    pub interval: Option<String>,
    pub timezone: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl AnalyticsQuery {
    /// Rejects inverted ranges and unsupported intervals before any query.
    pub fn validate(&self) -> Result<(), SearchOsError> {
        if let (Some(from), Some(to)) = (&self.from, &self.to) {
            if from > to {
                return Err(SearchOsError::validation(
                    "time_range",
                    "from must not be later than to",
                ));
            }
        }
        if let Some(interval) = &self.interval {
            const ALLOWED: [&str; 5] = ["minute", "hour", "day", "week", "month"];
            if !ALLOWED.contains(&interval.to_lowercase().as_str()) {
                return Err(SearchOsError::validation(
                    "interval",
                    "minute | hour | day | week | month",
                ));
            }
        }
        if let Some(tz) = &self.timezone {
            if !matches!(tz.as_str(), "UTC" | "utc") {
                return Err(SearchOsError::validation(
                    "timezone",
                    "MVP accepts UTC only",
                ));
            }
        }
        Ok(())
    }
}

/// One append-only analytics event (RFC-027 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsEvent {
    pub event_type: String,
    pub workspace_id: String,
    pub request_id: Option<String>,
    pub execution_id: Option<String>,
    pub source_module: String,
    pub correlation_id: Option<String>,
    pub payload: serde_json::Value,
}

#[async_trait]
pub trait AnalyticsQueryService: Send + Sync {
    /// Rebuilds the Read Models from operational data. Best-effort: a failure
    /// must never fail a search request (RFC-026 2.3).
    async fn refresh(&self) -> Result<(), SearchOsError>;

    /// Returns `{ data, meta }` for a named Read Model.
    async fn dataset(
        &self,
        model_key: &str,
        query: &AnalyticsQuery,
    ) -> Result<serde_json::Value, SearchOsError>;

    /// Overview aggregate, the primary Dashboard snapshot.
    async fn overview(&self, query: &AnalyticsQuery) -> Result<serde_json::Value, SearchOsError>;

    /// Appends one event. Errors are intentionally non-fatal for callers.
    async fn record(&self, event: AnalyticsEvent) -> Result<(), SearchOsError>;

    /// Events after an integer cursor, for SSE / recent-failure views.
    async fn events_since(
        &self,
        cursor: i64,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, SearchOsError>;
}
