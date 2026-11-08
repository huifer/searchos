//! Freshness policy and TTL table (RFC-005 5.5).
//!
//! TTL 是默认刷新策略，不等于文档在 TTL 到期前绝对正确。

use serde::{Deserialize, Serialize};

/// Content category driving default refresh TTLs (RFC-005 5.5 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentCategory {
    /// 静态技术概念 — 180 days.
    StaticConcept,
    /// 官方技术文档 — 30 days (7–30 permitted).
    OfficialDocs,
    /// GitHub Release — 3 days (1–3 permitted).
    GithubRelease,
    /// API Pricing — 24 hours (6–24 permitted).
    ApiPricing,
    /// 新闻 — 60 minutes (15–60 permitted).
    News,
    /// 实时信息 — not cached / extremely short TTL.
    Realtime,
}

impl ContentCategory {
    /// Default TTL in seconds per RFC-005 5.5.
    pub fn default_ttl_seconds(&self) -> Option<u64> {
        Some(match self {
            Self::StaticConcept => 180 * 24 * 3600,
            Self::OfficialDocs => 30 * 24 * 3600,
            Self::GithubRelease => 3 * 24 * 3600,
            Self::ApiPricing => 24 * 3600,
            Self::News => 60 * 60,
            Self::Realtime => 0, // never cached
        })
    }

    /// Parses from snake_case name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "static_concept" => Self::StaticConcept,
            "official_docs" => Self::OfficialDocs,
            "github_release" => Self::GithubRelease,
            "api_pricing" => Self::ApiPricing,
            "news" => Self::News,
            "realtime" => Self::Realtime,
            _ => return None,
        })
    }
}

/// Freshness policy: per-category TTLs + strict maximum age.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreshnessPolicy {
    /// Version marker; changing it invalidates dependent caches.
    pub version: String,
    /// TTL override table (falls back to [`ContentCategory::default_ttl_seconds`]).
    pub ttl_overrides: std::collections::HashMap<String, u64>,
    /// Documents older than this are "stale" regardless of category.
    pub strict_max_age_seconds: Option<u64>,
}

impl FreshnessPolicy {
    /// Default policy with the RFC table.
    pub fn default_policy() -> Self {
        Self {
            version: crate::FRESHNESS_POLICY_VERSION.into(),
            ttl_overrides: Default::default(),
            strict_max_age_seconds: Some(365 * 24 * 3600),
        }
    }

    /// Effective TTL for a category.
    pub fn ttl_seconds(&self, category: ContentCategory) -> Option<u64> {
        if let Some(v) = self.ttl_overrides.get(category_name(category)) {
            return if *v == 0 { None } else { Some(*v) };
        }
        category.default_ttl_seconds().filter(|v| *v > 0)
    }

    /// Whether content fetched at `age_seconds` is fresh for `category`.
    pub fn is_fresh(&self, category: ContentCategory, age_seconds: i64) -> bool {
        match self.ttl_seconds(category) {
            None => false, // realtime: never fresh from cache
            Some(ttl) => {
                if age_seconds < 0 {
                    return true;
                }
                if let Some(strict) = self.strict_max_age_seconds {
                    if age_seconds as u64 > strict {
                        return false;
                    }
                }
                (age_seconds as u64) <= ttl
            }
        }
    }
}

fn category_name(c: ContentCategory) -> &'static str {
    match c {
        ContentCategory::StaticConcept => "static_concept",
        ContentCategory::OfficialDocs => "official_docs",
        ContentCategory::GithubRelease => "github_release",
        ContentCategory::ApiPricing => "api_pricing",
        ContentCategory::News => "news",
        ContentCategory::Realtime => "realtime",
    }
}

/// Naive classifier: infers content category from URL and title keywords.
pub fn classify_content(url: &str, title: Option<&str>) -> ContentCategory {
    let u = url.to_lowercase();
    let t = title.unwrap_or("").to_lowercase();
    if u.contains("github.com")
        && (u.contains("/releases") || u.contains("release") || u.contains("/tags"))
    {
        return ContentCategory::GithubRelease;
    }
    if u.contains("pricing") || t.contains("pricing") || t.contains("price") {
        return ContentCategory::ApiPricing;
    }
    if u.contains("news") || u.contains("blog") && t.contains("announce") {
        return ContentCategory::News;
    }
    if u.contains("docs.rs") || u.contains("/docs/") || t.contains("documentation") {
        return ContentCategory::OfficialDocs;
    }
    ContentCategory::StaticConcept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_table_matches_rfc() {
        let p = FreshnessPolicy::default_policy();
        assert_eq!(
            p.ttl_seconds(ContentCategory::StaticConcept),
            Some(180 * 24 * 3600)
        );
        assert_eq!(
            p.ttl_seconds(ContentCategory::GithubRelease),
            Some(3 * 24 * 3600)
        );
        assert_eq!(p.ttl_seconds(ContentCategory::ApiPricing), Some(24 * 3600));
        assert_eq!(p.ttl_seconds(ContentCategory::News), Some(60 * 60));
        assert_eq!(p.ttl_seconds(ContentCategory::Realtime), None);
    }

    #[test]
    fn stale_detection() {
        let p = FreshnessPolicy::default_policy();
        assert!(p.is_fresh(ContentCategory::StaticConcept, 100 * 24 * 3600));
        assert!(!p.is_fresh(ContentCategory::StaticConcept, 200 * 24 * 3600));
        assert!(!p.is_fresh(ContentCategory::News, 2 * 60 * 60));
    }

    #[test]
    fn classify_github_release() {
        assert_eq!(
            classify_content("https://github.com/tokio-rs/tokio/releases", None),
            ContentCategory::GithubRelease
        );
        assert_eq!(
            classify_content("https://openai.com/pricing", None),
            ContentCategory::ApiPricing
        );
    }
}
