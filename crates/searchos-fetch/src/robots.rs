//! robots.txt parsing and access decisions (RFC-010 10.5).

use std::collections::HashMap;
use std::sync::RwLock;

/// robots.txt verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RobotsDecision {
    Allowed,
    Disallowed {
        rule: String,
    },
    /// robots.txt unavailable or unreachable — treat as allowed (RFC 9309
    /// says unreachable disallows all; MVP treats transient failures as
    /// allowed but logs; a 4xx robots is "no robots" = allowed).
    Unknown,
}

/// One parsed robots.txt.
#[derive(Debug, Clone, Default)]
pub struct RobotsFile {
    /// user-agent (lowercased group) → disallow prefixes, allow prefixes.
    groups: HashMap<String, (Vec<String>, Vec<String>)>,
    /// `*` group fallback.
    star: Option<(Vec<String>, Vec<String>)>,
}

impl RobotsFile {
    /// Parses robots.txt content (minimal RFC 9309 subset: User-agent,
    /// Disallow, Allow; supports `*` wildcard at rule end).
    pub fn parse(content: &str) -> Self {
        let mut groups: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
        let mut current_agents: Vec<String> = Vec::new();
        for raw in content.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_lowercase();
            let value = value.trim().to_string();
            match key.as_str() {
                "user-agent" => {
                    let ua = value.to_lowercase();
                    if !current_agents.contains(&ua) {
                        current_agents.push(ua);
                    }
                }
                "disallow" => {
                    for ua in &current_agents {
                        groups.entry(ua.clone()).or_default().0.push(value.clone());
                    }
                }
                "allow" => {
                    for ua in &current_agents {
                        groups.entry(ua.clone()).or_default().1.push(value.clone());
                    }
                }
                _ => {}
            }
        }
        let star = groups.remove("*");
        Self { groups, star }
    }

    /// Whether `path` is allowed for user-agent `agent` (longest-match rule).
    pub fn allowed(&self, agent: &str, path: &str) -> RobotsDecision {
        let agent_l = agent.to_lowercase();
        let group = self.groups.get(&agent_l).or(self.star.as_ref()).or(None);
        let Some((disallows, allows)) = group else {
            return RobotsDecision::Allowed;
        };
        let longest_matching = |patterns: &[String]| -> Option<usize> {
            patterns
                .iter()
                .filter(|p| !p.is_empty() && path_matches(p, path))
                .map(|p| p.len())
                .max()
        };
        let d = longest_matching(disallows);
        let a = longest_matching(allows);
        match (d, a) {
            (Some(dl), Some(al)) if al >= dl => RobotsDecision::Allowed,
            (Some(dl), _) => RobotsDecision::Disallowed {
                rule: disallows
                    .iter()
                    .find(|p| p.len() == dl)
                    .cloned()
                    .unwrap_or_default(),
            },
            _ => RobotsDecision::Allowed,
        }
    }
}

/// RFC 9309 subset: `*` matches any sequence; `$` anchors the end.
fn path_matches(pattern: &str, path: &str) -> bool {
    let (pat, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let parts: Vec<&str> = pat.split('*').collect();
    let mut rest = path;
    // Leading literal part must prefix-match.
    let Some(first) = parts.first() else {
        return false;
    };
    if !rest.starts_with(first) {
        return false;
    }
    rest = &rest[first.len()..];
    // Middle literal parts must appear in order.
    for mid in &parts[1..parts.len().saturating_sub(1).max(1)] {
        if parts.len() == 1 {
            break;
        }
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    // Trailing literal part must suffix-match (or match rest).
    if let Some(last) = parts.last() {
        if parts.len() > 1 {
            if anchored {
                rest.ends_with(last)
            } else {
                rest.contains(last)
            }
        } else {
            // No wildcard: plain prefix (or exact when anchored).
            if anchored {
                rest.is_empty()
            } else {
                true
            }
        }
    } else {
        true
    }
}

/// Cached robots checker per host (RFC-014 reliability: bounded fetches).
#[derive(Debug, Default)]
pub struct RobotsChecker {
    cache: RwLock<HashMap<String, Option<RobotsFile>>>,
}

impl RobotsChecker {
    /// New empty checker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks a URL against the cached robots for its host.
    pub fn check_cached(&self, host: &str, path: &str, agent: &str) -> RobotsDecision {
        let cache = self.cache.read().unwrap();
        match cache.get(host) {
            Some(Some(f)) => f.allowed(agent, path),
            Some(None) => RobotsDecision::Unknown,
            None => RobotsDecision::Unknown,
        }
    }

    /// Installs robots content for a host (fetched by HttpFetcher).
    pub fn install(&self, host: &str, content: Option<&str>) {
        let parsed = content.map(RobotsFile::parse);
        self.cache.write().unwrap().insert(host.to_string(), parsed);
    }

    /// Whether robots for a host were fetched at least once.
    pub fn known(&self, host: &str) -> bool {
        self.cache.read().unwrap().contains_key(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROBOTS: &str = "User-agent: *\nDisallow: /private\nAllow: /private/public$\n\nUser-agent: searchos\nDisallow: /admin\nCrawl-delay: 1\n";

    #[test]
    fn star_group_rules() {
        let r = RobotsFile::parse(ROBOTS);
        assert_eq!(r.allowed("anybot", "/public/page"), RobotsDecision::Allowed);
        assert!(matches!(
            r.allowed("anybot", "/private/x"),
            RobotsDecision::Disallowed { .. }
        ));
        // Allow longer-match wins over Disallow.
        assert_eq!(
            r.allowed("anybot", "/private/public"),
            RobotsDecision::Allowed
        );
    }

    #[test]
    fn agent_specific_group_wins() {
        let r = RobotsFile::parse(ROBOTS);
        assert!(matches!(
            r.allowed("searchos", "/admin/login"),
            RobotsDecision::Disallowed { .. }
        ));
        // RFC 9309: the specific group replaces the star group entirely.
        assert_eq!(r.allowed("searchos", "/private/x"), RobotsDecision::Allowed);
    }

    #[test]
    fn wildcard_and_anchor() {
        let r = RobotsFile::parse("User-agent: *\nDisallow: /*.pdf$\n");
        assert!(matches!(
            r.allowed("x", "/docs/file.pdf"),
            RobotsDecision::Disallowed { .. }
        ));
        assert_eq!(
            r.allowed("x", "/docs/file.pdf.html"),
            RobotsDecision::Allowed
        );
    }

    #[test]
    fn empty_disallow_means_allow_all() {
        let r = RobotsFile::parse("User-agent: *\nDisallow:\n");
        assert_eq!(r.allowed("x", "/anything"), RobotsDecision::Allowed);
    }

    #[test]
    fn checker_cache() {
        let c = RobotsChecker::new();
        assert_eq!(
            c.check_cached("a.com", "/x", "bot"),
            RobotsDecision::Unknown
        );
        c.install("a.com", Some(ROBOTS));
        assert!(matches!(
            c.check_cached("a.com", "/private", "bot"),
            RobotsDecision::Disallowed { .. }
        ));
        assert!(c.known("a.com"));
        c.install("b.com", None);
        assert_eq!(
            c.check_cached("b.com", "/x", "bot"),
            RobotsDecision::Unknown
        );
    }
}
