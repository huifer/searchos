//! Version diff decision (RFC-006 6.4).
//!
//! 内容 Hash 不变: no new version, no re-chunking, no re-embedding, no index
//! write — only fetch-metadata updates.
//! 内容 Hash 变化: new version, new chunks, new index events, keep old
//! version, move the current-version pointer.

use searchos_domain::document::VersionDiff;

/// Decides what to do with freshly fetched content vs the current version.
pub fn decide_version_diff(
    current_content_hash: Option<&str>,
    new_content_hash: &str,
) -> VersionDiff {
    match current_content_hash {
        Some(h) if h == new_content_hash => VersionDiff::Unchanged,
        _ => VersionDiff::NewVersion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use searchos_domain::document::VersionDiff as VD;

    #[test]
    fn unchanged_when_hashes_equal() {
        assert_eq!(decide_version_diff(Some("abc"), "abc"), VD::Unchanged);
    }

    #[test]
    fn new_version_when_different_or_missing() {
        assert_eq!(decide_version_diff(Some("abc"), "xyz"), VD::NewVersion);
        assert_eq!(decide_version_diff(None, "abc"), VD::NewVersion);
    }
}
