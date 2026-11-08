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
