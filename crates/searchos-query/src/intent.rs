//! Intent classification (RFC-003 3.3) and source requirement extraction
//! (RFC-004 Rule 2).
//!
//! Intents are multi-label: `Compare latest Tokio and Axum versions` yields
//! Comparison + Latest + Technical + Documentation.

use regex::Regex;
use searchos_domain::request::SearchIntent;
use std::sync::OnceLock;

use crate::TemporalSensitivity;

fn re(pattern: &str) -> Regex {
    static CACHE: OnceLock<std::collections::HashMap<&'static str, Regex>> = OnceLock::new();
    let map = CACHE.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        let macros: &[(&str, &str)] = &[
            ("url", r"(https?://\S+|\b[a-z0-9-]+\.(?:com|org|net|io|dev|rs|sh|ai|cn|jp|de|uk)\b)"),
            ("latest", r"\b(latest|newest|current version|up to date|up-to-date|最新|最近|现在|目前)\b"),
            ("news", r"\b(news|breaking|announce|announced|发布|新闻|公告)\b"),
            ("price", r"\b(price|pricing|cost|how much|多少钱|价格|定价|费用)\b"),
            ("compare", r"\b(compare|comparison|vs\.?|versus|difference|对比|比较|区别|哪个)\b"),
            ("trouble", r"\b(error|fail|panic|crash|not work|doesn't work|fix|debug|报错|失败|修复|排查)\b"),
            ("howto", r"\b(how (to|do|does|can)|how-to|tutorial|guide|怎么|如何|教程)\b"),
            ("research", r"\b(research|survey|state of the art|landscape|综述|调研|深入)\b"),
            ("docs", r"\b(documentation|docs|api reference|manual|文档|手册|参考)\b"),
            ("concept", r"\b(what is|what are|who|why|when|explain|definition|是什么|定义|为什么)\b"),
            ("tech", r"\b(crate|library|framework|api|sdk|rust|tokio|axum|python|golang|node|kubernetes|版本|framework)\b"),
        ];
        for (k, v) in macros {
            m.insert(*k, Regex::new(v).unwrap());
        }
        m
    });
    map.get(pattern)
        .cloned()
        .unwrap_or_else(|| Regex::new(pattern).unwrap())
}

/// Classifies a raw+normalized query into a multi-label intent set.
pub fn classify_intents(raw: &str, _normalized: &str) -> Vec<SearchIntent> {
    let lower = raw.to_lowercase();
    let mut intents = Vec::new();

    if re("url").is_match(&lower) {
        intents.push(SearchIntent::ExactUrl);
    }
    if re("latest").is_match(&lower) {
        intents.push(SearchIntent::Latest);
    }
    if re("news").is_match(&lower) {
        intents.push(SearchIntent::News);
    }
    if re("price").is_match(&lower) {
        intents.push(SearchIntent::Price);
    }
    if re("compare").is_match(&lower) {
        intents.push(SearchIntent::Comparison);
    }
    if re("trouble").is_match(&lower) {
        intents.push(SearchIntent::Troubleshooting);
    }
    if re("howto").is_match(&lower) {
        intents.push(SearchIntent::Troubleshooting);
        intents.push(SearchIntent::Documentation);
    }
    if re("research").is_match(&lower) {
        intents.push(SearchIntent::Research);
    }
    if re("docs").is_match(&lower) {
        intents.push(SearchIntent::Documentation);
    }
    if re("concept").is_match(&lower) {
        intents.push(SearchIntent::Concept);
    }
    if re("tech").is_match(&lower) {
        intents.push(SearchIntent::Technical);
    }

    if intents.is_empty() {
        intents.push(SearchIntent::Unknown);
    } else {
        intents.sort_by_key(|i| intent_order(*i));
        intents.dedup();
    }
    intents
}

fn intent_order(i: SearchIntent) -> u8 {
    match i {
        SearchIntent::ExactUrl => 0,
        SearchIntent::Latest => 1,
        SearchIntent::News => 2,
        SearchIntent::Price => 3,
        SearchIntent::Comparison => 4,
        SearchIntent::Research => 5,
        SearchIntent::Troubleshooting => 6,
        SearchIntent::Documentation => 7,
        SearchIntent::Technical => 8,
        SearchIntent::Concept => 9,
        SearchIntent::Unknown => 10,
    }
}

/// Temporal sensitivity detection (RFC-003 3.2 pipeline step).
pub fn detect_temporal_sensitivity(raw: &str, intents: &[SearchIntent]) -> TemporalSensitivity {
    let lower = raw.to_lowercase();
    let realtime =
        re(r"\b(today|now|right now|live|real.?time|just released|今天|现在|刚刚|实时)\b")
            .is_match(&lower);
    let strong = intents.iter().any(|i| i.is_time_sensitive());
    if realtime || strong {
        TemporalSensitivity::Strict
    } else if re(r"\b(recent|new|newer|最近|新版)\b").is_match(&lower) {
        TemporalSensitivity::Recent
    } else {
        TemporalSensitivity::Static
    }
}

