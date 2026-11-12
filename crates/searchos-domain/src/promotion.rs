//! External evidence promotion (RFC-031).
//!
//! Domain objects for the "buy once, reuse forever" loop: an external
//! acquisition (one provider call and the requirement it targeted), the
//! result items it returned, and the promotion state machine that decides
//! whether an item becomes durable knowledge.
//!
//! The invariant this module protects: **only evidence the final quality gate
//! accepted may become knowledge**. Everything else is recorded with a reason
//! and never reaches the index.

use crate::decision::GapResolutionStrategy;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Promotion state of one external result item (RFC-031 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromotionState {
    /// Recorded at acquisition; the final quality gate has not judged it yet.
    #[default]
    Pending,
    /// The final quality gate accepted the item; queued for capture.
    Accepted,
    /// The gate rejected it (unresolved conflict / failed hard constraint).
    Rejected,
    /// Capture succeeded: document + version + chunks + index entry exist.
    Promoted,
    /// Capture was not possible (robots / empty / budget / size / policy).
    Skipped,
}

impl PromotionState {
    /// Stable wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Promoted => "promoted",
            Self::Skipped => "skipped",
        }
    }

    /// Parses a stored state name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "promoted" => Some(Self::Promoted),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }

    /// Whether the item reached durable knowledge.
    pub fn is_promoted(&self) -> bool {
        matches!(self, Self::Promoted)
    }
}

/// Where an external query came from (RFC-031 5.2).
///
/// The plan and the ledger must agree: every provider call is attributable to
/// exactly one origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QueryOrigin {
    /// Built by the external planner for one open information requirement.
    Gap,
    /// Added by the query expander because the requirement stayed open.
    Expansion,
    /// The raw user query, used when no plan could be built.
    #[default]
    Fallback,
}

impl QueryOrigin {
    /// Stable wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::Expansion => "expansion",
            Self::Fallback => "fallback",
        }
    }

    /// Parses a stored origin name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gap" => Some(Self::Gap),
            "expansion" => Some(Self::Expansion),
            "fallback" => Some(Self::Fallback),
            _ => None,
        }
    }
}

/// Provenance class of a local candidate (RFC-031 5.4).
///
/// `ExternalSourced` candidates are legal evidence for a `local_only`
/// decision — that is the compound effect — but they must stay distinguishable
/// from knowledge the user owns, or the system reports savings it did not
/// earn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    /// The user's own material: local ingestion, workspace documents.
    #[default]
    FirstParty,
    /// A page this system captured after buying a search that pointed to it.
    ExternalSourced,
}

impl EvidenceOrigin {
    /// Stable wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FirstParty => "first_party",
            Self::ExternalSourced => "external_sourced",
        }
    }

    /// Parses a stored origin name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "first_party" => Some(Self::FirstParty),
            "external_sourced" => Some(Self::ExternalSourced),
            _ => None,
        }
    }
}

/// One external acquisition: a provider call (or a group of calls) and the
/// requirement it targeted (RFC-031 5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalAcquisition {
    pub id: String,
    pub task_id: String,
    pub request_id: String,
    pub workspace_id: String,
    pub provider_id: String,
    /// The exact query issued to the provider.
    pub query: String,
    pub query_origin: QueryOrigin,
    /// The open information requirement this purchase targeted, if any.
    #[serde(default)]
    pub requirement_id: Option<String>,
    #[serde(default)]
    pub strategy: GapResolutionStrategy,
    pub created_at: DateTime<Utc>,
}

/// One result item of an acquisition (RFC-031 5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub id: String,
    pub acquisition_id: String,
    pub url: String,
    pub canonical_url: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Provider relevance as reported by the adapter.
    #[serde(default)]
    pub provider_score: f32,
    /// Relevance as measured by the quality gate.
    #[serde(default)]
    pub relevance: f32,
    #[serde(default)]
    pub state: PromotionState,
    #[serde(default)]
    pub document_id: Option<String>,
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub chunk_count: Option<usize>,
    /// Why the item was rejected or skipped.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub promoted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_round_trip() {
        for s in [
            PromotionState::Pending,
            PromotionState::Accepted,
            PromotionState::Rejected,
            PromotionState::Promoted,
            PromotionState::Skipped,
        ] {
            assert_eq!(PromotionState::parse(s.as_str()), Some(s));
        }
        assert_eq!(PromotionState::parse("nope"), None);
    }

    #[test]
    fn origins_round_trip() {
        for o in [
            QueryOrigin::Gap,
            QueryOrigin::Expansion,
            QueryOrigin::Fallback,
        ] {
            assert_eq!(QueryOrigin::parse(o.as_str()), Some(o));
        }
        for o in [EvidenceOrigin::FirstParty, EvidenceOrigin::ExternalSourced] {
            assert_eq!(EvidenceOrigin::parse(o.as_str()), Some(o));
        }
    }

    #[test]
    fn default_state_is_pending() {
        assert_eq!(PromotionState::default(), PromotionState::Pending);
        assert!(!PromotionState::Pending.is_promoted());
        assert!(PromotionState::Promoted.is_promoted());
    }
}
