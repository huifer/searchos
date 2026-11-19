//! Evidence merge (RFC-004 v1.1 6.11).
//!
//! Local and external evidence are merged with canonical URL deduplication,
//! version awareness, independent-source accounting, and conflict retention.
//! Conflicting evidence is never silently dropped or overwritten by "latest".

use searchos_document::canonical::{canonical_url as canonical_document_url, source_host};
use searchos_domain::evidence::{normalized_content_fingerprint, Evidence, ProvenanceOrigin};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Outcome of a merge — exposed in warnings and quality decisions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MergeReport {
    /// Duplicate canonical URLs collapsed into one evidence item.
    pub duplicates_removed: usize,
    /// Same canonical URL / source with materially different content.
    pub conflicts: Vec<String>,
    /// RFC-059: contradiction graph edges from the older claim to the kept
    /// newer claim. Both sides remain auditable; the newer copy wins the
    /// primary evidence slot but the older claim is never erased.
    #[serde(default)]
    pub conflict_links: Vec<(String, String)>,
    /// Distinct hosts among merged evidence (independent-source count).
    pub independent_sources: usize,
    pub merged_count: usize,
    /// Evidence items that carried the SAME normalized content as an item on
    /// another canonical URL (a syndicated / re-published copy). RFC-074:
    /// these are occurrences, not independent corroboration.
    #[serde(default)]
    pub syndicated_copies: usize,
    /// RFC-038: same-canonical folds observed during dedup — the removed
    /// (raw) URL and the canonical URL that KEPT the evidence. `duplicate_of`
    /// edges in the evidence graph are built from these.
    #[serde(default)]
    pub duplicate_links: Vec<(String, String)>,
    /// Distinct content communities: `merged_count - syndicated_copies`.
    #[serde(default)]
    pub independent_content_sources: usize,
    /// One entry per detected syndication set (≥ 2 URLs, same content), with
    /// the identified original publisher.
    #[serde(default)]
    pub content_groups: Vec<ContentGroup>,
}

/// One set of evidence items carrying the same normalized content on
/// different canonical URLs (RFC-074).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentGroup {
    /// Canonical URL of the identified original publisher.
    pub original_url: String,
    /// Canonical URLs of the copies, first-seen order.
    pub copies: Vec<String>,
    /// Total occurrences (original + copies).
    pub occurrences: usize,
}

/// Canonical URL form used for deduplication (Part 30 INV-30-15).
///
/// Fetch, ingest and index writes call the same `searchos-document`
/// implementation; the raw URL stays on the evidence item for display.
pub fn canonical_url(url: &str) -> String {
    canonical_document_url(url)
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
                if ev.fetched_at > prev.fetched_at {
                    report
                        .conflict_links
                        .push((prev.url.clone(), ev.url.clone()));
                } else {
                    report
                        .conflict_links
                        .push((ev.url.clone(), prev.url.clone()));
                }
            }
            // Keep the freshest version as the primary record; the older
            // evidence is still counted through the conflict marker. The
            // folded-away URL becomes a `duplicate_of` edge (RFC-038).
            if ev.fetched_at > prev.fetched_at {
                report
                    .duplicate_links
                    .push((prev.url.clone(), ev.url.clone()));
                by_canonical.insert(key.clone(), ev);
            } else {
                report
                    .duplicate_links
                    .push((ev.url.clone(), prev.url.clone()));
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
    let hosts: HashSet<String> = merged.iter().map(|e| source_host(&e.url)).collect();
    report.independent_sources = hosts.len();
    report.merged_count = merged.len();

    // RFC-074: cross-URL same-content detection. Items are grouped by their
    // normalized content fingerprint; a group with more than one URL is a
    // syndication set with an identifiable original publisher. The merged
    // list itself is never mutated or reordered — this is corroboration
    // accounting, not content replacement.
    let mut by_fingerprint: HashMap<String, Vec<&Evidence>> = HashMap::new();
    let mut fingerprint_order: Vec<String> = Vec::new();
    for ev in &merged {
        let Some(fingerprint) = ev
            .snippet
            .as_deref()
            .and_then(normalized_content_fingerprint)
        else {
            continue;
        };
        if !by_fingerprint.contains_key(&fingerprint) {
            fingerprint_order.push(fingerprint.clone());
        }
        by_fingerprint.entry(fingerprint).or_default().push(ev);
    }
    for fingerprint in fingerprint_order {
        let items = &by_fingerprint[&fingerprint];
        if items.len() < 2 {
            continue;
        }
        let original = items
            .iter()
            .enumerate()
            .min_by_key(|(first_seen, ev)| originality_key(ev, *first_seen))
            .map(|(_, ev)| *ev)
            .expect("a content group has at least one item");
        let copies: Vec<String> = items
            .iter()
            .filter(|ev| ev.id != original.id)
            .map(|ev| canonical_url(&ev.url))
            .collect();
        report.syndicated_copies += copies.len();
        report.content_groups.push(ContentGroup {
            original_url: canonical_url(&original.url),
            copies,
            occurrences: items.len(),
        });
    }
    report.independent_content_sources = merged.len().saturating_sub(report.syndicated_copies);
    (merged, report)
}

