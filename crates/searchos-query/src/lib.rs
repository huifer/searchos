//! Query intelligence (RFC-003).
//!
//! Pipeline: Raw Query → Language Detection → Normalization → Entity
//! Extraction → Intent Classification → Temporal Sensitivity → Source
//! Requirement → Query Fingerprint → Search Profile Resolution.

pub mod expansion;
pub mod fingerprint;
pub mod intent;
pub mod language;
pub mod normalize;

pub use expansion::{ExpansionConfig, QueryExpander};
pub use fingerprint::{fingerprint, QueryFingerprintParts};
pub use intent::{
    classify_intents, detect_source_requirement, detect_temporal_sensitivity, SourceRequirement,
};
pub use language::detect_language;
pub use normalize::normalize_query;

use searchos_domain::request::SearchIntent;
use serde::{Deserialize, Serialize};

/// Output of the query processing pipeline for one raw query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryAnalysis {
    /// Raw query as submitted.
    pub raw: String,
    /// Normalized query text.
    pub normalized: String,
    /// Detected language (BCP-47-ish: `en`, `zh`, ...).
    pub language: String,
    /// Multi-label intents (RFC-003 3.3: intents must not be forced to one).
    pub intents: Vec<SearchIntent>,
    /// Temporal sensitivity verdict.
    pub temporal_sensitivity: TemporalSensitivity,
    /// Source requirement extracted from the query (RFC-004 Rule 2).
    pub source_requirement: SourceRequirement,
    /// Extracted named entities (best-effort, rule-based).
    pub entities: Vec<String>,
}

/// How time-sensitive the query is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalSensitivity {
    /// Latest / today / current / live / real-time.
    Strict,
    /// Wants recent-ish info but tolerant.
    Recent,
    /// Time-insensitive.
    Static,
}

/// Runs the full query analysis pipeline.
pub fn analyze(raw: &str) -> QueryAnalysis {
    let normalized = normalize_query(raw);
    let language = detect_language(raw);
    let intents = classify_intents(raw, &normalized);
    let temporal_sensitivity = detect_temporal_sensitivity(raw, &intents);
    let source_requirement = detect_source_requirement(raw);
    let entities = intent::extract_entities(raw);
    QueryAnalysis {
        raw: raw.to_string(),
        normalized,
        language,
        intents,
        temporal_sensitivity,
        source_requirement,
        entities,
    }
}
