//! Source policy (RFC-003 3.5, RFC-004 Rule 2).

use serde::{Deserialize, Serialize};

/// Which source-policy constraint failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceViolation {
    ExcludedDomain,
    RequiredDomainMissing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePolicy {
    /// Version marker; changing it invalidates dependent caches.
    pub version: String,
    pub required_domains: Vec<String>,
    pub excluded_domains: Vec<String>,
    /// Require the exact source the user named; no substitutes (RFC-004 Rule 2).
    pub require_exact_source: bool,
}

impl SourcePolicy {
    /// Default permissive-but-trusted policy.
    pub fn default_policy() -> Self {
        Self {
            version: crate::SOURCE_POLICY_VERSION.into(),
            required_domains: Vec::new(),
            excluded_domains: Vec::new(),
            require_exact_source: false,
        }
    }

    /// Extracts the registrable-ish domain from a URL string.
    pub fn domain_of(url: &str) -> Option<String> {
        let u = url.trim();
        let rest = u
            .strip_prefix("https://")
            .or_else(|| u.strip_prefix("http://"))
            .unwrap_or(u);
        let host = rest.split(['/', '?', '#']).next()?;
        let host = host.split('@').next_back()?; // strip userinfo
        let host = host.split(':').next()?; // strip port
        let host = host.trim_end_matches('.').to_lowercase();
        if host.is_empty() {
            None
        } else {
            Some(host)
        }
    }

    /// Classifies a source-policy violation with the exact constraint that
    /// failed (INV-30-20: exclusions are not "missing required domain").
    pub fn violation(&self, url: &str) -> Option<SourceViolation> {
        let domain = Self::domain_of(url)?;
        if self
            .excluded_domains
            .iter()
            .any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
        {
            return Some(SourceViolation::ExcludedDomain);
        }
        if !self.required_domains.is_empty()
            && !self
                .required_domains
                .iter()
                .any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
        {
            return Some(SourceViolation::RequiredDomainMissing);
        }
        None
    }

    /// Whether a URL satisfies required/excluded domain constraints.
    pub fn allows(&self, url: &str) -> bool {
        self.violation(url).is_none()
    }

    /// Merges request constraints into a policy copy.
    pub fn with_constraints(mut self, required: &[String], excluded: &[String]) -> Self {
        for d in required {
            if !self.required_domains.contains(d) {
                self.required_domains.push(d.clone());
            }
        }
        for d in excluded {
            if !self.excluded_domains.contains(d) {
                self.excluded_domains.push(d.clone());
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_extraction() {
        assert_eq!(
            SourcePolicy::domain_of("https://docs.rs/tokio/latest/x"),
            Some("docs.rs".into())
        );
        assert_eq!(
            SourcePolicy::domain_of("http://Example.COM:8080/a"),
            Some("example.com".into())
        );
        assert_eq!(SourcePolicy::domain_of("notaurl"), Some("notaurl".into()));
    }

    #[test]
    fn allow_and_exclude() {
        let p = SourcePolicy::default_policy()
            .with_constraints(&["github.com".to_string()], &["pinterest.com".to_string()]);
        assert!(p.allows("https://github.com/foo/bar"));
        assert!(p.allows("https://api.github.com/repos/foo/bar"));
        assert!(!p.allows("https://gitlab.com/foo/bar"));
        assert!(!p.allows("https://pinterest.com/pin/1"));
    }
}
