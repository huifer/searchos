//! Versioned policy management state (RFC-034).
//!
//! The durable history lives in `policy_versions` (append-only, one active
//! row per kind); this module is the RUNTIME hot-reload surface: an
//! `Arc<PolicyState>` shared by the search service, the replay/simulation
//! service and the HTTP write surface. Each submit/rollback atomically
//! swaps ONE policy kind and bumps a stamp that is part of every cache key,
//! so old cached answers can never outlive the policy that produced them.

use searchos_domain::SearchOsError;
use searchos_policy::{FreshnessPolicy, QualityPolicy, SourcePolicy};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

/// The three policy kinds managed here (RFC-034).
pub const POLICY_KINDS: [&str; 3] = ["quality", "freshness", "source"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyKind {
    Quality,
    Freshness,
    Source,
}

impl PolicyKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "quality" => Some(Self::Quality),
            "freshness" => Some(Self::Freshness),
            "source" => Some(Self::Source),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Quality => "quality",
            Self::Freshness => "freshness",
            Self::Source => "source",
        }
    }
}

#[derive(Debug, Clone)]
struct Versioned<T> {
    version: i64,
    policy: T,
}

/// Thread-safe current policies + a monotonically increasing cache stamp.
pub struct PolicyState {
    quality: RwLock<Versioned<QualityPolicy>>,
    freshness: RwLock<Versioned<FreshnessPolicy>>,
    source: RwLock<Versioned<SourcePolicy>>,
    stamp: std::sync::atomic::AtomicU64,
}

