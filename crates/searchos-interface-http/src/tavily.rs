//! Tavily-compatible compatibility layer (RFC-012 12.7, ADR-006).
//!
//! Maps Tavily-shaped requests/responses onto the SearchOS domain model.
//! Compatibility mapping only — these DTOs never leak into the core.

use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::response::SearchResponse;
use std::collections::BTreeMap;

/// The frozen Tavily-compatible search request.
///
/// Supported knobs are mapped onto the SearchOS domain model. Every other
/// documented Tavily knob is reported back in
/// [`TavilySearchResponse::unsupported_features`], and unknown fields are
/// captured and reported too — nothing is silently ignored (RFC-012 12.7).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct TavilySearchRequest {
    pub query: String,
    // -- supported --
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
    /// Optional workspace override. Tavily clients normally get the
    /// dedicated `tavily` workspace; token-bound deployments can supply
    /// their own workspace explicitly.
    #[serde(default)]
    pub workspace_id: Option<String>,
    // -- documented by Tavily but not honored here; reported explicitly --
    #[serde(default)]
    pub search_depth: Option<String>,
    #[serde(default)]
    pub include_answer: Option<bool>,
    #[serde(default)]
    pub include_raw_content: Option<bool>,
    #[serde(default)]
    pub include_images: Option<bool>,
    #[serde(default)]
    pub include_image_descriptions: Option<bool>,
    #[serde(default)]
    pub chunks_per_source: Option<u32>,
    #[serde(default)]
    pub time_range: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub include_favicon: Option<bool>,
    #[serde(default)]
    pub auto_parameters: Option<bool>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub include_usage: Option<bool>,
    /// Any other field the client sent. Reported as unsupported rather than
    /// dropped, so a client can tell its request was not fully honored.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
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
    /// Unsupported features that were requested but not honored. Always
    /// present (possibly empty) so a client can tell the request was not
    /// fully honored instead of guessing (RFC-012 12.7).
    pub unsupported_features: Vec<String>,
    /// Always `null`: SearchOS never fabricates an answer (RFC-001 1.2).
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_time: Option<f64>,
}

