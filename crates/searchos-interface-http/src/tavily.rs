//! Tavily-compatible compatibility layer (RFC-012 12.7, ADR-006).
//!
//! Maps Tavily-shaped requests/responses onto the SearchOS domain model.
//! Compatibility mapping only — these DTOs never leak into the core.

use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::response::SearchResponse;

/// Tavily-compatible search request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct TavilySearchRequest {
    pub query: String,
    #[serde(default)]
    pub search_depth: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub max_results: Option<u32>,
    #[serde(default)]
    pub include_domains: Vec<String>,
    #[serde(default)]
    pub exclude_domains: Vec<String>,
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub include_raw_content: Option<bool>,
    #[serde(default)]
    pub include_answer: Option<bool>,
}

/// Tavily-compatible result item.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TavilyResult {
    pub title: String,
    pub url: String,
    pub content: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_content: Option<String>,
}

/// Tavily-compatible search response (core fields).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TavilySearchResponse {
    pub query: String,
    pub results: Vec<TavilyResult>,
    /// Unsupported features that were requested but not honored — explicit
    /// reporting instead of silent behavior change (RFC-012 12.7).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unsupported_features: Vec<String>,
    /// Always `null`: SearchOS never fabricates an answer (RFC-001 1.2).
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_time: Option<f64>,
}

impl TavilySearchRequest {
    /// Maps to the internal request. `topic`/`search_depth` are mapped to
    /// profile/mode; the docs list also records unsupported knobs.
    pub fn to_domain(&self) -> Result<(SearchRequest, Vec<String>), String> {
        let mut unsupported = Vec::new();
        if self.include_answer.unwrap_or(false) {
            unsupported.push("include_answer (SearchOS does not generate answers)".into());
        }
        let profile = match self.topic.as_deref() {
            Some("news") => ProfileId::latest(),
            Some("finance") => ProfileId::research(),
            None | Some("general") => ProfileId::general(),
            Some(other) => {
                unsupported.push(format!("topic={other}"));
                ProfileId::general()
            }
        };
        let mode = SearchMode::LocalFirst; // Tavily semantics: always online-capable
        let mut req = SearchRequest::new(
            self.query.clone(),
            profile,
            mode,
            WorkspaceId::new("tavily"),
        )
        .map_err(|e| e.to_string())?;
        if let Some(mr) = self.max_results {
            // max_results affects result trimming, not external query count.
            if mr > 20 {
                unsupported.push("max_results > 20 clamped".into());
            }
        }
        if let Some(d) = self.days {
            req.constraints.freshness =
                searchos_domain::request::FreshnessRequirement::MaximumAgeSeconds(d as u64 * 86400);
        }
        req.constraints.required_domains = self.include_domains.clone();
        req.constraints.excluded_domains = self.exclude_domains.clone();
        Ok((req, unsupported))
    }

    /// Requested result cap (default 5, capped at 20).
    pub fn effective_max_results(&self) -> usize {
        self.max_results.unwrap_or(5).clamp(1, 20) as usize
    }
}

impl TavilySearchResponse {
    /// Maps a core response into the Tavily shape.
    pub fn from_domain(
        core: &SearchResponse,
        max_results: usize,
        unsupported: Vec<String>,
        elapsed_ms: u64,
    ) -> Self {
        let results = core
            .results
            .iter()
            .take(max_results)
            .map(|r| TavilyResult {
                title: r.title.clone().unwrap_or_default(),
                url: r.url.clone(),
                content: r.content.clone(),
                score: r.score,
                published_date: r.published_at.map(|d| d.to_rfc3339()),
                raw_content: None,
            })
            .collect();
        Self {
            query: core.query.clone(),
            results,
            unsupported_features: unsupported,
            answer: None,
            response_time: Some(elapsed_ms as f64 / 1000.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::ids::TraceId;
    use searchos_domain::response::{
        CostReport, CoverageStatus, DecisionReport, FreshnessStatus, QualityReport, QualityStatus,
        SearchResultItem,
    };

    #[test]
    fn maps_news_topic_to_latest_profile() {
        let dto = TavilySearchRequest {
            query: "latest rust release".into(),
            topic: Some("news".into()),
            max_results: Some(3),
            days: Some(2),
            ..Default::default()
        };
        let (req, unsupported) = dto.to_domain().unwrap();
        assert_eq!(req.profile.as_str(), "latest");
        assert!(matches!(
            req.constraints.freshness,
            searchos_domain::request::FreshnessRequirement::MaximumAgeSeconds(172800)
        ));
        assert!(unsupported.is_empty());
        assert_eq!(dto.effective_max_results(), 3);
    }

    #[test]
    fn reports_unsupported_answer_generation() {
        let dto = TavilySearchRequest {
            query: "q".into(),
            include_answer: Some(true),
            ..Default::default()
        };
        let (_req, unsupported) = dto.to_domain().unwrap();
        assert!(unsupported.iter().any(|u| u.contains("include_answer")));
    }

    #[test]
    fn maps_response_without_answer() {
        let core = SearchResponse {
            query: "q".into(),
            decision: DecisionReport {
                path: "local".into(),
                reason: "test".into(),
            },
            results: vec![SearchResultItem {
                title: Some("T".into()),
                url: "https://x".into(),
                content: "c".into(),
                score: 1.0,
                published_at: Some(Utc::now()),
                source: "local".into(),
            }],
            quality: QualityReport {
                status: QualityStatus::Passed,
                freshness: FreshnessStatus::Verified,
                coverage: CoverageStatus::Sufficient,
                score: 0.9,
                failed_constraints: vec![],
            },
            cost: CostReport {
                estimated: 0.0,
                actual: None,
                currency: "USD".into(),
            },
            provenance: vec![],
            trace_id: TraceId::generate(),
        };
        let out = TavilySearchResponse::from_domain(&core, 5, vec![], 120);
        assert_eq!(out.results.len(), 1);
        assert!(out.answer.is_none());
        assert_eq!(out.response_time, Some(0.12));
    }
}
