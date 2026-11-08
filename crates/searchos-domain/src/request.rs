//! Search request domain types (RFC-002 2.3, RFC-003).

use crate::ids::{AgentId, ProfileId, RequestId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Execution mode requested for a search task (RFC-002 2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    LocalOnly,
    LocalFirst,
    ExternalFirst,
    ExternalOnly,
    Hybrid,
}

impl SearchMode {
    /// Snake-case name used in traces and CLI output.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LocalOnly => "local_only",
            Self::LocalFirst => "local_first",
            Self::ExternalFirst => "external_first",
            Self::ExternalOnly => "external_only",
            Self::Hybrid => "hybrid",
        }
    }

    /// Whether local retrieval is permitted under this mode.
    pub fn allows_local(&self) -> bool {
        matches!(self, Self::LocalOnly | Self::LocalFirst | Self::Hybrid)
    }

    /// Whether external execution is permitted under this mode.
    pub fn allows_external(&self) -> bool {
        matches!(
            self,
            Self::LocalFirst | Self::ExternalFirst | Self::ExternalOnly | Self::Hybrid
        )
    }
}

impl fmt::Display for SearchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Freshness requirement attached to a request (RFC-002 2.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessRequirement {
    Any,
    Recent,
    Strict,
    MaximumAgeSeconds(u64),
}

/// Budget bounds for one search task (RFC-009 9.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchBudget {
    pub max_external_queries: u32,
    pub max_fetch_pages: u32,
    /// Maximum total cost in micro units of [`Self::currency`].
    pub max_total_cost_micros: i64,
    pub max_execution_time_ms: u64,
    pub max_retries: u32,
    pub max_query_expansions: u32,
    pub currency: String,
}

impl Default for SearchBudget {
    fn default() -> Self {
        Self {
            max_external_queries: 5,
            max_fetch_pages: 10,
            max_total_cost_micros: 100_000, // 0.10 USD
            max_execution_time_ms: 30_000,
            max_retries: 2,
            max_query_expansions: 3,
            currency: "USD".into(),
        }
    }
}

/// Hard constraints a request places on results (RFC-002 2.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchConstraints {
    pub freshness: FreshnessRequirement,
    pub minimum_coverage: f32,
    pub minimum_quality: f32,
    pub required_domains: Vec<String>,
    pub excluded_domains: Vec<String>,
    pub language: Option<String>,
}

impl Default for SearchConstraints {
    fn default() -> Self {
        Self {
            freshness: FreshnessRequirement::Recent,
            minimum_coverage: 0.6,
            minimum_quality: 0.6,
            required_domains: Vec::new(),
            excluded_domains: Vec::new(),
            language: None,
        }
    }
}

/// Multi-label query intent (RFC-003 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchIntent {
    Concept,
    Technical,
    ExactUrl,
    Latest,
    News,
    Price,
    Comparison,
    Research,
    Troubleshooting,
    Documentation,
    Unknown,
}

impl SearchIntent {
    /// All labels considered "strongly time-sensitive" (RFC-004 Rule 1).
    pub fn is_time_sensitive(&self) -> bool {
        matches!(self, Self::Latest | Self::News | Self::Price)
    }
}

/// A normalized search task submitted by an agent (RFC-002 2.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub request_id: RequestId,
    pub query: String,
    pub profile: ProfileId,
    pub mode: SearchMode,
    pub workspace_id: WorkspaceId,
    pub agent_id: Option<AgentId>,
    pub budget: SearchBudget,
    pub constraints: SearchConstraints,
}

impl SearchRequest {
    /// Creates a request with defaults; validates non-empty query.
    pub fn new(
        query: impl Into<String>,
        profile: ProfileId,
        mode: SearchMode,
        workspace_id: WorkspaceId,
    ) -> Result<Self, crate::SearchOsError> {
        let query = query.into();
        if query.trim().is_empty() {
            return Err(crate::SearchOsError::validation(
                "query",
                "must not be empty",
            ));
        }
        Ok(Self {
            request_id: RequestId::generate(),
            query,
            profile,
            mode,
            workspace_id,
            agent_id: None,
            budget: SearchBudget::default(),
            constraints: SearchConstraints::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_permissions() {
        assert!(SearchMode::LocalOnly.allows_local());
        assert!(!SearchMode::LocalOnly.allows_external());
        assert!(SearchMode::ExternalOnly.allows_external());
        assert!(!SearchMode::ExternalOnly.allows_local());
        assert!(SearchMode::Hybrid.allows_local() && SearchMode::Hybrid.allows_external());
    }

    #[test]
    fn empty_query_rejected() {
        let err = SearchRequest::new(
            "  ",
            ProfileId::general(),
            SearchMode::LocalFirst,
            WorkspaceId::new("ws"),
        );
        assert!(err.is_err());
    }

    #[test]
    fn default_budget_matches_rfc() {
        let b = SearchBudget::default();
        assert_eq!(b.max_external_queries, 5);
        assert_eq!(b.max_fetch_pages, 10);
        assert_eq!(b.max_total_cost_micros, 100_000);
        assert_eq!(b.max_execution_time_ms, 30_000);
        assert_eq!(b.max_retries, 2);
        assert_eq!(b.max_query_expansions, 3);
    }
}