/// Source requirement extracted from a query (RFC-004 Rule 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SourceRequirement {
    /// Exact URL the user named, if any.
    pub exact_url: Option<String>,
    /// Domains the user explicitly required (e.g. "official site").
    pub required_domains: Vec<String>,
    /// GitHub repository the user named (`owner/repo`).
    pub github_repo: Option<String>,
    /// Whether the user demanded official sources.
    pub require_official: bool,
}

impl SourceRequirement {
    /// Whether any explicit source requirement exists (Rule 2).
    pub fn has_explicit_requirement(&self) -> bool {
        self.exact_url.is_some()
            || !self.required_domains.is_empty()
            || self.github_repo.is_some()
            || self.require_official
    }
}

use serde::{Deserialize, Serialize};

/// Detects explicit source requirements in the query text.
pub fn detect_source_requirement(raw: &str) -> SourceRequirement {
    let mut req = SourceRequirement::default();
    let lower = raw.to_lowercase();

    if let Some(m) = re("url").find(&lower) {
        let mut u = m.as_str().to_string();
        if !u.starts_with("http") {
            u = format!("https://{u}");
        }
        let domain = searchos_policy::SourcePolicy::domain_of(&u);
        req.exact_url = Some(u);
        if let Some(d) = domain {
            req.required_domains.push(d);
        }
    }

    if lower.contains("official") || lower.contains("官方网站") || lower.contains("官方") {
        req.require_official = true;
    }

    if let Some(m) = Regex::new(r"github\.com/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)")
        .unwrap()
        .captures(&lower)
    {
        req.github_repo = Some(format!(
            "{}/{}",
            &m[1],
            m[2].trim_end_matches(['.', ' ', '/'])
        ));
        req.required_domains.push("github.com".into());
    }

    req
}

/// Naive named-entity extraction: capitalized multi-word sequences and
/// known tech tokens. Used by coverage gap analysis, not for final answers.
pub fn extract_entities(raw: &str) -> Vec<String> {
    let cap_re = re(
        r"\b([A-Z][A-Za-z0-9]*(?:[-.][A-Za-z0-9]+)*)(?:\s+(?:and|vs\.?|or|与|和|跟)\s+([A-Z][A-Za-z0-9]*))?",
    );
    let mut entities: Vec<String> = Vec::new();
    for c in cap_re.captures_iter(raw) {
        if let Some(a) = c.get(1) {
            let e = a.as_str().to_string();
            if !is_stopword(&e) && !entities.contains(&e) {
                entities.push(e);
            }
        }
        if let Some(b) = c.get(2) {
            let e = b.as_str().to_string();
            if !is_stopword(&e) && !entities.contains(&e) {
                entities.push(e);
            }
        }
    }
    entities
}

fn is_stopword(s: &str) -> bool {
    matches!(
        s.to_lowercase().as_str(),
        "what"
            | "who"
            | "why"
            | "when"
            | "how"
            | "the"
            | "a"
            | "an"
            | "is"
            | "are"
            | "latest"
            | "new"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_query_is_multi_label() {
        let intents = classify_intents("Compare latest Tokio and Axum versions", "");
        assert!(intents.contains(&SearchIntent::Comparison));
        assert!(intents.contains(&SearchIntent::Latest));
        assert!(intents.contains(&SearchIntent::Technical));
        assert!(intents.len() >= 3);
    }

    #[test]
    fn price_query_detected() {
        let intents = classify_intents("What is the pricing of OpenAI o3?", "");
        assert!(intents.contains(&SearchIntent::Price));
    }

    #[test]
    fn url_intent_and_source_requirement() {
        let req = detect_source_requirement("how to install from https://docs.rs/axum");
        assert_eq!(req.exact_url.as_deref(), Some("https://docs.rs/axum"));
        assert_eq!(req.required_domains, vec!["docs.rs".to_string()]);
    }

    #[test]
    fn github_repo_extracted() {
        let req = detect_source_requirement("check https://github.com/tokio-rs/tokio releases");
        assert_eq!(req.github_repo.as_deref(), Some("tokio-rs/tokio"));
    }

    #[test]
    fn official_flag() {
        let req = detect_source_requirement("official rust release notes");
        assert!(req.require_official);
    }

    #[test]
    fn entities_extracted_from_comparison() {
        let e = extract_entities("Compare Tokio and Axum performance");
        assert!(e.contains(&"Tokio".to_string()));
        assert!(e.contains(&"Axum".to_string()));
    }

    #[test]
    fn realtime_is_strict() {
        let intents = classify_intents("What happened today in tech news?", "");
        let t = detect_temporal_sensitivity("What happened today in tech news?", &intents);
        assert_eq!(t, TemporalSensitivity::Strict);
    }
}
