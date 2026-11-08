//! Crawler manifest schema (RFC-010 10.3).

use serde::{Deserialize, Serialize};

/// Manifest `type` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrawlerType {
    SiteCrawler,
    Generic,
}

/// Match specification: domains this crawler claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchSpec {
    pub domains: Vec<String>,
}

/// Runtime specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSpec {
    /// `native` (MVP) or future `wasm`.
    #[serde(rename = "type")]
    pub kind: String,
    pub timeout_ms: u64,
}

/// Permission block (RFC-011 11.5: 扩展只能访问授权资源).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Permissions {
    /// Explicitly allowed network targets.
    pub network: Vec<String>,
}

/// Healthcheck block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthCheck {
    pub enabled: bool,
}

/// Crawler manifest (RFC-010 10.3 example schema).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrawlerManifest {
    /// Id like `crawler.github.com`.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "type")]
    pub kind: CrawlerType,
    #[serde(rename = "match")]
    pub match_spec: MatchSpec,
    pub capabilities: Vec<String>,
    pub runtime: RuntimeSpec,
    pub permissions: Permissions,
    pub healthcheck: Option<HealthCheck>,
}

impl CrawlerManifest {
    /// Parses a manifest from YAML.
    pub fn from_yaml(yaml: &str) -> Result<Self, String> {
        serde_yaml::from_str(yaml).map_err(|e| format!("manifest yaml: {e}"))
    }

    /// Validates the manifest (RFC-011 11.3 Manifest Validation).
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || !self.id.starts_with("crawler.") {
            return Err("id must start with 'crawler.'".into());
        }
        if self.version.split('.').count() != 3 {
            return Err("version must be semver x.y.z".into());
        }
        if self.match_spec.domains.is_empty() && self.kind == CrawlerType::SiteCrawler {
            return Err("site crawler must claim at least one domain".into());
        }
        if self.runtime.kind != "native" && self.runtime.kind != "wasm" {
            return Err("runtime.type must be native or wasm".into());
        }
        // Permissions must not exceed the claimed domains (least privilege).
        for n in &self.permissions.network {
            let host = n.split(':').next().unwrap_or(n);
            if self.kind == CrawlerType::SiteCrawler
                && !self
                    .match_spec
                    .domains
                    .iter()
                    .any(|d| host == d || host.ends_with(&format!(".{d}")))
                && host != "*"
            {
                return Err(format!(
                    "permission network target '{host}' outside claimed domains"
                ));
            }
        }
        Ok(())
    }

    /// Whether this crawler claims a URL's host (exact domain match first,
    /// RFC-010 10.4).
    pub fn claims(&self, host: &str) -> bool {
        self.match_spec
            .domains
            .iter()
            .any(|d| host == d.as_str() || host.ends_with(&format!(".{d}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GITHUB_MANIFEST: &str = r#"
id: crawler.github.com
name: GitHub Crawler
version: 1.0.0

type: site_crawler

match:
  domains:
    - github.com

capabilities:
  - fetch
  - extract
  - pagination

runtime:
  type: native
  timeout_ms: 15000

permissions:
  network:
    - github.com

healthcheck:
  enabled: true
"#;

    #[test]
    fn parses_rfc_example_manifest() {
        let m = CrawlerManifest::from_yaml(GITHUB_MANIFEST).unwrap();
        assert_eq!(m.id, "crawler.github.com");
        assert_eq!(m.kind, CrawlerType::SiteCrawler);
        assert!(m.claims("github.com"));
        assert!(
            m.claims("gist.github.com"),
            "subdomains of the claimed domain match"
        );
        // raw.githubusercontent.com is NOT under github.com — needs its own
        // registration (RFC-010 10.4: explicit registration only).
        assert!(!m.claims("raw.githubusercontent.com"));
        assert!(!m.claims("gitlab.com"));
        m.validate().unwrap();
    }

    #[test]
    fn rejects_permission_outside_domains() {
        let mut m = CrawlerManifest::from_yaml(GITHUB_MANIFEST).unwrap();
        m.permissions.network = vec!["evil.example.net".into()];
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_bad_id_and_version() {
        let mut m = CrawlerManifest::from_yaml(GITHUB_MANIFEST).unwrap();
        m.id = "notcrawlers".into();
        assert!(m.validate().is_err());
        let mut m2 = CrawlerManifest::from_yaml(GITHUB_MANIFEST).unwrap();
        m2.version = "1.0".into();
        assert!(m2.validate().is_err());
    }
}