impl TavilySearchRequest {
    /// Maps to the internal request. `topic`/`search_depth` are mapped to
    /// profile/mode; unsupported knobs are reported, never dropped.
    pub fn to_domain(&self) -> Result<(SearchRequest, Vec<String>), String> {
        let mut unsupported = Vec::new();
        if let Some(depth) = &self.search_depth {
            unsupported.push(format!(
                "search_depth={depth} (SearchOS selects a profile, not a depth preset)"
            ));
        }
        if self.include_answer.unwrap_or(false) {
            unsupported.push("include_answer (SearchOS does not generate answers)".into());
        }
        if self.include_raw_content.unwrap_or(false) {
            unsupported
                .push("include_raw_content (SearchOS returns extracted text, not raw HTML)".into());
        }
        if self.include_images.unwrap_or(false) {
            unsupported.push("include_images (SearchOS does not extract images)".into());
        }
        if self.include_image_descriptions.unwrap_or(false) {
            unsupported
                .push("include_image_descriptions (SearchOS does not extract images)".into());
        }
        if let Some(chunks) = self.chunks_per_source {
            unsupported.push(format!(
                "chunks_per_source={chunks} (SearchOS chunks are profile-driven)"
            ));
        }
        if let Some(range) = &self.time_range {
            unsupported.push(format!(
                "time_range={range} (use days=N for the supported numeric window)"
            ));
        }
        if let Some(country) = &self.country {
            unsupported.push(format!("country={country} (SearchOS has no geo filter)"));
        }
        if self.include_favicon.unwrap_or(false) {
            unsupported.push("include_favicon (SearchOS does not extract favicons)".into());
        }
        if self.auto_parameters.unwrap_or(false) {
            unsupported.push("auto_parameters (SearchOS never auto-selects knobs)".into());
        }
        if let Some(model) = &self.model {
            unsupported.push(format!(
                "model={model} (SearchOS does not proxy a model choice)"
            ));
        }
        if self.include_usage.unwrap_or(false) {
            unsupported.push("include_usage (SearchOS has no request quota to report)".into());
        }
        for key in self.extra.keys() {
            unsupported.push(format!("{key} (not part of the v1 Tavily subset)"));
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
            WorkspaceId::new(
                self.workspace_id
                    .as_deref()
                    .filter(|w| !w.trim().is_empty())
                    .unwrap_or("tavily"),
            ),
        )
        .map_err(|e| e.to_string())?;
        if let Some(mr) = self.max_results {
            // One shared cap across CLI, HTTP and MCP (INV-30-19).
            req = req.with_max_results(mr);
            if mr > searchos_domain::request::MAX_RESULTS_LIMIT {
                unsupported.push(format!(
                    "max_results > {} clamped",
                    searchos_domain::request::MAX_RESULTS_LIMIT
                ));
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

    /// Requested result cap (shared default 5, shared max 100).
    pub fn effective_max_results(&self) -> usize {
        searchos_domain::request::clamp_max_results(
            self.max_results
                .unwrap_or(searchos_domain::request::DEFAULT_MAX_RESULTS),
        ) as usize
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

/// Tavily-compatible extract request.
///
/// SearchOS extracts one URL per call; the first URL is honored and any
/// further URLs are reported back in `failed_results` with an explicit
/// reason instead of being silently dropped.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct TavilyExtractRequest {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub extract_depth: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub include_images: Option<bool>,
    #[serde(default)]
    pub include_favicon: Option<bool>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl TavilyExtractRequest {
    /// Unsupported knobs plus the reason each URL beyond the first is not
    /// processed in this call.
    pub fn unsupported(&self, extra_urls: usize) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(depth) = &self.extract_depth {
            out.push(format!(
                "extract_depth={depth} (SearchOS uses one extractor per content type)"
            ));
        }
        if let Some(format) = &self.format {
            out.push(format!(
                "format={format} (SearchOS returns markdown and raw text only)"
            ));
        }
        if self.include_images.unwrap_or(false) {
            out.push("include_images (SearchOS does not extract images)".into());
        }
        if self.include_favicon.unwrap_or(false) {
            out.push("include_favicon (SearchOS does not extract favicons)".into());
        }
        for key in self.extra.keys() {
            out.push(format!("{key} (not part of the v1 Tavily subset)"));
        }
        if extra_urls > 0 {
            out.push(format!(
                "urls[1..] ({extra_urls} URL(s) skipped; SearchOS extracts one URL per call)"
            ));
        }
        out
    }
}

/// One successful Tavily extract result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TavilyExtractResult {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Always `null`: SearchOS returns extracted text, never raw HTML.
    pub raw_content: Option<String>,
}

/// One failed URL in a Tavily extract request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TavilyExtractFailure {
    pub url: String,
    pub error: String,
}

/// Tavily-compatible extract response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TavilyExtractResponse {
    pub results: Vec<TavilyExtractResult>,
    pub failed_results: Vec<TavilyExtractFailure>,
    pub unsupported_features: Vec<String>,
    pub response_time: f64,
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

    /// Every documented-but-unhonored Tavily knob is reported; nothing is
    /// silently dropped (RFC-012 12.7).
    #[test]
    fn reports_every_unsupported_tavily_knob() {
        let dto = TavilySearchRequest {
            query: "q".into(),
            search_depth: Some("advanced".into()),
            include_raw_content: Some(true),
            include_images: Some(true),
            include_image_descriptions: Some(true),
            chunks_per_source: Some(3),
            time_range: Some("week".into()),
            country: Some("DE".into()),
            include_favicon: Some(true),
            auto_parameters: Some(true),
            model: Some("gpt-4o".into()),
            include_usage: Some(true),
            ..Default::default()
        };
        let (_req, unsupported) = dto.to_domain().unwrap();
        for fragment in [
            "search_depth",
            "include_raw_content",
            "include_images",
            "include_image_descriptions",
            "chunks_per_source",
            "time_range",
            "country",
            "include_favicon",
            "auto_parameters",
            "model",
            "include_usage",
        ] {
            assert!(
                unsupported.iter().any(|item| item.contains(fragment)),
                "{fragment} must be reported, got {unsupported:?}"
            );
        }
    }

    /// Unknown fields are captured and reported rather than dropped, so a
    /// future Tavily knob surfaces immediately.
    #[test]
    fn unknown_fields_are_reported() {
        let dto: TavilySearchRequest = serde_json::from_value(serde_json::json!({
            "query": "q",
            "search_depth": "basic",
            "some_future_knob": "on"
        }))
        .unwrap();
        let (_req, unsupported) = dto.to_domain().unwrap();
        assert!(
            unsupported
                .iter()
                .any(|item| item.contains("some_future_knob")),
            "unknown field must be reported, got {unsupported:?}"
        );
    }

    /// Supported knobs must not produce unsupported noise.
    #[test]
    fn supported_knobs_are_quiet() {
        let dto: TavilySearchRequest = serde_json::from_value(serde_json::json!({
            "query": "q",
            "topic": "general",
            "max_results": 3,
            "days": 2,
            "include_domains": ["example.com"],
            "exclude_domains": ["spam.example"]
        }))
        .unwrap();
        let (_req, unsupported) = dto.to_domain().unwrap();
        assert!(unsupported.is_empty(), "{unsupported:?}");
    }

    #[test]
    fn extract_reports_depth_format_and_extra_urls() {
        let dto = TavilyExtractRequest {
            urls: vec!["https://a.example".into(), "https://b.example".into()],
            extract_depth: Some("advanced".into()),
            format: Some("markdown".into()),
            ..Default::default()
        };
        let unsupported = dto.unsupported(1);
        for fragment in ["extract_depth", "format", "urls[1..]"] {
            assert!(
                unsupported.iter().any(|item| item.contains(fragment)),
                "{fragment} must be reported, got {unsupported:?}"
            );
        }
    }

    #[test]
    fn extract_unknown_fields_are_reported() {
        let dto: TavilyExtractRequest = serde_json::from_value(serde_json::json!({
            "urls": ["https://a.example"],
            "some_future_extract_knob": true
        }))
        .unwrap();
        let unsupported = dto.unsupported(0);
        assert!(
            unsupported
                .iter()
                .any(|item| item.contains("some_future_extract_knob")),
            "{unsupported:?}"
        );
    }

    /// The search response always carries `unsupported_features`, even when
    /// empty, so a client can tell the request was fully honored.
    #[test]
    fn unsupported_features_is_always_serialized() {
        let core = SearchResponse {
            query: "q".into(),
            decision: DecisionReport {
                path: "local".into(),
                reason: "test".into(),
            },
            results: vec![],
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
            status: searchos_domain::response::SearchStatus::Completed,
            warnings: vec![],
            task_id: None,
            replayed: false,
        };
        let out = TavilySearchResponse::from_domain(&core, 5, vec![], 10);
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["unsupported_features"], serde_json::json!([]));
        assert!(json.get("answer").unwrap().is_null());
    }

    #[test]
    fn workspace_override_is_respected() {
        let dto = TavilySearchRequest {
            query: "q".into(),
            workspace_id: Some("ws_bound".into()),
            ..Default::default()
        };
        let (req, _) = dto.to_domain().unwrap();
        assert_eq!(req.workspace_id.as_str(), "ws_bound");
        let default_dto = TavilySearchRequest {
            query: "q".into(),
            ..Default::default()
        };
        let (default_req, _) = default_dto.to_domain().unwrap();
        assert_eq!(default_req.workspace_id.as_str(), "tavily");
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
            status: searchos_domain::response::SearchStatus::Completed,
            warnings: vec![],
            task_id: None,
            replayed: false,
        };
        let out = TavilySearchResponse::from_domain(&core, 5, vec![], 120);
        assert_eq!(out.results.len(), 1);
        assert!(out.answer.is_none());
        assert_eq!(out.response_time, Some(0.12));
    }
}
