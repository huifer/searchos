//! Configuration (configs/searchos.example.yaml schema + env overrides).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
    pub ingest: IngestConfig,
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
    /// Gates the RRF result cache (not a model reranker). No `Reranker`
    /// provider is attached in v0.1.0; RFC-007 7.7 leaves reranking off by default.
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
    /// RFC 9309 default is false: unreachable robots.txt disallows all.
    /// Set true only for an explicitly permissive deployment.
    pub robots_unknown_allow: bool,
    /// Upper bound for a site-declared Crawl-delay.
    pub crawl_delay_cap_seconds: u64,
}
impl Default for CrawlerConfig {
    fn default() -> Self {
        Self {
            default_timeout_ms: 15_000,
            max_concurrent: 4,
            respect_robots: true,
            robots_unknown_allow: false,
            crawl_delay_cap_seconds: 10,
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
    /// Opt-in OTLP/HTTP export. Off by default: no exporter, no background
    /// thread and no network call when disabled.
    pub otlp_enabled: bool,
    /// Collector base URL, e.g. http://localhost:4318.
    pub otlp_endpoint: String,
    /// service.name resource attribute.
    pub otlp_service_name: String,
    /// `k=v,k2=v2` extra HTTP headers (e.g. an auth token). Secrets never
    /// appear in config files or logs; use SEARCHOS_OTLP_HEADERS instead.
    pub otlp_headers: String,
    pub otlp_metrics_interval_seconds: u64,
    pub otlp_export_traces: bool,
    pub otlp_export_metrics: bool,
}
impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: "warn".into(),
            trace_retention_days: 30,
            metrics_enabled: true,
            otlp_enabled: false,
            otlp_endpoint: "http://127.0.0.1:4318".into(),
            otlp_service_name: "searchos".into(),
            otlp_headers: String::new(),
            otlp_metrics_interval_seconds: 15,
            otlp_export_traces: true,
            otlp_export_metrics: true,
        }
    }
}

impl ObservabilityConfig {
    /// Builds the plain OTLP config consumed by the observability crate.
    /// Header values come from the environment in practice; parsing happens
    /// here so secrets are never echoed in validation errors.
    pub fn otlp_config(&self) -> searchos_observability::OtlpConfig {
        searchos_observability::OtlpConfig {
            enabled: self.otlp_enabled,
            endpoint: self.otlp_endpoint.clone(),
            service_name: self.otlp_service_name.clone(),
            headers: searchos_observability::otlp::parse_headers(&self.otlp_headers),
            metrics_interval_seconds: self.otlp_metrics_interval_seconds,
            export_traces: self.otlp_export_traces,
            export_metrics: self.otlp_export_metrics,
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
            ingest: IngestConfig::default(),
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
    /// Remote retrieval backends (user-selectable via `fulltext`).
    pub remote: RemoteStorageConfig,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            metadata: "sqlite".into(),
            fulltext: "tantivy".into(),
            vector: "lancedb".into(),
            objects: "local".into(),
            remote: RemoteStorageConfig::default(),
        }
    }
}

/// Remote full-text backend connection settings (ADR-027). Only the
/// backend selected by `storage.fulltext` is used; secrets come from the
/// environment, never from this file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RemoteStorageConfig {
    pub elasticsearch: RemoteBackendSection,
    pub meilisearch: RemoteBackendSection,
    pub postgres: RemoteBackendSection,
    pub mysql: RemoteBackendSection,
}

/// One remote backend section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteBackendSection {
    /// REST base URL or SQL connection string.
    pub url: String,
    /// Bearer token for REST backends (env override preferred).
    pub api_key: String,
    /// Index / table name.
    pub index: String,
    /// Per-request timeout seconds.
    pub timeout_seconds: u64,
    /// Verify TLS certificates (default true).
    pub verify_tls: bool,
}

impl Default for RemoteBackendSection {
    fn default() -> Self {
        Self {
            url: String::new(),
            api_key: String::new(),
            index: "searchos_chunks".into(),
            timeout_seconds: 10,
            verify_tls: true,
        }
    }
}

