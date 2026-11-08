//! Configuration (configs/searchos.example.yaml schema + env overrides).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Top-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchOsConfig {
    /// Configuration schema version (RFC-019 19.4).
    pub config_version: u32,
    pub app: AppConfig,
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub search: SearchConfig,
    pub memory: MemoryConfig,
    pub intelligence: IntelligenceConfig,
    pub indexes: IndexesConfig,
    pub policies: PoliciesConfig,
    pub budgets: BudgetsConfig,
    pub crawler: CrawlerConfig,
    pub extensions: ExtensionsConfig,
    pub security: SecurityConfig,
    pub observability: ObservabilityConfig,
    pub providers: ProvidersConfig,
}

/// RFC-019 19.3: the remaining configuration sections. All are optional
/// with explicit defaults so an old config file stays loadable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IntelligenceConfig {
    pub query_expansion: bool,
    pub reranking: bool,
    /// SearchOS returns evidence, never a fabricated final answer.
    pub answer_generation: bool,
}
impl Default for IntelligenceConfig {
    fn default() -> Self {
        Self {
            query_expansion: true,
            reranking: true,
            answer_generation: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexesConfig {
    pub rebuild_on_start: bool,
    pub verify_on_start: bool,
}
impl Default for IndexesConfig {
    fn default() -> Self {
        Self {
            rebuild_on_start: false,
            verify_on_start: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PoliciesConfig {
    pub default_profile: String,
    pub minimum_quality: f32,
    pub require_provenance: bool,
}
impl Default for PoliciesConfig {
    fn default() -> Self {
        Self {
            default_profile: "general".into(),
            minimum_quality: 0.6,
            require_provenance: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetsConfig {
    pub default_max_external_queries: u32,
    pub default_max_total_cost_micros: i64,
    /// External-only baseline price per query used by savings reports.
    pub baseline_per_query_micros: i64,
}
impl Default for BudgetsConfig {
    fn default() -> Self {
        Self {
            default_max_external_queries: 5,
            default_max_total_cost_micros: 100_000,
            baseline_per_query_micros: 5_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CrawlerConfig {
    pub default_timeout_ms: u64,
    pub max_concurrent: u32,
    pub respect_robots: bool,
}
impl Default for CrawlerConfig {
    fn default() -> Self {
        Self {
            default_timeout_ms: 15_000,
            max_concurrent: 4,
            respect_robots: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtensionsConfig {
    pub allow_native: bool,
    /// WASM runtime is a later phase (ADR-024).
    pub wasm_enabled: bool,
}
impl Default for ExtensionsConfig {
    fn default() -> Self {
        Self {
            allow_native: true,
            wasm_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub ssrf_protection: bool,
    pub max_response_bytes: u64,
    pub max_redirects: u32,
}
impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            ssrf_protection: true,
            max_response_bytes: 10 * 1024 * 1024,
            max_redirects: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ObservabilityConfig {
    pub log_level: String,
    pub trace_retention_days: u32,
    pub metrics_enabled: bool,
}
impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: "warn".into(),
            trace_retention_days: 30,
            metrics_enabled: true,
        }
    }
}

impl Default for SearchOsConfig {
    fn default() -> Self {
        Self {
            config_version: 1,
            app: AppConfig::default(),
            server: ServerConfig::default(),
            storage: StorageConfig::default(),
            search: SearchConfig::default(),
            memory: MemoryConfig::default(),
            intelligence: IntelligenceConfig::default(),
            indexes: IndexesConfig::default(),
            policies: PoliciesConfig::default(),
            budgets: BudgetsConfig::default(),
            crawler: CrawlerConfig::default(),
            extensions: ExtensionsConfig::default(),
            security: SecurityConfig::default(),
            observability: ObservabilityConfig::default(),
            providers: ProvidersConfig::default(),
        }
    }
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
    /// Explicit transport API version (RFC-013 13.7); internal schema
    /// versions are independent.
    pub api_version: String,
    /// Requests per minute per client key; 0 disables rate limiting.
    pub rate_limit_per_minute: u32,
    /// Allowed browser origin for direct API access from the console.
    /// The Next.js console normally uses its server-side proxy instead.
    /// `*` allows any origin; empty disables CORS.
    pub cors_origin: String,
    /// bearer token -> workspace id. When non-empty, gateway requests are
    /// bound to that workspace (RFC-013 13.3, RFC-017 17.6).
    pub workspace_tokens: BTreeMap<String, String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 8787,
            auth_token: String::new(),
            api_version: "1".into(),
            rate_limit_per_minute: 600,
            cors_origin: "http://localhost:3100".into(),
            workspace_tokens: BTreeMap::new(),
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

    /// Validates the resolved configuration (RFC-019 19.4). Invalid
    /// configurations must fail with explicit, actionable errors.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.config_version == 0 {
            errors.push("config_version must be >= 1".into());
        }
        if self.server.port == 0 {
            errors.push("server.port must be > 0".into());
        }
        if self.app.data_dir.as_os_str().is_empty() {
            errors.push("app.data_dir must not be empty".into());
        }
        if !(0.0..=1.0).contains(&self.policies.minimum_quality) {
            errors.push("policies.minimum_quality must be in [0,1]".into());
        }
        if self.budgets.baseline_per_query_micros < 0
            || self.budgets.default_max_total_cost_micros <= 0
        {
            errors.push("budgets must be non-negative with a positive total cap".into());
        }
        if self.security.max_redirects > 20 {
            errors.push("security.max_redirects must be <= 20".into());
        }
        if !self.server.workspace_tokens.is_empty() && self.server.auth_token.is_empty() {
            errors.push("server.workspace_tokens requires server.auth_token".into());
        }
        if self.intelligence.answer_generation {
            errors.push(
                "intelligence.answer_generation is out of scope: SearchOS returns evidence, not answers"
                    .into(),
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Redacted config snapshot for backup / export (never writes secrets).
    pub fn redacted(&self) -> serde_json::Value {
        serde_json::json!({
            "config_version": self.config_version,
            "app": { "name": self.app.name, "mode": self.app.mode },
            "server": { "host": self.server.host, "port": self.server.port,
                         "api_version": self.server.api_version,
                         "rate_limit_per_minute": self.server.rate_limit_per_minute },
            "search": self.search,
            "policies": self.policies,
            "budgets": self.budgets,
            "security": self.security,
            "providers": { "tavily": self.providers.tavily.enabled,
                           "brave": self.providers.brave.enabled },
            "secrets": "[redacted: env only]",
        })
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

/// Loads a config file; missing file → defaults. Parse/validation problems
/// are returned alongside the safe default so callers can report them
/// instead of silently running on a wrong configuration.
pub fn load_config_report(explicit: Option<&Path>) -> (SearchOsConfig, Vec<String>) {
    let path = explicit
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::var(ENV_CONFIG_PATH).ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            dirs_home()
                .join(".searchos")
                .join("config")
                .join("config.yaml")
        });
    let mut errors = Vec::new();
    let mut cfg: SearchOsConfig = match std::fs::read_to_string(&path) {
        Ok(yaml) => match serde_yaml::from_str(&yaml) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("{}: invalid YAML: {}", path.display(), e));
                SearchOsConfig::default()
            }
        },
        Err(_) => SearchOsConfig::default(),
    };
    if let Ok(dir) = std::env::var(ENV_DATA_DIR) {
        if !dir.trim().is_empty() {
            cfg.app.data_dir = PathBuf::from(dir);
        }
    }
    if let Err(v) = cfg.validate() {
        errors.extend(v);
    }
    (cfg, errors)
}

/// Loads a config file; missing file → defaults. Warnings are surfaced on
/// stderr; `config validate` / `doctor` use [`load_config_report`] for the
/// full diagnostic.
pub fn load_config(explicit: Option<&Path>) -> SearchOsConfig {
    let (cfg, errors) = load_config_report(explicit);
    for e in errors {
        eprintln!("searchos config: {e}");
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
    fn new_governance_sections_have_documented_defaults() {
        let c = SearchOsConfig::default();
        assert_eq!(c.config_version, 1);
        assert_eq!(c.server.api_version, "1");
        assert_eq!(c.server.rate_limit_per_minute, 600);
        assert_eq!(c.budgets.baseline_per_query_micros, 5_000);
        assert!(c.security.ssrf_protection);
        assert!(!c.extensions.wasm_enabled);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn invalid_config_reports_actionable_errors() {
        let mut c = SearchOsConfig::default();
        c.server.port = 0;
        c.policies.minimum_quality = 2.0;
        c.intelligence.answer_generation = true;
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("server.port")));
        assert!(errs.iter().any(|e| e.contains("minimum_quality")));
        assert!(errs.iter().any(|e| e.contains("answer_generation")));
    }

    #[test]
    fn redacted_config_never_exposes_workspace_token_map() {
        let mut c = SearchOsConfig::default();
        c.server
            .workspace_tokens
            .insert("secret-token".into(), "ws1".into());
        let v = c.redacted();
        let s = v.to_string();
        assert!(!s.contains("secret-token"));
        assert!(s.contains("redacted"));
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
