//! Extension manifest schema (RFC-011 11.1, 11.5).

use serde::{Deserialize, Serialize};

/// Extension types (RFC-011 11.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionType {
    SearchProvider,
    FetchProvider,
    SiteCrawler,
    Extractor,
    Embedding,
    Reranker,
    Memory,
    Intelligence,
}

impl ExtensionType {
    /// Snake-case name for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SearchProvider => "search_provider",
            Self::FetchProvider => "fetch_provider",
            Self::SiteCrawler => "site_crawler",
            Self::Extractor => "extractor",
            Self::Embedding => "embedding",
            Self::Reranker => "reranker",
            Self::Memory => "memory",
            Self::Intelligence => "intelligence",
        }
    }

    /// Parses from the storage name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "search_provider" => Self::SearchProvider,
            "fetch_provider" => Self::FetchProvider,
            "site_crawler" => Self::SiteCrawler,
            "extractor" => Self::Extractor,
            "embedding" => Self::Embedding,
            "reranker" => Self::Reranker,
            "memory" => Self::Memory,
            "intelligence" => Self::Intelligence,
            _ => return None,
        })
    }
}

/// Runtime kinds (RFC-011 11.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    Declarative,
    Native,
    Wasm,
}

/// Explicit permission grants (RFC-011 11.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ExtensionPermissions {
    /// Explicitly allowed network hosts/domains.
    #[serde(default)]
    pub network: Vec<String>,
    /// File paths the extension may touch (WASM only in later phases).
    #[serde(default)]
    pub filesystem: Vec<String>,
    /// Execution budget per task in ms.
    #[serde(default)]
    pub budget_timeout_ms: Option<u64>,
}

/// Extension manifest (YAML, one per extension directory).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    /// Globally unique id, e.g. `ext.tavily`.
    pub id: String,
    pub name: String,
    /// Semver.
    pub version: String,
    #[serde(rename = "type")]
    pub kind: ExtensionType,
    /// Runtime: declarative | native | wasm.
    pub runtime: RuntimeKind,
    /// Entry point: crate name for native, rule file for declarative,
    /// module for wasm.
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub permissions: ExtensionPermissions,
    /// Compatibility: minimum core ABI/host version.
    #[serde(default)]
    pub requires_core: Option<String>,
    #[serde(default)]
    pub healthcheck: Option<bool>,
}

impl ExtensionManifest {
    /// Parses from YAML.
    pub fn from_yaml(yaml: &str) -> Result<Self, String> {
        serde_yaml::from_str(yaml).map_err(|e| format!("extension manifest: {e}"))
    }

    /// Full validation (RFC-011 11.3 Manifest Validation stage).
    pub fn validate(&self) -> Result<(), String> {
        if !self.id.starts_with("ext.") {
            return Err("id must start with 'ext.'".into());
        }
        if self.version.split('.').count() != 3 {
            return Err("version must be semver x.y.z".into());
        }
        if self.runtime == RuntimeKind::Wasm {
            // WASM sandbox requirements must be declared up-front.
            if self.permissions.budget_timeout_ms.is_none() {
                return Err("wasm extensions must declare a budget timeout".into());
            }
            if self.permissions.network.contains(&"*".to_string()) {
                return Err("wasm extensions cannot hold wildcard network access".into());
            }
        }
        if self.runtime == RuntimeKind::Declarative && self.entry.is_none() {
            return Err("declarative extensions must declare an entry rule file".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_provider_manifest() {
        let yaml = r#"
id: ext.tavily
name: Tavily Search
version: 1.0.0
type: search_provider
runtime: native
entry: searchos-provider::tavily
permissions:
  network:
    - api.tavily.com
healthcheck: true
"#;
        let m = ExtensionManifest::from_yaml(yaml).unwrap();
        m.validate().unwrap();
        assert_eq!(m.kind, ExtensionType::SearchProvider);
        assert_eq!(m.runtime, RuntimeKind::Native);
        assert_eq!(m.permissions.network, vec!["api.tavily.com".to_string()]);
    }

    #[test]
    fn wasm_requires_budget_and_no_wildcard() {
        let yaml = r#"
id: ext.thirdparty
name: Third Party
version: 0.1.0
type: extractor
runtime: wasm
entry: module.wasm
permissions:
  network: ["*"]
"#;
        let m = ExtensionManifest::from_yaml(yaml).unwrap();
        assert!(m.validate().is_err(), "wildcard network denied for wasm");
        let yaml2 = yaml.replace(
            "permissions:\n  network: [\"*\"]",
            "permissions:\n  network: [\"example.com\"]\n  budget_timeout_ms: 5000",
        );
        let m2 = ExtensionManifest::from_yaml(&yaml2).unwrap();
        m2.validate().unwrap();
    }

    #[test]
    fn declarative_needs_entry() {
        let yaml = r#"
id: ext.rules
name: Rules
version: 1.0.0
type: extractor
runtime: declarative
"#;
        let m = ExtensionManifest::from_yaml(yaml).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn type_roundtrip() {
        for t in [
            ExtensionType::Reranker,
            ExtensionType::Memory,
            ExtensionType::Intelligence,
        ] {
            assert_eq!(ExtensionType::parse(t.as_str()), Some(t));
        }
    }
}
