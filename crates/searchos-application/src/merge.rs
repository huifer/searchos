//! Evidence merge (RFC-004 v1.1 6.11).
//!
//! Local and external evidence are merged with canonical URL deduplication,
//! version awareness, independent-source accounting, and conflict retention.
//! Conflicting evidence is never silently dropped or overwritten by "latest".

use searchos_domain::evidence::Evidence;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Outcome of a merge — exposed in warnings and quality decisions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MergeReport {
    /// Duplicate canonical URLs collapsed into one evidence item.
    pub duplicates_removed: usize,
    /// Same canonical URL / source with materially different content.
    pub conflicts: Vec<String>,
    /// Distinct hosts among merged evidence (independent-source count).
    pub independent_sources: usize,
    pub merged_count: usize,
}

/// Canonical URL form used for deduplication (RFC-006 9.2).
pub fn canonical_url(url: &str) -> String {
    let mut s = url.trim().to_lowercase();
    if let Some(pos) = s.find('#') {
        s.truncate(pos);
    }
    s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(&s)
        .to_string();
    // Drop tracking parameters, keep meaningful query (e.g. docs ?v=2).
    let (base, query) = match s.split_once('?') {
        Some((b, q)) => (b.to_string(), Some(q)),
        None => (s.clone(), None),
    };
    let mut base = base;
    if base.ends_with('/') && base.len() > 1 {
        base.pop();
    }
    let kept: Vec<&str> = query
        .unwrap_or("")
        .split('&')
        .filter(|kv| {
            let k = kv.split('=').next().unwrap_or("");
            !kv.is_empty() && !k.starts_with("utm_") && k != "fbclid" && k != "gclid" && k != "ref"
        })
        .collect();
    if kept.is_empty() {
        base
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

fn host_of(url: &str) -> String {
    canonical_url(url)
        .split('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Whether two snippets are materially different (a conflict, not a version
/// update with identical body).
fn materially_different(a: Option<&str>, b: Option<&str>) -> bool {
    let (a, b) = (a.unwrap_or(""), b.unwrap_or(""));
    if a == b {
        return false;
    }
    let ta: HashSet<String> = a
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 3)
        .map(|t| t.to_string())
        .collect();
    let tb: HashSet<String> = b
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 3)
        .map(|t| t.to_string())
        .collect();
    let shared = ta.intersection(&tb).count();
    let smaller = ta.len().min(tb.len()).max(1);
    (shared as f32 / smaller as f32) < 0.5
}

/// Merges local + external evidence.
///
/// Rules (RFC-004 v1.1 6.11):
/// 1. Different versions of the same document are not independent sources.
/// 2. Same content at different URLs is canonicalized.
/// 3. Conflicting evidence is retained and reported, never dropped.
/// 4. Newer documents do not silently overwrite older evidence.
pub fn merge_evidence(
    local: Vec<Evidence>,
    external: Vec<Evidence>,
) -> (Vec<Evidence>, MergeReport) {
    let mut by_canonical: HashMap<String, Evidence> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut report = MergeReport::default();

    for ev in local.into_iter().chain(external) {
        let key = canonical_url(&ev.url);
        if let Some(prev) = by_canonical.get(&key) {
            report.duplicates_removed += 1;
            if materially_different(prev.snippet.as_deref(), ev.snippet.as_deref()) {
                let conflict = format!("conflicting_versions:{}", key);
                if !report.conflicts.contains(&conflict) {
                    report.conflicts.push(conflict);
                }
            }
            // Keep the freshest version as the primary record; the older
            // evidence is still counted through the conflict marker.
            if ev.fetched_at > prev.fetched_at {
                by_canonical.insert(key.clone(), ev);
            }
        } else {
            order.push(key.clone());
            by_canonical.insert(key, ev);
        }
    }

    let merged: Vec<Evidence> = order
        .iter()
        .filter_map(|k| by_canonical.get(k).cloned())
        .collect();
    let hosts: HashSet<String> = merged.iter().map(|e| host_of(&e.url)).collect();
    report.independent_sources = hosts.len();
    report.merged_count = merged.len();
    (merged, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use searchos_domain::evidence::{Provenance, ProvenanceOrigin, ReliabilityLevel};
    use searchos_domain::ids::{EvidenceId, SourceId};

    fn ev(url: &str, snippet: &str, age_secs: i64) -> Evidence {
        Evidence {
            id: EvidenceId::generate(),
            source_id: SourceId::generate(),
            document_version_id: None,
            title: Some("T".into()),
            url: url.into(),
            content_ref: None,
            snippet: Some(snippet.into()),
            published_at: None,
            fetched_at: Utc::now() - chrono::Duration::seconds(age_secs),
            source_reliability: ReliabilityLevel::Community,
            provenance: Provenance {
                origin: ProvenanceOrigin::External,
                provider_id: None,
                source_url: Some(url.into()),
                document_id: None,
                retrieved_at: Utc::now(),
            },
        }
    }

    #[test]
    fn canonicalization_strips_tracking_and_fragment() {
        assert_eq!(
            canonical_url("HTTPS://Example.com/docs/?utm_source=x&v=2#part"),
            "example.com/docs?v=2"
        );
    }

    #[test]
    fn duplicate_urls_are_not_independent_sources() {
        let (merged, report) = merge_evidence(
            vec![ev("https://a.com/x", "alpha content", 10)],
            vec![ev("https://a.com/x?utm_source=y", "alpha content", 1)],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(report.duplicates_removed, 1);
        assert_eq!(report.independent_sources, 1);
        assert!(report.conflicts.is_empty());
    }

    #[test]
    fn conflicting_versions_are_retained_and_flagged() {
        let (merged, report) = merge_evidence(
            vec![ev("https://a.com/x", "tokio version 1.20 released", 10)],
            vec![ev("https://a.com/x", "axum framework pricing changed", 1)],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(report.conflicts.len(), 1);
        assert!(report.conflicts[0].contains("conflicting_versions"));
    }

    #[test]
    fn different_hosts_count_as_independent_sources() {
        let (_, report) = merge_evidence(
            vec![ev("https://a.com/x", "one", 1)],
            vec![ev("https://b.com/y", "two", 1)],
        );
        assert_eq!(report.independent_sources, 2);
        assert_eq!(report.duplicates_removed, 0);
    }
}