/// Builds the RFC-038 evidence graph from what the merge actually observed:
/// canonical folds (`duplicate_of`: a raw variant URL points at its
/// canonical form) and cross-URL same-content groups (`same_content`,
/// original → copy). Nodes are the URLs that appear as endpoints; an empty
/// edge set means there was nothing to relate.
pub fn build_evidence_graph(
    merged: &[Evidence],
    report: &MergeReport,
) -> searchos_domain::response::EvidenceGraphBlock {
    use searchos_domain::response::{EvidenceGraphEdge, EvidenceRelation};
    let mut edges: Vec<EvidenceGraphEdge> = Vec::new();
    let push = |edges: &mut Vec<EvidenceGraphEdge>, from: String, to: String, relation| {
        if from != to && !edges.iter().any(|e| e.from == from && e.to == to) {
            edges.push(EvidenceGraphEdge { from, to, relation });
        }
    };
    // Surviving evidence whose raw URL is a VARIANT of its canonical form
    // (tracking parameters, fragments, case) is a duplicate_of edge.
    for ev in merged {
        let canonical = canonical_url(&ev.url);
        push(
            &mut edges,
            ev.url.clone(),
            canonical,
            EvidenceRelation::DuplicateOf,
        );
    }
    // Folded-away URLs still point at the evidence that kept the content.
    for (removed, kept) in &report.duplicate_links {
        let kept_canonical = canonical_url(kept);
        let removed_canonical = canonical_url(removed);
        push(
            &mut edges,
            removed_canonical,
            kept_canonical.clone(),
            EvidenceRelation::DuplicateOf,
        );
        push(
            &mut edges,
            removed.clone(),
            kept_canonical,
            EvidenceRelation::DuplicateOf,
        );
    }
    for group in &report.content_groups {
        for copy in &group.copies {
            push(
                &mut edges,
                copy.clone(),
                group.original_url.clone(),
                EvidenceRelation::SameContent,
            );
        }
    }
    // RFC-059: retained conflicts are graph edges, not just opaque warning
    // strings. Older → newer is deterministic and mirrors the merge choice.
    for (older, newer) in &report.conflict_links {
        let older_canonical = canonical_url(older);
        let newer_canonical = canonical_url(newer);
        push(
            &mut edges,
            older_canonical,
            newer_canonical.clone(),
            EvidenceRelation::Contradicts,
        );
        push(
            &mut edges,
            older.clone(),
            newer.clone(),
            EvidenceRelation::Contradicts,
        );
    }
    let mut nodes: Vec<String> = Vec::new();
    for edge in &edges {
        if !nodes.contains(&edge.from) {
            nodes.push(edge.from.clone());
        }
        if !nodes.contains(&edge.to) {
            nodes.push(edge.to.clone());
        }
    }
    searchos_domain::response::EvidenceGraphBlock { nodes, edges }
}

