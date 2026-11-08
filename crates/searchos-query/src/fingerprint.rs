//! Query fingerprint (RFC-003 3.4).
//!
//! ```text
//! fingerprint = SHA256(
//!   normalized_query + language + profile_version + freshness_policy +
//!   source_policy + workspace_scope
//! )
//! ```
//!
//! Requests with different quality constraints must never merge into the
//! same cache entry. Fields are joined length-prefixed to avoid ambiguity.

use sha2::{Digest, Sha256};

/// Inputs used to compute the fingerprint (kept for trace records).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueryFingerprintParts {
    pub normalized_query: String,
    pub language: String,
    pub profile_version: String,
    pub freshness_policy: String,
    pub source_policy: String,
    pub workspace_scope: String,
}

impl QueryFingerprintParts {
    /// Serializes fields unambiguously (length-prefixed).
    fn encoded(&self) -> String {
        let f = |s: &str| format!("{:06}{}", s.len(), s);
        format!(
            "{}{}{}{}{}{}",
            f(&self.normalized_query),
            f(&self.language),
            f(&self.profile_version),
            f(&self.freshness_policy),
            f(&self.source_policy),
            f(&self.workspace_scope),
        )
    }
}

/// Computes the hex SHA-256 fingerprint of a query + constraint set.
pub fn fingerprint(parts: &QueryFingerprintParts) -> String {
    let mut hasher = Sha256::new();
    hasher.update(parts.encoded().as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(q: &str, lang: &str, fresh: &str, ws: &str) -> QueryFingerprintParts {
        QueryFingerprintParts {
            normalized_query: q.into(),
            language: lang.into(),
            profile_version: "1.0.0".into(),
            freshness_policy: fresh.into(),
            source_policy: "trusted".into(),
            workspace_scope: ws.into(),
        }
    }

    #[test]
    fn same_inputs_same_fingerprint() {
        let a = fingerprint(&parts("rust tokio", "en", "recent", "ws_1"));
        let b = fingerprint(&parts("rust tokio", "en", "recent", "ws_1"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn different_freshness_different_fingerprint() {
        // 不能把不同质量约束的请求合并为同一缓存 (RFC-003 3.4).
        let a = fingerprint(&parts("rust tokio", "en", "recent", "ws_1"));
        let b = fingerprint(&parts("rust tokio", "en", "strict", "ws_1"));
        assert_ne!(a, b);
    }

    #[test]
    fn different_workspace_different_fingerprint() {
        let a = fingerprint(&parts("rust tokio", "en", "recent", "ws_1"));
        let b = fingerprint(&parts("rust tokio", "en", "recent", "ws_2"));
        assert_ne!(a, b);
    }

    #[test]
    fn length_prefix_avoids_ambiguity() {
        let a = fingerprint(&parts("ab", "c", "", ""));
        let b = fingerprint(&parts("a", "bc", "", ""));
        assert_ne!(a, b);
    }
}
