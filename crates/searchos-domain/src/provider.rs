//! Provider capability declaration (RFC-009 9.3).
//!
//! Capabilities must be declared by the actual adapter, never inferred from
//! the provider name.

use serde::{Deserialize, Serialize};

/// Capabilities an external provider adapter declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderCapabilities {
    pub search: bool,
    pub extract: bool,
    pub crawl: bool,
    pub map: bool,
    pub semantic_search: bool,
    pub supports_domain_filter: bool,
    pub supports_date_filter: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_no_capabilities() {
        let c = ProviderCapabilities::default();
        assert!(!c.search && !c.extract && !c.crawl);
    }
}
