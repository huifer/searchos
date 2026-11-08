//! Versioned search profiles (RFC-003 3.5, ADR-020).

use searchos_domain::request::{FreshnessRequirement, SearchMode};
use serde::{Deserialize, Serialize};

/// Source preference of a profile (RFC-003 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePreference {
    Trusted,
    OfficialPreferred,
    MultipleSources,
    ExactSource,
    LocalOnly,
}

/// Coverage depth requirement (RFC-003 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageLevel {
    Exact,
    Normal,
    High,
    VeryHigh,
    PolicyDefined,
}

impl CoverageLevel {
    /// Numeric minimum coverage used by the quality gate.
    pub fn minimum_coverage(&self) -> f32 {
        match self {
            Self::Exact => 1.0,
            Self::Normal => 0.6,
            Self::High => 0.75,
            Self::VeryHigh => 0.9,
            Self::PolicyDefined => 0.6,
        }
    }
}

/// A versioned search profile (RFC-003 3.5 table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchProfile {
    pub id: String,
    pub name: String,
    /// Profile version recorded with every task (ADR-020).
    pub version: String,
    /// Default freshness requirement.
    pub freshness: FreshnessRequirement,
    pub source: SourcePreference,
    pub coverage: CoverageLevel,
    /// Default execution mode.
    pub default_mode: SearchMode,
    /// TTL category default used to judge local evidence freshness.
    pub content_category: crate::ContentCategory,
}

/// Registry of built-in + custom profiles with version lookup.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileRegistry {
    profiles: Vec<SearchProfile>,
}

impl ProfileRegistry {
    /// Registry containing the six built-in profiles (RFC-003 3.5).
    pub fn builtin() -> Self {
        let mk = |id: &str, freshness, source, coverage, mode, category| SearchProfile {
            id: id.into(),
            name: id.into(),
            version: crate::POLICY_SUITE_VERSION.into(),
            freshness,
            source,
            coverage,
            default_mode: mode,
            content_category: category,
        };
        use CoverageLevel as C;
        use FreshnessRequirement as FR;
        use SearchMode as M;
        use SourcePreference as S;
        Self {
            profiles: vec![
                mk(
                    "general",
                    FR::Recent,
                    S::Trusted,
                    C::Normal,
                    M::LocalFirst,
                    crate::ContentCategory::StaticConcept,
                ),
                mk(
                    "technical",
                    FR::Recent,
                    S::OfficialPreferred,
                    C::High,
                    M::LocalFirst,
                    crate::ContentCategory::OfficialDocs,
                ),
                mk(
                    "latest",
                    FR::Strict,
                    S::OfficialPreferred,
                    C::High,
                    M::ExternalFirst,
                    crate::ContentCategory::GithubRelease,
                ),
                mk(
                    "research",
                    FR::Strict,
                    S::MultipleSources,
                    C::VeryHigh,
                    M::Hybrid,
                    crate::ContentCategory::StaticConcept,
                ),
                // "Policy-defined" freshness: the exact-source constraint governs (RFC-003 3.5).
                mk(
                    "exact",
                    FR::Any,
                    S::ExactSource,
                    C::Exact,
                    M::LocalFirst,
                    crate::ContentCategory::OfficialDocs,
                ),
                mk(
                    "offline",
                    FR::Any,
                    S::LocalOnly,
                    C::PolicyDefined,
                    M::LocalOnly,
                    crate::ContentCategory::StaticConcept,
                ),
            ],
        }
    }

    /// Looks a profile up by id.
    pub fn get(&self, id: &str) -> Option<&SearchProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// Adds or replaces a custom profile.
    pub fn upsert(&mut self, profile: SearchProfile) {
        if let Some(existing) = self.profiles.iter_mut().find(|p| p.id == profile.id) {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    /// All registered profiles.
    pub fn all(&self) -> &[SearchProfile] {
        &self.profiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_profiles_match_rfc_table() {
        let r = ProfileRegistry::builtin();
        assert_eq!(r.all().len(), 6);
        let latest = r.get("latest").unwrap();
        assert_eq!(latest.default_mode, SearchMode::ExternalFirst);
        assert_eq!(latest.freshness, FreshnessRequirement::Strict);
        let offline = r.get("offline").unwrap();
        assert_eq!(offline.default_mode, SearchMode::LocalOnly);
        let general = r.get("general").unwrap();
        assert_eq!(general.default_mode, SearchMode::LocalFirst);
    }

    #[test]
    fn coverage_minimums_ordered() {
        assert!(CoverageLevel::Normal.minimum_coverage() < CoverageLevel::High.minimum_coverage());
        assert!(
            CoverageLevel::High.minimum_coverage() < CoverageLevel::VeryHigh.minimum_coverage()
        );
    }
}
