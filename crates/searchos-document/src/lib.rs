//! Document lifecycle (RFC-006).
//!
//! - [`hash`] — SHA-256 content hashing, content-addressed refs.
//! - [`chunk`] — deterministic chunking with chunk hashes.
//! - [`lifecycle`] — version diff decisions (Unchanged / NewVersion).
//! - [`outbox`] — transactional outbox event types (ADR-011).

pub mod canonical;
pub mod chunk;
pub mod hash;
pub mod lifecycle;
pub mod outbox;

pub use canonical::{canonical_url, source_host};
pub use chunk::{chunk_text, ChunkingConfig};
pub use hash::{content_hash, content_ref, CONTENT_REF_PREFIX};
pub use lifecycle::decide_version_diff;
pub use outbox::{OutboxEntity, OutboxEvent, OutboxOperation};
pub use searchos_domain::document::VersionDiff;

/// Bump this when the extractor changes output shape so new versions are
/// written even for identical upstream content.
pub const EXTRACTOR_VERSION: &str = "extractor/1.0.0";
/// Default crawler version for the generic crawler.
pub const GENERIC_CRAWLER_VERSION: &str = "generic/1.0.0";
