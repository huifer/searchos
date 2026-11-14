//! Evidence model and provenance (RFC-005 5.2, ADR-021).

use crate::ids::{DocumentVersionId, EvidenceId, SourceId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How reliable a source is considered (RFC-005 5.3 Source Reliability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReliabilityLevel {
    Unverified,
    Community,
    Professional,
    Official,
    Primary,
}

impl ReliabilityLevel {
    /// Numeric score in `[0, 1]` used by the quality scorer.
    pub fn score(&self) -> f32 {
        match self {
            Self::Unverified => 0.2,
            Self::Community => 0.4,
            Self::Professional => 0.7,
            Self::Official => 0.9,
            Self::Primary => 1.0,
        }
    }

    /// Parses a reliability level from its snake_case name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "unverified" => Some(Self::Unverified),
            "community" => Some(Self::Community),
            "professional" => Some(Self::Professional),
            "official" => Some(Self::Official),
            "primary" => Some(Self::Primary),
            _ => None,
        }
    }
}

/// Where an evidence item came from (ADR-021 Evidence Provenance).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Provenance {
    pub origin: ProvenanceOrigin,
    /// Provider id when externally retrieved (e.g. `tavily`).
    pub provider_id: Option<String>,
    /// Remote URL when externally retrieved.
    pub source_url: Option<String>,
    /// Local document id when retrieved from local knowledge.
    pub document_id: Option<crate::DocumentId>,
    /// Whether the content was retrieved locally (in-process) or externally.
    pub retrieved_at: DateTime<Utc>,
}

/// Origin of evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceOrigin {
    Local,
    External,
}

/// Normalized content fingerprint used for cross-source corroboration
/// (RFC-074).
///
/// Two texts produce the same fingerprint iff their whitespace-collapsed,
/// lowercased forms are identical — a deterministic, dependency-free FNV-1a
/// digest of the normalized text, not a cryptographic claim. `None` means the
/// text carries too little signal (under [`MIN_FINGERPRINT_CHARS`] characters
/// after normalization, e.g. a button label or a footer) and must be treated
/// as "not comparable", never as "matches".
///
/// This must never be used as a permission, integrity, or authenticity proof.
pub fn normalized_content_fingerprint(text: &str) -> Option<String> {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if normalized.chars().count() < MIN_FINGERPRINT_CHARS {
        return None;
    }
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in normalized.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Some(format!("fnv1a:{hash:016x}"))
}

/// Minimum normalized length before two identical texts are treated as the
/// same content. Below it, equality is too weak to collapse sources.
pub const MIN_FINGERPRINT_CHARS: usize = 40;

/// One piece of evidence supporting an answer (RFC-005 5.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: EvidenceId,
    pub source_id: SourceId,
    pub document_version_id: Option<DocumentVersionId>,

    pub title: Option<String>,
    pub url: String,

    /// Reference into content storage (e.g. `sha256:...`).
    pub content_ref: Option<String>,
    pub snippet: Option<String>,

    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,

    pub source_reliability: ReliabilityLevel,
    pub provenance: Provenance,
}

impl Evidence {
    /// Age of this evidence in seconds relative to now.
    pub fn age_seconds(&self) -> i64 {
        (Utc::now() - self.fetched_at).num_seconds().max(0)
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    #[test]
    fn identical_normalized_text_produces_the_same_fingerprint() {
        let a = "Tokio  is  the async runtime. It\tscales well.";
        let b = "tokio is the async runtime. it scales well.";
        assert_eq!(
            normalized_content_fingerprint(a),
            normalized_content_fingerprint(b)
        );
    }

    #[test]
    fn different_content_produces_different_fingerprints() {
        assert_ne!(
            normalized_content_fingerprint(
                "Tokio is the async runtime for Rust with work stealing",
            ),
            normalized_content_fingerprint(
                "Axum is the web framework for Rust built on tokio runtime",
            )
        );
    }

    #[test]
    fn short_texts_are_not_comparable() {
        assert_eq!(normalized_content_fingerprint("read more"), None);
        assert_eq!(normalized_content_fingerprint("  "), None);
    }

    #[test]
    fn cjk_passages_above_the_threshold_are_comparable() {
        let a = "SearchOS 是一个本地优先的检索基础设施，负责解析问题、检索本地知识与外部来源并评估证据质量。";
        let b = "SearchOS 是一个本地优先的检索基础设施，负责解析问题、检索本地知识与外部来源并评估证据质量。";
        assert_eq!(
            normalized_content_fingerprint(a),
            normalized_content_fingerprint(b)
        );
    }
}
