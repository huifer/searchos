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

    /// Parses one interface-level mode spelling.
    ///
    /// All interfaces accept the canonical snake_case name plus the
    /// documented kebab-case alias so CLI examples, MCP schemas, and HTTP
    /// clients behave identically. Anything else is a validation error —
    /// never a silent fallback to an external-capable mode.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "local_only" | "local-only" => Ok(Self::LocalOnly),
            "local_first" | "local-first" => Ok(Self::LocalFirst),
            "external_first" | "external-first" => Ok(Self::ExternalFirst),
            "external_only" | "external-only" => Ok(Self::ExternalOnly),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(format!(
                "unknown mode {other}; use local_only|local_first|external_first|external_only|hybrid"
            )),
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

/// Shared result-limit contract for CLI, HTTP and MCP (INV-30-19).
pub const DEFAULT_MAX_RESULTS: u32 = 5;
pub const MAX_RESULTS_LIMIT: u32 = 100;

/// Clamps a transport result limit into the one shared range.
pub fn clamp_max_results(value: u32) -> u32 {
    value.clamp(1, MAX_RESULTS_LIMIT)
}

fn default_max_results() -> u32 {
    DEFAULT_MAX_RESULTS
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

impl FreshnessRequirement {
    /// Stable cache/trace label; the numeric maximum-age variant carries its
    /// value so differently bounded requests never share a cache identity.
    pub fn cache_label(&self) -> String {
        match self {
            Self::Any => "any".into(),
            Self::Recent => "recent".into(),
            Self::Strict => "strict".into(),
            Self::MaximumAgeSeconds(seconds) => format!("max_age_{seconds}"),
        }
    }
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
    /// Shared output cap. Every transport writes this field; the service
    /// enforces it before the response is returned (INV-30-19).
    #[serde(default = "default_max_results")]
    pub max_results: u32,
}

impl SearchRequest {
    /// Overrides the client-supplied idempotency key (RFC-014 14.7).
    ///
    /// Reusing a `request_id` must not create a second external search,
    /// a second bill, or a second document version.
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        let id = request_id.into();
        if !id.trim().is_empty() {
            self.request_id = crate::ids::RequestId::new(id);
        }
        self
    }

    /// Applies the shared transport result cap.
    pub fn with_max_results(mut self, max_results: u32) -> Self {
        self.max_results = clamp_max_results(max_results);
        self
    }

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
            max_results: DEFAULT_MAX_RESULTS,
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

    /// P0/P2 alignment: every interface must accept the same mode spellings
    /// and reject unknown modes instead of silently selecting one.
    #[test]
    fn mode_parsing_is_strict_and_shared() {
        assert_eq!(
            SearchMode::parse("local_only").unwrap(),
            SearchMode::LocalOnly
        );
        assert_eq!(
            SearchMode::parse("local-only").unwrap(),
            SearchMode::LocalOnly
        );
        assert_eq!(
            SearchMode::parse("external_first").unwrap(),
            SearchMode::ExternalFirst
        );
        for bogus in ["LOCAL_ONLY", "localOnly", "", "hybrid "] {
            assert!(SearchMode::parse(bogus).is_err(), "accepted {bogus:?}");
        }
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
    fn transports_share_one_result_limit_contract() {
        assert_eq!(DEFAULT_MAX_RESULTS, 5);
        assert_eq!(clamp_max_results(0), 1);
        assert_eq!(clamp_max_results(17), 17);
        assert_eq!(clamp_max_results(500), MAX_RESULTS_LIMIT);
        let req = SearchRequest::new(
            "q",
            ProfileId::general(),
            SearchMode::LocalFirst,
            WorkspaceId::new("ws"),
        )
        .unwrap()
        .with_max_results(500);
        assert_eq!(req.max_results, MAX_RESULTS_LIMIT);
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
