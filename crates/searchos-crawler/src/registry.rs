//! Crawler selection & registry (RFC-010 10.4).
//!
//! 优先级: 1 精确域名匹配 → 2 显式注册域名匹配 → 3 Generic Crawler →
//! 4 Fallback Fetch. 同一 URL 不允许多个专属 Crawler 无序竞争.

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use crate::manifest::CrawlerManifest;
use crate::status::{CrawlerStatus, FailureTracker};

/// The selected executor for a URL.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectedCrawler {
    /// A site-specific crawler claimed this domain.
    SiteSpecific(String),
    /// Generic crawler.
    Generic,
    /// Plain fallback fetch (no crawler claims this URL).
    FallbackFetch,
}

/// Registry of loaded crawler manifests + failure tracker.
pub struct CrawlerRegistry {
    crawlers: RwLock<HashMap<String, CrawlerManifest>>,
    tracker: FailureTracker,
}

impl CrawlerRegistry {
    /// Empty registry with a 5-failure / 60s quarantine policy.
    pub fn new() -> Self {
        Self {
            crawlers: RwLock::new(HashMap::new()),
            tracker: FailureTracker::new(5, std::time::Duration::from_secs(60)),
        }
    }

    /// Registers a manifest (validates first, RFC-010 10.3).
    pub fn register(&self, manifest: CrawlerManifest) -> Result<(), String> {
        manifest.validate()?;
        self.crawlers
            .write()
            .unwrap()
            .insert(manifest.id.clone(), manifest);
        Ok(())
    }

    /// Discovers manifests from an extensions/crawlers directory tree
    /// (each subdir holds manifest.yaml).
    pub fn discover(&self, dir: &Path) -> Result<Vec<String>, String> {
        let mut loaded = Vec::new();
        let mut errors = Vec::new();
        if !dir.exists() {
            return Ok(loaded);
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let manifest_path = entry.path().join("manifest.yaml");
            if !manifest_path.exists() {
                continue;
            }
            let yaml = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
            match CrawlerManifest::from_yaml(&yaml).and_then(|m| self.register(m).map(|_| ())) {
                Ok(()) => loaded.push(entry.file_name().to_string_lossy().to_string()),
                Err(e) => errors.push(format!("{}: {e}", entry.file_name().to_string_lossy())),
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        Ok(loaded)
    }

    /// Selects the executor for a URL host (RFC-010 10.4 priority).
    /// Quarantined/disabled site crawlers are skipped.
    pub fn select(&self, host: &str) -> SelectedCrawler {
        let map = self.crawlers.read().unwrap();
        // Priority 1: exact domain match, priority 2: registered suffix match.
        let mut best: Option<(&CrawlerManifest, usize)> = None;
        for m in map.values() {
            if self.tracker.status(&m.id) == CrawlerStatus::Quarantined
                || self.tracker.status(&m.id) == CrawlerStatus::Disabled
            {
                continue;
            }
            for d in &m.match_spec.domains {
                let exact = host == d.as_str();
                let suffix = host.ends_with(&format!(".{d}"));
                let priority = if exact {
                    0
                } else if suffix {
                    1
                } else {
                    continue;
                };
                if best.map(|(_, p)| priority < p).unwrap_or(true) {
                    best = Some((m, priority));
                    break;
                }
            }
        }
        match best {
            Some((m, _)) => SelectedCrawler::SiteSpecific(m.id.clone()),
            None => SelectedCrawler::Generic,
        }
    }

    /// All registered manifests (for `crawler list`).
    pub fn list(&self) -> Vec<CrawlerManifest> {
        let mut v: Vec<CrawlerManifest> = self.crawlers.read().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Inspects one crawler (for `crawler inspect`).
    pub fn inspect(&self, id: &str) -> Option<(CrawlerManifest, CrawlerStatus)> {
        self.crawlers
            .read()
            .unwrap()
            .get(id)
            .map(|m| (m.clone(), self.tracker.status(id)))
    }

    /// Failure tracker access (record failures/successes, list failures).
    pub fn tracker(&self) -> &FailureTracker {
        &self.tracker
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.crawlers.read().unwrap().is_empty()
    }
}

impl Default for CrawlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{CrawlerErrorKind, CrawlerFailure};

    const GH: &str = r#"
id: crawler.github.com
name: GitHub Crawler
version: 1.0.0
type: site_crawler
match:
  domains:
    - github.com
capabilities: [fetch, extract, pagination]
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
    fn exact_match_beats_suffix_and_fallback() {
        let r = CrawlerRegistry::new();
        r.register(CrawlerManifest::from_yaml(GH).unwrap()).unwrap();
        assert_eq!(
            r.select("github.com"),
            SelectedCrawler::SiteSpecific("crawler.github.com".into())
        );
        assert_eq!(
            r.select("raw.github.com"),
            SelectedCrawler::SiteSpecific("crawler.github.com".into())
        );
        assert_ne!(
            r.select("gitlab.com"),
            SelectedCrawler::SiteSpecific("crawler.github.com".into())
        );
    }

    #[test]
    fn quarantined_crawler_skipped() {
        let r = CrawlerRegistry::new();
        r.register(CrawlerManifest::from_yaml(GH).unwrap()).unwrap();
        r.tracker().record_failure(CrawlerFailure {
            crawler_id: "crawler.github.com".into(),
            kind: CrawlerErrorKind::Timeout,
            url: "u".into(),
            detail: "d".into(),
            at_ms: 0,
        });
        assert_eq!(
            r.tracker().status("crawler.github.com"),
            CrawlerStatus::Degraded
        );
        assert_eq!(
            r.select("github.com"),
            SelectedCrawler::SiteSpecific("crawler.github.com".into())
        );
        for _ in 0..5 {
            r.tracker().record_failure(CrawlerFailure {
                crawler_id: "crawler.github.com".into(),
                kind: CrawlerErrorKind::Timeout,
                url: "u".into(),
                detail: "d".into(),
                at_ms: 0,
            });
        }
        assert_eq!(
            r.select("github.com"),
            SelectedCrawler::Generic,
            "quarantined crawler skipped"
        );
    }

    #[test]
    fn discover_from_directory() {
        let dir = std::env::temp_dir().join(format!("searchos-crawlers-{}", std::process::id()));
        let gh_dir = dir.join("github.com");
        std::fs::create_dir_all(&gh_dir).unwrap();
        std::fs::write(gh_dir.join("manifest.yaml"), GH).unwrap();
        let r = CrawlerRegistry::new();
        let loaded = r.discover(&dir).unwrap();
        assert_eq!(loaded, vec!["github.com".to_string()]);
        assert_eq!(r.list().len(), 1);
        let (m, status) = r.inspect("crawler.github.com").unwrap();
        assert_eq!(m.id, "crawler.github.com");
        assert_eq!(status, CrawlerStatus::Healthy);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inspect_unknown_is_none() {
        let r = CrawlerRegistry::new();
        assert!(r.inspect("crawler.nope").is_none());
        assert!(r.is_empty());
    }
}
