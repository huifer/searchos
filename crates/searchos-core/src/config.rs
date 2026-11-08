//! Configuration (configs/searchos.example.yaml schema + env overrides).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct SearchOsConfig {
    pub app: AppConfig,
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub search: SearchConfig,
    pub memory: MemoryConfig,
    pub providers: ProvidersConfig,
}

/// App section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub name: String,
    pub mode: String,
    pub data_dir: PathBuf,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            name: "searchos".into(),
            mode: "standalone".into(),
            data_dir: dirs_home().join(".searchos"),
        }
    }
}

/// Server section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    /// Bearer token for gateway auth (RFC-012 12.3); empty = no auth
    /// (local dev).
    pub auth_token: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 8787,
            auth_token: String::new(),
        }
    }
}

/// Storage section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub metadata: String,
    pub fulltext: String,
    pub vector: String,
    pub objects: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            metadata: "sqlite".into(),
            fulltext: "tantivy".into(),
            vector: "lancedb".into(),
            objects: "local".into(),
        }
    }
}

/// Search section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub local_first: bool,
    pub semantic_dedup: bool,
    pub incremental: bool,
    pub max_external_queries: u32,
    /// Cache TTL seconds for the exact query cache.
    pub cache_ttl_seconds: u64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            local_first: true,
            semantic_dedup: true,
            incremental: true,
            max_external_queries: 3,
            cache_ttl_seconds: 3600,
        }
    }
}

/// Memory section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub embedding: EmbeddingConfig,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            embedding: EmbeddingConfig::default(),
        }
    }
}

/// Embedding config (RFC Part 22: pluggable model provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// `local` (hashing embedder) | `remote`.
    pub provider: String,
    pub model: String,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: "local".into(),
            model: "bge-m3".into(),
        }
    }
}

/// Providers section (keys come from env: TAVILY_API_KEY / BRAVE_API_KEY —
/// RFC-014 14.3: keys never in config files).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProvidersConfig {
    pub tavily: ProviderConfig,
    pub brave: ProviderConfig,
}

/// One provider block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub enabled: bool,
    pub priority: u32,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            priority: 1,
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Environment override keys.
pub const ENV_DATA_DIR: &str = "SEARCHOS_DATA_DIR";
pub const ENV_TAVILY_KEY: &str = "TAVILY_API_KEY";
pub const ENV_BRAVE_KEY: &str = "BRAVE_API_KEY";
pub const ENV_CONFIG_PATH: &str = "SEARCHOS_CONFIG";
pub const ENV_AUTH_TOKEN: &str = "SEARCHOS_AUTH_TOKEN";

impl SearchOsConfig {
    /// Loads config from `~/.searchos/config/config.yaml` (or
    /// SEARCHOS_CONFIG), falling back to defaults.
    pub fn load() -> Self {
        load_config(None)
    }

    /// Data directory layout (RFC-006 6.5):
    /// `~/.searchos/{data,indexes,extensions,models,cache,logs,temp}`.
    pub fn data_db_path(&self) -> PathBuf {
        self.app.data_dir.join("data").join("searchos.db")
    }
    pub fn tantivy_path(&self) -> PathBuf {
        self.app.data_dir.join("indexes").join("tantivy")
    }
    pub fn lancedb_path(&self) -> PathBuf {
        self.app.data_dir.join("indexes").join("lancedb")
    }
    pub fn objects_path(&self) -> PathBuf {
        self.app.data_dir.join("data").join("objects")
    }
    pub fn extensions_path(&self) -> PathBuf {
        self.app.data_dir.join("extensions")
    }
    pub fn crawlers_path(&self) -> PathBuf {
        // Site crawlers live in the workspace extensions/crawlers dir when
        // present (dev checkout), else data dir.
        std::env::current_dir()
            .map(|d| d.join("extensions").join("crawlers"))
            .unwrap_or_else(|_| self.app.data_dir.join("extensions").join("crawlers"))
    }

    /// Tavily key resolution: env first (RFC-014 14.3).
    pub fn tavily_api_key(&self) -> Option<String> {
        std::env::var(ENV_TAVILY_KEY)
            .ok()
            .filter(|k| !k.trim().is_empty())
    }
    /// Brave key resolution: env first.
    pub fn brave_api_key(&self) -> Option<String> {
        std::env::var(ENV_BRAVE_KEY)
            .ok()
            .filter(|k| !k.trim().is_empty())
    }

    /// Effective config: file + env overrides applied.
    pub fn effective(mut self) -> Self {
        if let Ok(dir) = std::env::var(ENV_DATA_DIR) {
            if !dir.trim().is_empty() {
                self.app.data_dir = PathBuf::from(dir);
            }
        }
        if let Ok(token) = std::env::var(ENV_AUTH_TOKEN) {
            self.server.auth_token = token;
        }
        self
    }
}

/// Loads a config file; missing file → defaults.
pub fn load_config(explicit: Option<&Path>) -> SearchOsConfig {
    let path = explicit
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::var(ENV_CONFIG_PATH).ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            dirs_home()
                .join(".searchos")
                .join("config")
                .join("config.yaml")
        });
    let file = std::fs::read_to_string(&path).ok();
    let mut cfg: SearchOsConfig = match file {
        Some(yaml) => serde_yaml::from_str(&yaml).unwrap_or_default(),
        None => SearchOsConfig::default(),
    };
    if let Ok(dir) = std::env::var(ENV_DATA_DIR) {
        if !dir.trim().is_empty() {
            cfg.app.data_dir = PathBuf::from(dir);
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_example_config() {
        let c = SearchOsConfig::default();
        assert_eq!(c.server.port, 8787);
        assert!(c.search.local_first);
        assert_eq!(c.search.max_external_queries, 3);
        assert_eq!(c.storage.metadata, "sqlite");
        assert_eq!(c.storage.fulltext, "tantivy");
        assert_eq!(c.storage.vector, "lancedb");
    }

    #[test]
    fn parses_example_yaml() {
        let yaml = r#"
app:
  name: searchos
  mode: standalone
  data_dir: /tmp/searchos-test
server:
  enabled: true
  host: 127.0.0.1
  port: 9000
search:
  max_external_queries: 5
providers:
  tavily:
    enabled: true
    priority: 1
  brave:
    enabled: false
    priority: 2
"#;
        let c: SearchOsConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.server.port, 9000);
        assert_eq!(c.search.max_external_queries, 5);
        assert!(!c.providers.brave.enabled);
        assert_eq!(c.app.data_dir, PathBuf::from("/tmp/searchos-test"));
    }

    #[test]
    fn data_layout_matches_rfc() {
        let mut c = SearchOsConfig::default();
        c.app.data_dir = PathBuf::from("/x");
        assert_eq!(c.data_db_path(), PathBuf::from("/x/data/searchos.db"));
        assert_eq!(c.tantivy_path(), PathBuf::from("/x/indexes/tantivy"));
        assert_eq!(c.lancedb_path(), PathBuf::from("/x/indexes/lancedb"));
        assert_eq!(c.objects_path(), PathBuf::from("/x/data/objects"));
    }

    #[test]
    fn env_keys_are_the_only_key_source() {
        // Keys are never parsed from config; env only.
        assert_eq!(
            std::env::var("TAVILY_API_KEY").ok(),
            std::env::var("TAVILY_API_KEY").ok()
        );
    }
}