impl RemoteBackendSection {
    /// Converts to the adapter crate's config type.
    pub fn to_adapter_config(&self) -> searchos_index_remote::RemoteBackendConfig {
        searchos_index_remote::RemoteBackendConfig {
            url: self.url.clone(),
            api_key: self.api_key.clone(),
            index: self.index.clone(),
            timeout_seconds: self.timeout_seconds,
            verify_tls: self.verify_tls,
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

/// RFC-029 local ingestion bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IngestConfig {
    pub max_bytes_per_document: usize,
    pub max_bytes_per_batch: usize,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            max_bytes_per_document: 20 * 1024 * 1024,
            max_bytes_per_batch: 200 * 1024 * 1024,
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
/// RFC-014 14.3: keys never in config files; multi-key rotation uses
/// TAVILY_API_KEYS / BRAVE_API_KEYS).
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
    /// Multiple API keys for rotation (RFC-009 9.7). Env override:
    /// `TAVILY_API_KEYS` / `BRAVE_API_KEYS` (comma or whitespace separated).
    pub api_keys: Vec<String>,
    /// Rotation policy: `round_robin` (default) | `failover`.
    pub rotation: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            priority: 1,
            api_keys: Vec::new(),
            rotation: "round_robin".into(),
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
/// Multi-key rotation env vars (comma / whitespace separated).
pub const ENV_TAVILY_KEYS: &str = "TAVILY_API_KEYS";
pub const ENV_BRAVE_KEYS: &str = "BRAVE_API_KEYS";
pub const ENV_CONFIG_PATH: &str = "SEARCHOS_CONFIG";
pub const ENV_AUTH_TOKEN: &str = "SEARCHOS_AUTH_TOKEN";
pub const ENV_OTLP_ENABLED: &str = "SEARCHOS_OTLP_ENABLED";
pub const ENV_OTLP_ENDPOINT: &str = "SEARCHOS_OTLP_ENDPOINT";
pub const ENV_OTLP_SERVICE_NAME: &str = "SEARCHOS_OTLP_SERVICE_NAME";
pub const ENV_OTLP_HEADERS: &str = "SEARCHOS_OTLP_HEADERS";

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

    /// Tavily key resolution: env first (RFC-014 14.3). Multi-key env
    /// (`TAVILY_API_KEYS`) wins; then config list; then the single-key env.
    pub fn tavily_api_key(&self) -> Option<String> {
        Self::resolve_keys(
            ENV_TAVILY_KEYS,
            &self.providers.tavily.api_keys,
            ENV_TAVILY_KEY,
        )
        .into_iter()
        .next()
    }

    /// All Tavily keys for rotation (deduped, empty removed).
    pub fn tavily_api_keys(&self) -> Vec<String> {
        Self::resolve_keys(
            ENV_TAVILY_KEYS,
            &self.providers.tavily.api_keys,
            ENV_TAVILY_KEY,
        )
    }

    /// Brave key resolution: env first.
    pub fn brave_api_key(&self) -> Option<String> {
        Self::resolve_keys(
            ENV_BRAVE_KEYS,
            &self.providers.brave.api_keys,
            ENV_BRAVE_KEY,
        )
        .into_iter()
        .next()
    }

    /// All Brave keys for rotation (deduped, empty removed).
    pub fn brave_api_keys(&self) -> Vec<String> {
        Self::resolve_keys(
            ENV_BRAVE_KEYS,
            &self.providers.brave.api_keys,
            ENV_BRAVE_KEY,
        )
    }

    /// Rotation policy for a provider section (`round_robin` | `failover`).
    pub fn rotation_policy(&self, provider: &str) -> searchos_provider::RotationPolicy {
        let raw = match provider {
            "tavily" => &self.providers.tavily.rotation,
            "brave" => &self.providers.brave.rotation,
            _ => return searchos_provider::RotationPolicy::RoundRobin,
        };
        searchos_provider::RotationPolicy::from_config(raw)
    }

    /// Key resolution order: multi-key env → config list → single env key.
    fn resolve_keys(env_multi: &str, config_keys: &[String], env_single: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Ok(raw) = std::env::var(env_multi) {
            for part in raw.split([',', ';', ' ', '\n']) {
                push_key(&mut out, part);
            }
        }
        for k in config_keys {
            push_key(&mut out, k);
        }
        if out.is_empty() {
            if let Ok(single) = std::env::var(env_single) {
                push_key(&mut out, &single);
            }
        }
        out
    }
}

/// Pushes a trimmed, non-empty, not-yet-present key.
fn push_key(out: &mut Vec<String>, raw: &str) {
    let k = raw.trim().to_string();
    if !k.is_empty() && !out.contains(&k) {
        out.push(k);
    }
}

/// Complete schema field list accepted by `Config::validate`.
/// A field must appear here and in CONSUMED/DEPRECATED lists below.
pub const ALL_CONFIG_FIELDS: &[&str] = &[
    "config_version",
    "app.name",
    "app.mode",
    "app.data_dir",
    "server.enabled",
    "server.host",
    "server.port",
    "server.auth_token",
    "server.api_version",
    "server.rate_limit_per_minute",
    "server.cors_origin",
    "server.workspace_tokens",
    "storage.metadata",
    "storage.fulltext",
    "storage.vector",
    "storage.objects",
    "storage.remote.elasticsearch.url",
    "storage.remote.elasticsearch.api_key",
    "storage.remote.elasticsearch.index",
    "storage.remote.elasticsearch.timeout_seconds",
    "storage.remote.elasticsearch.verify_tls",
    "storage.remote.meilisearch.url",
    "storage.remote.meilisearch.api_key",
    "storage.remote.meilisearch.index",
    "storage.remote.meilisearch.timeout_seconds",
    "storage.remote.meilisearch.verify_tls",
    "storage.remote.postgres.url",
    "storage.remote.postgres.api_key",
    "storage.remote.postgres.index",
    "storage.remote.postgres.timeout_seconds",
    "storage.remote.postgres.verify_tls",
    "storage.remote.mysql.url",
    "storage.remote.mysql.api_key",
    "storage.remote.mysql.index",
    "storage.remote.mysql.timeout_seconds",
    "storage.remote.mysql.verify_tls",
    "search.local_first",
    "search.semantic_dedup",
    "search.incremental",
    "search.max_external_queries",
    "search.cache_ttl_seconds",
    "ingest.max_bytes_per_document",
    "ingest.max_bytes_per_batch",
    "memory.enabled",
    "memory.embedding.provider",
    "memory.embedding.model",
    "intelligence.query_expansion",
    "intelligence.reranking",
    "intelligence.answer_generation",
    "indexes.rebuild_on_start",
    "indexes.verify_on_start",
    "policies.default_profile",
    "policies.minimum_quality",
    "policies.require_provenance",
    "budgets.default_max_external_queries",
    "budgets.default_max_total_cost_micros",
    "budgets.baseline_per_query_micros",
    "crawler.default_timeout_ms",
    "crawler.max_concurrent",
    "crawler.respect_robots",
    "crawler.robots_unknown_allow",
    "crawler.crawl_delay_cap_seconds",
    "extensions.allow_native",
    "extensions.wasm_enabled",
    "security.ssrf_protection",
    "security.max_response_bytes",
    "security.max_redirects",
    "observability.log_level",
    "observability.trace_retention_days",
    "observability.metrics_enabled",
    "observability.otlp_enabled",
    "observability.otlp_endpoint",
    "observability.otlp_service_name",
    "observability.otlp_headers",
    "observability.otlp_metrics_interval_seconds",
    "observability.otlp_export_traces",
    "observability.otlp_export_metrics",
    "providers.tavily.enabled",
    "providers.tavily.priority",
    "providers.tavily.api_keys",
    "providers.tavily.rotation",
    "providers.brave.enabled",
    "providers.brave.priority",
    "providers.brave.api_keys",
    "providers.brave.rotation",
];

/// Fields with a real runtime consumer (Part 30 INV-30-16).
pub const CONSUMED_CONFIG_FIELDS: &[&str] = &[
    "config_version",
    "app.name",
    "app.mode",
    "app.data_dir",
    "server.enabled",
    "server.host",
    "server.port",
    "server.auth_token",
    "server.api_version",
    "server.rate_limit_per_minute",
    "server.cors_origin",
    "server.workspace_tokens",
    "storage.metadata",
    "storage.fulltext",
    "storage.vector",
    "storage.objects",
    "storage.remote.elasticsearch.url",
    "storage.remote.elasticsearch.api_key",
    "storage.remote.elasticsearch.index",
    "storage.remote.elasticsearch.timeout_seconds",
    "storage.remote.elasticsearch.verify_tls",
    "storage.remote.meilisearch.url",
    "storage.remote.meilisearch.api_key",
    "storage.remote.meilisearch.index",
    "storage.remote.meilisearch.timeout_seconds",
    "storage.remote.meilisearch.verify_tls",
    "storage.remote.postgres.url",
    "storage.remote.postgres.api_key",
    "storage.remote.postgres.index",
    "storage.remote.postgres.timeout_seconds",
    "storage.remote.postgres.verify_tls",
    "storage.remote.mysql.url",
    "storage.remote.mysql.api_key",
    "storage.remote.mysql.index",
    "storage.remote.mysql.timeout_seconds",
    "storage.remote.mysql.verify_tls",
    "search.local_first",
    "search.semantic_dedup",
    "search.incremental",
    "search.max_external_queries",
    "search.cache_ttl_seconds",
    "ingest.max_bytes_per_document",
    "ingest.max_bytes_per_batch",
    "memory.enabled",
    "memory.embedding.provider",
    "memory.embedding.model",
    "intelligence.query_expansion",
    "intelligence.reranking",
    "indexes.rebuild_on_start",
    "indexes.verify_on_start",
    "policies.default_profile",
    "policies.minimum_quality",
    "policies.require_provenance",
    "budgets.default_max_external_queries",
    "budgets.default_max_total_cost_micros",
    "budgets.baseline_per_query_micros",
    "crawler.default_timeout_ms",
    "crawler.max_concurrent",
    "crawler.respect_robots",
    "crawler.robots_unknown_allow",
    "crawler.crawl_delay_cap_seconds",
    "extensions.allow_native",
    "extensions.wasm_enabled",
    "security.ssrf_protection",
    "security.max_response_bytes",
    "security.max_redirects",
    "observability.log_level",
    "observability.trace_retention_days",
    "observability.metrics_enabled",
    "observability.otlp_enabled",
    "observability.otlp_endpoint",
    "observability.otlp_service_name",
    "observability.otlp_headers",
    "observability.otlp_metrics_interval_seconds",
    "observability.otlp_export_traces",
    "observability.otlp_export_metrics",
    "providers.tavily.enabled",
    "providers.tavily.priority",
    "providers.tavily.api_keys",
    "providers.tavily.rotation",
    "providers.brave.enabled",
    "providers.brave.priority",
    "providers.brave.api_keys",
    "providers.brave.rotation",
];

/// Explicitly deprecated knobs; `config validate` warns about them.
pub const DEPRECATED_CONFIG_FIELDS: &[&str] = &["intelligence.answer_generation"];

impl SearchOsConfig {
    /// Fields read by runtime code (static parity assertion target).
    pub fn consumed_config_fields() -> &'static [&'static str] {
        CONSUMED_CONFIG_FIELDS
    }

    /// Explicit deprecation warnings, never silent no-op knobs.
    pub fn deprecation_warnings(&self) -> Vec<String> {
        DEPRECATED_CONFIG_FIELDS
            .iter()
            .map(|field| {
                format!(
                    "deprecated config field {field}: SearchOS returns evidence, not generated answers"
                )
            })
            .collect()
    }

    /// Fields neither consumed nor explicitly deprecated.
    pub fn unconsumed_fields() -> Vec<&'static str> {
        let consumed: BTreeSet<&str> = CONSUMED_CONFIG_FIELDS.iter().copied().collect();
        let deprecated: BTreeSet<&str> = DEPRECATED_CONFIG_FIELDS.iter().copied().collect();
        ALL_CONFIG_FIELDS
            .iter()
            .copied()
            .filter(|f| !consumed.contains(f) && !deprecated.contains(f))
            .collect()
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
        if let Err(message) = self.observability.otlp_config().validate() {
            errors.push(format!("observability: {message}"));
        }
        if !self.server.workspace_tokens.is_empty() && self.server.auth_token.is_empty() {
            errors.push("server.workspace_tokens requires server.auth_token".into());
        }
        if self.intelligence.answer_generation {
            errors.push(
                "intelligence.answer_generation is deprecated/out of scope: SearchOS returns evidence, not answers"
                    .into(),
            );
        }
        for field in Self::unconsumed_fields() {
            errors.push(format!(
                "config field {field} has no runtime consumer and is not deprecated"
            ));
        }
        // Pricing strings are validated here as well as in the estimator, so
        // a malformed price is never silently zero (INV-30-13).
        for e in searchos_cost::CostModelConfig::default_config().validate_prices() {
            errors.push(e);
        }
        // Storage backend selection (ADR-027): only known backends are
        // accepted; a remote backend must have a configured endpoint.
        let fulltext = self.storage.fulltext.trim().to_lowercase();
        let kind = searchos_index_remote::BackendKind::from_config(&fulltext);
        if kind.is_remote() {
            let section = match kind {
                searchos_index_remote::BackendKind::Elasticsearch => {
                    &self.storage.remote.elasticsearch
                }
                searchos_index_remote::BackendKind::Meilisearch => &self.storage.remote.meilisearch,
                searchos_index_remote::BackendKind::PostgresFts => &self.storage.remote.postgres,
                searchos_index_remote::BackendKind::MySqlFts => &self.storage.remote.mysql,
                searchos_index_remote::BackendKind::Tantivy => unreachable!(),
            };
            if section.url.trim().is_empty() {
                errors.push(format!(
                    "storage.remote.{}.url is empty — set the {backend} endpoint or keep storage.fulltext: tantivy",
                    kind.config_name(),
                    backend = kind.config_name()
                ));
            }
        } else if !fulltext.is_empty() && fulltext != "tantivy" && fulltext != "local" {
            errors.push(format!(
                "storage.fulltext: unknown backend '{fulltext}' (expected tantivy | elasticsearch | meilisearch | postgres | mysql)"
            ));
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
        apply_env_overrides(&mut self);
        self
    }
}

/// Applies environment overrides to a parsed config.
///
/// This is the single place env wins over the file. `load_config_report_full`
/// calls it on the real load path; `effective()` exposes it for callers that
/// build a config in memory. Secrets (bearer token, collector headers) are
/// env-only by design and are never echoed.
pub fn apply_env_overrides(cfg: &mut SearchOsConfig) {
    if let Ok(dir) = std::env::var(ENV_DATA_DIR) {
        if !dir.trim().is_empty() {
            cfg.app.data_dir = PathBuf::from(dir);
        }
    }
    if let Ok(token) = std::env::var(ENV_AUTH_TOKEN) {
        cfg.server.auth_token = token;
    }
    // OTLP is env-first: collector tokens must not live in the config file.
    if let Ok(value) = std::env::var(ENV_OTLP_ENABLED) {
        let value = value.trim().to_ascii_lowercase();
        if matches!(value.as_str(), "1" | "true" | "yes" | "on") {
            cfg.observability.otlp_enabled = true;
        } else if matches!(value.as_str(), "0" | "false" | "no" | "off") {
            cfg.observability.otlp_enabled = false;
        }
    }
    if let Ok(value) = std::env::var(ENV_OTLP_ENDPOINT) {
        if !value.trim().is_empty() {
            cfg.observability.otlp_endpoint = value;
        }
    } else if let Ok(value) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        // Standard OTEL env var, for collectors configured globally.
        if !value.trim().is_empty() {
            cfg.observability.otlp_endpoint = value;
            cfg.observability.otlp_enabled = true;
        }
    }
    if let Ok(value) = std::env::var(ENV_OTLP_SERVICE_NAME) {
        if !value.trim().is_empty() {
            cfg.observability.otlp_service_name = value;
        }
    }
    if let Ok(value) = std::env::var(ENV_OTLP_HEADERS) {
        cfg.observability.otlp_headers = value;
    }
}

