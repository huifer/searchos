//! SearchOS policy layer (RFC-003 3.5, RFC-005 5.5, RFC-009 9.6).
//!
//! Contains:
//! - [`SearchProfile`] — versioned profile configurations (general /
//!   technical / latest / research / exact / offline).
//! - [`FreshnessPolicy`] — content-category TTL table (RFC-005 5.5).
//! - [`SourcePolicy`], [`QualityPolicy`] — hard quality constraints (P-001).
//! - [`BudgetEnforcer`] — pre-execution budget checks with reservations.

pub mod budget;
pub mod freshness;
pub mod profile;
pub mod quality;
pub mod source;

pub use budget::{BudgetCheck, BudgetEnforcer, BudgetReservation, BudgetViolation};
pub use freshness::{classify_content, ContentCategory, FreshnessPolicy};
pub use profile::{ProfileRegistry, SearchProfile};
pub use quality::QualityPolicy;
pub use source::SourcePolicy;

/// Current suite version stamped into policies and profiles.
pub const POLICY_SUITE_VERSION: &str = "1.0.0";

/// Freshness policy table version (invalidates caches on change).
pub const FRESHNESS_POLICY_VERSION: &str = "1";
/// Source policy table version (invalidates caches on change).
pub const SOURCE_POLICY_VERSION: &str = "1";