impl PolicyState {
    pub fn new(quality: QualityPolicy, freshness: FreshnessPolicy, source: SourcePolicy) -> Self {
        Self {
            quality: RwLock::new(Versioned {
                version: 1,
                policy: quality,
            }),
            freshness: RwLock::new(Versioned {
                version: 1,
                policy: freshness,
            }),
            source: RwLock::new(Versioned {
                version: 1,
                policy: source,
            }),
            stamp: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn quality(&self) -> QualityPolicy {
        self.quality.read().unwrap().policy.clone()
    }
    pub fn freshness(&self) -> FreshnessPolicy {
        self.freshness.read().unwrap().policy.clone()
    }
    pub fn source(&self) -> SourcePolicy {
        self.source.read().unwrap().policy.clone()
    }

    /// Cache-key component: changes on every submit/rollback.
    pub fn stamp(&self) -> String {
        format!(
            "pol{}|q{}|f{}|s{}",
            self.stamp.load(std::sync::atomic::Ordering::SeqCst),
            self.quality.read().unwrap().version,
            self.freshness.read().unwrap().version,
            self.source.read().unwrap().version
        )
    }

    /// Active version numbers for diagnostics.
    pub fn versions(&self) -> serde_json::Value {
        serde_json::json!({
            "quality": self.quality.read().unwrap().version,
            "freshness": self.freshness.read().unwrap().version,
            "source": self.source.read().unwrap().version,
            "stamp": self.stamp(),
        })
    }

    /// Applies a new active version to ONE kind. The payload was already
    /// validated and durably committed by the caller.
    pub fn set(
        &self,
        kind: PolicyKind,
        version: i64,
        payload_json: &str,
    ) -> Result<(), SearchOsError> {
        let bump = || self.stamp.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match kind {
            PolicyKind::Quality => {
                let policy: QualityPolicy = serde_json::from_str(payload_json).map_err(|e| {
                    SearchOsError::Validation(validation_error("policy", &e.to_string()))
                })?;
                validate_quality(&policy)?;
                *self.quality.write().unwrap() = Versioned { version, policy };
            }
            PolicyKind::Freshness => {
                let policy: FreshnessPolicy = serde_json::from_str(payload_json).map_err(|e| {
                    SearchOsError::Validation(validation_error("policy", &e.to_string()))
                })?;
                *self.freshness.write().unwrap() = Versioned { version, policy };
            }
            PolicyKind::Source => {
                let policy: SourcePolicy = serde_json::from_str(payload_json).map_err(|e| {
                    SearchOsError::Validation(validation_error("policy", &e.to_string()))
                })?;
                *self.source.write().unwrap() = Versioned { version, policy };
            }
        }
        bump();
        Ok(())
    }
}

/// Validates a quality policy BEFORE it is persisted or activated.
pub fn validate_quality(policy: &QualityPolicy) -> Result<(), SearchOsError> {
    let fail = |field: &str, msg: &str| SearchOsError::Validation(validation_error(field, msg));
    if policy.version.trim().is_empty() {
        return Err(fail("version", "MUST_NOT_BE_EMPTY"));
    }
    if !(0.0..=1.0).contains(&policy.minimum_quality) {
        return Err(fail("minimum_quality", "MUST_BE_0_TO_1"));
    }
    if !(0.0..=1.0).contains(&policy.minimum_coverage) {
        return Err(fail("minimum_coverage", "MUST_BE_0_TO_1"));
    }
    if policy.minimum_distinct_sources > 32 {
        return Err(fail("minimum_distinct_sources", "MUST_BE_AT_MOST_32"));
    }
    let w = &policy.weights;
    if w.relevance < 0.0
        || w.freshness < 0.0
        || w.coverage < 0.0
        || w.source < 0.0
        || w.provenance < 0.0
    {
        return Err(fail("weights", "MUST_BE_NON_NEGATIVE"));
    }
    let total = w.relevance + w.freshness + w.coverage + w.source + w.provenance;
    if (total - 1.0).abs() > 0.01 {
        return Err(fail("weights", "MUST_SUM_TO_ONE"));
    }
    Ok(())
}

/// Parses + validates an incoming policy payload for one kind.
pub fn validate_payload(
    kind: PolicyKind,
    value: &serde_json::Value,
) -> Result<String, SearchOsError> {
    // Deserialize to the concrete type first: an unknown/mistyped field must
    // be a client error, never a silently ignored policy update.
    match kind {
        PolicyKind::Quality => {
            let p: QualityPolicy = serde_json::from_value(value.clone()).map_err(|e| {
                SearchOsError::Validation(validation_error("policy", &e.to_string()))
            })?;
            validate_quality(&p)?;
        }
        PolicyKind::Freshness => {
            let _: FreshnessPolicy = serde_json::from_value(value.clone()).map_err(|e| {
                SearchOsError::Validation(validation_error("policy", &e.to_string()))
            })?;
        }
        PolicyKind::Source => {
            let _: SourcePolicy = serde_json::from_value(value.clone()).map_err(|e| {
                SearchOsError::Validation(validation_error("policy", &e.to_string()))
            })?;
        }
    }
    Ok(value.to_string())
}

/// Shared handle type used by runtime + transports.
pub type SharedPolicyState = Arc<PolicyState>;

/// Builds an error for a validation field without depending on the ingest
/// service's private helper.
pub fn validation_error(field: &str, message: &str) -> searchos_domain::ValidationError {
    searchos_domain::ValidationError {
        field: field.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_validation_rejects_out_of_range_thresholds() {
        let bad = QualityPolicy {
            minimum_quality: 1.5,
            ..Default::default()
        };
        assert!(validate_quality(&bad).is_err());
        let good = QualityPolicy {
            minimum_quality: 0.8,
            ..Default::default()
        };
        assert!(validate_quality(&good).is_ok());
    }

    #[test]
    fn quality_validation_rejects_bad_weight_sum() {
        let mut value = serde_json::to_value(QualityPolicy::default()).unwrap();
        value["weights"]["relevance"] = serde_json::json!(99.0);
        let bad: QualityPolicy = serde_json::from_value(value).unwrap();
        assert!(validate_quality(&bad).is_err());
    }

    #[test]
    fn state_stamp_changes_on_every_set() {
        let state = PolicyState::new(
            QualityPolicy::default(),
            FreshnessPolicy::default_policy(),
            SourcePolicy::default_policy(),
        );
        let before = state.stamp();
        let p = serde_json::to_value(QualityPolicy::default()).unwrap();
        state.set(PolicyKind::Quality, 2, &p.to_string()).unwrap();
        assert_ne!(before, state.stamp());
        assert_eq!(state.quality().minimum_quality, 0.6);
    }

    #[test]
    fn set_rejects_malformed_payload_without_touching_the_stamp() {
        let state = PolicyState::new(
            QualityPolicy::default(),
            FreshnessPolicy::default_policy(),
            SourcePolicy::default_policy(),
        );
        let before = state.stamp();
        assert!(state.set(PolicyKind::Quality, 2, "{not json").is_err());
        assert_eq!(before, state.stamp());
    }
}