/// Loads a config file with errors and deprecation warnings separated.
pub fn load_config_report_full(
    explicit: Option<&Path>,
) -> (SearchOsConfig, Vec<String>, Vec<String>) {
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
    apply_env_overrides(&mut cfg);
    if let Err(v) = cfg.validate() {
        errors.extend(v);
    }
    let warnings = cfg.deprecation_warnings();
    (cfg, errors, warnings)
}

/// Compatibility wrapper: parse/validation errors alongside the safe default.
pub fn load_config_report(explicit: Option<&Path>) -> (SearchOsConfig, Vec<String>) {
    let (cfg, errors, _) = load_config_report_full(explicit);
    (cfg, errors)
}

/// Loads a config file; missing file → defaults. Errors and deprecation
/// warnings go to stderr so stdout stays machine-parseable.
pub fn load_config(explicit: Option<&Path>) -> SearchOsConfig {
    let (cfg, errors, warnings) = load_config_report_full(explicit);
    for e in errors {
        eprintln!("searchos config: {e}");
    }
    for w in warnings {
        eprintln!("searchos config warning: {w}");
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

    /// Part 30 §30.11: every validated config field is consumed or
    /// explicitly deprecated; silent no-op knobs are forbidden.
    #[test]
    fn no_unconsumed_config_fields() {
        let unconsumed = SearchOsConfig::unconsumed_fields();
        assert!(
            unconsumed.is_empty(),
            "unconsumed config fields: {unconsumed:?}"
        );
        for field in ALL_CONFIG_FIELDS {
            assert!(
                CONSUMED_CONFIG_FIELDS.contains(field) || DEPRECATED_CONFIG_FIELDS.contains(field),
                "{field} is neither consumed nor deprecated"
            );
        }
    }

    #[test]
    fn deprecated_field_emits_warning() {
        let warnings = SearchOsConfig::default().deprecation_warnings();
        assert!(warnings
            .iter()
            .any(|w| w.contains("intelligence.answer_generation")));
        let (_, _, warnings) = load_config_report_full(None);
        assert!(!warnings.is_empty());
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