/// Primacy ordering used to pick the ORIGINAL publisher inside a content
/// group (RFC-074):
/// 1. Local (workspace-owned) knowledge is authoritative (RFC-029).
/// 2. Then the earliest publication date; a dated item beats an undated one.
/// 3. Then first-seen order (procurement order), so the pick is deterministic
///    and never decided by URL spelling.
fn originality_key(ev: &Evidence, first_seen: usize) -> (u8, bool, i64, usize) {
    let origin_rank = match ev.provenance.origin {
        ProvenanceOrigin::Local => 0u8,
        ProvenanceOrigin::External => 1,
    };
    let published = ev.published_at.map(|t| t.timestamp());
    (
        origin_rank,
        published.is_none(),
        published.unwrap_or(0),
        first_seen,
    )
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
            "https://example.com/docs?v=2"
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
        // RFC-059: the retained conflict is a graph edge, older → newer.
        assert_eq!(
            report.conflict_links,
            vec![("https://a.com/x".to_string(), "https://a.com/x".to_string())]
        );
    }

    #[test]
    fn contradiction_graph_retains_both_sides_of_a_conflict() {
        let (merged, report) = merge_evidence(
            vec![ev(
                "https://a.com/x?utm_source=old",
                "tokio version 1.20 released",
                10,
            )],
            vec![ev(
                "https://a.com/x?utm_source=new",
                "axum framework pricing changed",
                1,
            )],
        );
        assert_eq!(report.conflicts.len(), 1);
        let graph = build_evidence_graph(&merged, &report);
        assert!(graph
            .nodes
            .iter()
            .any(|n| n == "https://a.com/x?utm_source=old"));
        assert!(graph
            .nodes
            .iter()
            .any(|n| n == "https://a.com/x?utm_source=new"));
        assert!(graph.edges.iter().any(|e| {
            e.relation == searchos_domain::response::EvidenceRelation::Contradicts
                && e.from == "https://a.com/x?utm_source=old"
                && e.to == "https://a.com/x?utm_source=new"
        }));
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

    #[test]
    fn same_content_on_different_hosts_is_one_source_with_copies() {
        let article = "Tokio 1.39 shipped the new work-stealing scheduler with a redesigned \n        task budget, improving tail latency for high-fanout workloads.";
        let (merged, report) = merge_evidence(
            vec![ev("https://tokio.rs/blog/scheduler", article, 10)],
            vec![ev("https://news.example.com/repost", article, 1)],
        );
        // The evidence list itself is untouched (accounting, not removal).
        assert_eq!(merged.len(), 2);
        assert_eq!(report.independent_sources, 2, "host count stays compatible");
        assert_eq!(report.syndicated_copies, 1);
        assert_eq!(report.independent_content_sources, 1);
        assert_eq!(report.content_groups.len(), 1);
        let group = &report.content_groups[0];
        assert_eq!(group.occurrences, 2);
        assert_eq!(group.original_url, "https://tokio.rs/blog/scheduler");
        assert_eq!(
            group.copies,
            vec!["https://news.example.com/repost".to_string()]
        );
    }

    #[test]
    fn original_publisher_prefers_local_then_earliest_publication() {
        let article = "The tokio scheduler redesign reduces tail latency under \n        high fanout by redesigning the task budget accounting model.";
        // Local (workspace-owned) wins even though it is undated and the
        // external repost carries an earlier date.
        let mut local = ev("https://docs.local/notes/scheduler", article, 30);
        local.provenance.origin = ProvenanceOrigin::Local;
        let mut earlier = ev("https://a.example.com/x", article, 20);
        earlier.published_at = Some(Utc::now() - chrono::Duration::days(30));
        let (_, report) = merge_evidence(vec![local], vec![earlier]);
        assert_eq!(report.content_groups.len(), 1);
        assert_eq!(
            report.content_groups[0].original_url, "https://docs.local/notes/scheduler",
            "local knowledge is authoritative over external reposts"
        );

        // Without local knowledge, the earliest publication date wins.
        let mut later = ev("https://late.example.com/y", article, 5);
        later.published_at = Some(Utc::now() - chrono::Duration::days(1));
        let mut earlier = ev("https://early.example.com/z", article, 6);
        earlier.published_at = Some(Utc::now() - chrono::Duration::days(2));
        let (_, report) = merge_evidence(vec![later], vec![earlier]);
        assert_eq!(
            report.content_groups[0].original_url, "https://early.example.com/z",
            "earlier publication identifies the original"
        );
    }

    #[test]
    fn evidence_graph_links_folds_and_syndication() {
        let article =
            "The federated planner records an auditable rejection chain for every source.";
        let (merged, report) = merge_evidence(
            vec![
                ev("https://a.dev/x", article, 10),
                ev("https://a.dev/x?utm_source=y", article, 1),
            ],
            vec![ev("https://mirror.dev/x", article, 1)],
        );
        let graph = build_evidence_graph(&merged, &report);
        assert!(
            graph
                .edges
                .iter()
                .any(|e| e.relation == searchos_domain::response::EvidenceRelation::DuplicateOf),
            "canonical folds become duplicate_of edges: {graph:?}"
        );
        assert!(
            graph
                .edges
                .iter()
                .any(|e| e.relation == searchos_domain::response::EvidenceRelation::SameContent),
            "syndication becomes same_content edges: {graph:?}"
        );
        assert!(graph.nodes.len() >= 2);
        // An unrelated single-source answer has nothing to graph.
        let (plain, plain_report) = merge_evidence(
            vec![ev(
                "https://a.dev/y",
                "a distinct snippet body that stands alone",
                1,
            )],
            Vec::new(),
        );
        assert!(build_evidence_graph(&plain, &plain_report).edges.is_empty());
    }

    #[test]
    fn short_or_distinct_content_is_not_grouped() {
        // A boilerplate label is below the fingerprint threshold: two hosts
        // carrying it are still independent sources.
        let (_, report) = merge_evidence(
            vec![ev("https://a.com/x", "read more", 1)],
            vec![ev("https://b.com/y", "read more", 1)],
        );
        assert_eq!(report.syndicated_copies, 0);
        assert_eq!(report.independent_content_sources, 2);
        assert!(report.content_groups.is_empty());
    }
}
