//! Runtime assembly (RFC-002 core crate responsibilities).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use searchos_application::analytics::{AnalyticsEvent, AnalyticsQuery, AnalyticsQueryService};
use searchos_application::fetch_service::{DocumentPort, FetchServiceImpl};
use searchos_application::search_service::{BudgetGuard, BudgetSettler, IdempotencyClaim};
use searchos_application::singleflight::SingleflightCoordinator;
use searchos_application::{CostUseCase, MemoryFacade, MemoryUseCase, SearchOsApplication};
use searchos_cache::{
    CanonicalQueryCache, EmbeddingCache, ExactQueryCache, RerankCache, RetrievalCache,
    SemanticCache, DEFAULT_MAX_CACHE_ENTRIES,
};
use searchos_cost::CostModelConfig;

use searchos_decision::trace::TraceCollector;
use searchos_domain::cost::{CostEvent, CostQuery, CostSummary};
use searchos_domain::memory::MemoryEntry;
use searchos_domain::SearchOsError;
use searchos_fetch::{FetchLimits, RobotsChecker, UrlGuard};
use searchos_provider::embedding::{cosine_similarity, EmbeddingProvider, HashingEmbedder};
use searchos_provider::registry::{ProviderRegistry, RegisteredProvider};
use searchos_provider::{BraveProvider, KeyPool, RotatingProvider, TavilyProvider};
use searchos_retrieval::{HybridRetriever, RrfConfig, SearchIndex, VectorIndex};
use searchos_storage_fs::LocalObjectStore;
use searchos_storage_sqlite::repos;
use searchos_storage_sqlite::store::SqliteStore;

use crate::config::{SearchOsConfig, CONSUMED_CONFIG_FIELDS, DEPRECATED_CONFIG_FIELDS};

/// The assembled runtime.
pub struct SearchOsRuntime {
    pub config: SearchOsConfig,
    pub application: Arc<SearchOsApplication>,
    pub sqlite: Arc<SqliteStore>,
    /// Active full-text index (ADR-027). `tantivy` by default, or a remote
    /// backend chosen via `storage.fulltext`. All writes/searches flow
    /// through this trait object; SQLite stays the metadata source of
    /// truth and remote indexes stay rebuildable (ADR-022).
    pub fulltext: Arc<dyn SearchIndex>,
    /// Concrete local index when the embedded backend is active; kept for
    /// CLI/diagnostics that read tantivy-specific counters.
    pub tantivy: Option<Arc<searchos_index_tantivy::TantivyIndex>>,
    pub lancedb: Arc<searchos_vector_lancedb::LanceVectorIndex>,
    /// Dense vector port used by hybrid retrieval and outbox upserts.
    pub vector: Arc<dyn VectorIndex>,
    pub hybrid: Arc<HybridRetriever>,
    pub embedder: Arc<dyn EmbeddingProvider>,
    pub canonical_cache: Arc<CanonicalQueryCache>,
    pub semantic_cache: Arc<SemanticCache>,
    pub retrieval_cache: Arc<RetrievalCache>,
    pub embedding_cache: Arc<EmbeddingCache>,
    pub rerank_cache: Arc<RerankCache>,
    pub objects: Arc<LocalObjectStore>,
    pub provider_registry: Arc<ProviderRegistry>,
    pub extensions: Arc<searchos_extension::ExtensionRegistry>,
    pub crawlers: Arc<searchos_crawler::CrawlerRegistry>,
    pub guard: Arc<UrlGuard>,
    pub robots: Arc<RobotsChecker>,
    pub exact_cache: Arc<ExactQueryCache>,
    /// Singleflight coordinator, kept for cache/index invalidation.
    pub singleflight: Arc<SingleflightCoordinator>,
    /// Config fields consumed by the runtime, for `config validate` parity.
    pub config_consumption: BTreeMap<String, String>,
    pub metrics_enabled: bool,
    pub trace_retention_days: u32,
    pub semantic_dedup: bool,
    pub incremental: bool,
}

impl SearchOsRuntime {
    /// Name of the active full-text backend (`tantivy` / `elasticsearch` /
    /// `meilisearch` / `postgres` / `mysql`), for diagnostics.
    pub fn fulltext_backend(&self) -> &'static str {
        searchos_index_remote::BackendKind::from_config(&self.config.storage.fulltext).config_name()
    }
}

impl SearchOsRuntime {
    /// Assembles the full runtime under a data directory.
    pub async fn init(config: SearchOsConfig) -> Result<Arc<Self>, SearchOsError> {
        Self::init_with_paths(
            &config,
            &config.data_db_path(),
            &config.tantivy_path(),
            &config.lancedb_path(),
            &config.objects_path(),
        )
        .await
    }

    /// Assembly with explicit paths (tests / custom layouts).
    pub async fn init_with_paths(
        config: &SearchOsConfig,
        db_path: &Path,
        tantivy_path: &Path,
        lancedb_path: &Path,
        objects_path: &Path,
    ) -> Result<Arc<Self>, SearchOsError> {
        // Metadata store (runs migrations).
        let sqlite = Arc::new(
            SqliteStore::open(db_path).map_err(|e| SearchOsError::Storage(e.to_string()))?,
        );

        // Full-text index (ADR-027): embedded Tantivy by default, or a
        // user-selected remote backend. The remote adapter implements the
        // same `SearchIndex` port, so the outbox worker, local knowledge
        // port and rebuild path stay backend-agnostic.
        let kind = searchos_index_remote::BackendKind::from_config(&config.storage.fulltext);
        let concrete_tantivy = if kind == searchos_index_remote::BackendKind::Tantivy {
            Some(Arc::new(searchos_index_tantivy::TantivyIndex::open(
                tantivy_path,
            )?))
        } else {
            None
        };
        let fulltext: Arc<dyn SearchIndex> = match kind {
            searchos_index_remote::BackendKind::Tantivy => {
                Arc::clone(concrete_tantivy.as_ref().expect("checked above"))
                    as Arc<dyn SearchIndex>
            }
            searchos_index_remote::BackendKind::Elasticsearch => Arc::new(
                searchos_index_remote::ElasticsearchIndex::open(
                    config.storage.remote.elasticsearch.to_adapter_config(),
                )
                .await?,
            ),
            searchos_index_remote::BackendKind::Meilisearch => Arc::new(
                searchos_index_remote::MeilisearchIndex::open(
                    config.storage.remote.meilisearch.to_adapter_config(),
                )
                .await?,
            ),
            searchos_index_remote::BackendKind::PostgresFts => Arc::new(
                searchos_index_remote::PostgresFtsIndex::open(
                    config.storage.remote.postgres.to_adapter_config(),
                )
                .await?,
            ),
            searchos_index_remote::BackendKind::MySqlFts => Arc::new(
                searchos_index_remote::MySqlFtsIndex::open(
                    config.storage.remote.mysql.to_adapter_config(),
                )
                .await?,
            ),
        };

        // Vector index (rebuildable).
        let lancedb = Arc::new(
            searchos_vector_lancedb::LanceVectorIndex::open(lancedb_path, "embeddings").await?,
        );

        // Content store.
        let objects = Arc::new(LocalObjectStore::new(objects_path));

        // Observability / config consumption. Every validated knob is read
        // here or explicitly deprecated; `config validate` reports the same
        // schema (Part 30 INV-30-16).
        let metrics = searchos_observability::MetricsRegistry::global();
        searchos_observability::logging::init_with_default(&config.observability.log_level);
        let cost_config = CostModelConfig::default_config();
        for error in cost_config.validate_prices() {
            tracing::warn!(%error, "invalid configured price");
            metrics.inc_counter("cost.pricing.parse_error_total");
        }
        let mut config_consumption: BTreeMap<String, String> = BTreeMap::new();
        for field in CONSUMED_CONFIG_FIELDS {
            config_consumption.insert((*field).to_string(), "runtime".into());
        }
        for field in DEPRECATED_CONFIG_FIELDS {
            config_consumption.insert((*field).to_string(), "deprecated".into());
        }
        config_consumption.insert("app.name".into(), config.app.name.clone());
        config_consumption.insert("app.mode".into(), config.app.mode.clone());
        config_consumption.insert(
            "crawler.default_timeout_ms".into(),
            config.crawler.default_timeout_ms.to_string(),
        );
        config_consumption.insert(
            "crawler.max_concurrent".into(),
            config.crawler.max_concurrent.to_string(),
        );
        config_consumption.insert(
            "crawler.respect_robots".into(),
            config.crawler.respect_robots.to_string(),
        );
        config_consumption.insert(
            "crawler.robots_unknown_allow".into(),
            config.crawler.robots_unknown_allow.to_string(),
        );
        config_consumption.insert(
            "crawler.crawl_delay_cap_seconds".into(),
            config.crawler.crawl_delay_cap_seconds.to_string(),
        );
        config_consumption.insert(
            "security.ssrf_protection".into(),
            config.security.ssrf_protection.to_string(),
        );
        config_consumption.insert(
            "security.max_response_bytes".into(),
            config.security.max_response_bytes.to_string(),
        );
        config_consumption.insert(
            "security.max_redirects".into(),
            config.security.max_redirects.to_string(),
        );
        config_consumption.insert(
            "observability.log_level".into(),
            config.observability.log_level.clone(),
        );
        config_consumption.insert(
            "observability.metrics_enabled".into(),
            config.observability.metrics_enabled.to_string(),
        );
        config_consumption.insert(
            "observability.trace_retention_days".into(),
            config.observability.trace_retention_days.to_string(),
        );
        config_consumption.insert(
            "search.semantic_dedup".into(),
            config.search.semantic_dedup.to_string(),
        );
        config_consumption.insert(
            "search.incremental".into(),
            config.search.incremental.to_string(),
        );
        config_consumption.insert(
            "indexes.rebuild_on_start".into(),
            config.indexes.rebuild_on_start.to_string(),
        );
        config_consumption.insert(
            "indexes.verify_on_start".into(),
            config.indexes.verify_on_start.to_string(),
        );
        config_consumption.insert(
            "extensions.allow_native".into(),
            config.extensions.allow_native.to_string(),
        );
        config_consumption.insert(
            "extensions.wasm_enabled".into(),
            config.extensions.wasm_enabled.to_string(),
        );
        config_consumption.insert(
            "memory.embedding.provider".into(),
            config.memory.embedding.provider.clone(),
        );
        config_consumption.insert(
            "memory.embedding.model".into(),
            config.memory.embedding.model.clone(),
        );
        metrics.set_gauge(
            "config.unconsumed_field_total",
            SearchOsConfig::unconsumed_fields().len() as u64,
        );
        metrics.set_gauge(
            "storage.schema.version",
            searchos_storage_sqlite::migrations::MIGRATIONS
                .last()
                .unwrap()
                .version as u64,
        );

        // Fetch safety + resource limits from config (Part 24 §24.1).
        let guard = Arc::new(UrlGuard::with_config(
            config.security.max_redirects,
            config.crawler.max_concurrent,
            config.security.ssrf_protection,
            vec![80, 443, 8080],
        ));
        let robots = Arc::new(RobotsChecker::new());
        let fetch_limits = FetchLimits {
            timeout: Duration::from_millis(config.crawler.default_timeout_ms),
            max_response_bytes: config.security.max_response_bytes as usize,
            max_redirects: config.security.max_redirects,
            max_concurrent: config.crawler.max_concurrent,
            respect_robots: config.crawler.respect_robots,
            unknown_robots_disallow: !config.crawler.robots_unknown_allow,
            crawl_delay_cap_seconds: config.crawler.crawl_delay_cap_seconds,
        };

        // Document port over sqlite repos.
        let doc_port = Arc::new(SqliteDocumentPort {
            store: Arc::clone(&sqlite),
            objects: objects.clone(),
        });

        // Providers (native extensions compiled in; RFC-011 11.2).
        // Multi-key rotation (RFC-009 9.7): when more than one key is
        // configured the adapter is wrapped in a RotatingProvider that
        // round-robins / fails over across keys. The SAME adapter instance
        // is registered in the router and used for execution, so rotation
        // state is shared.
        let registry = Arc::new(ProviderRegistry::new());
        let mut search_providers: Vec<Arc<dyn searchos_provider::SearchProvider>> = Vec::new();
        let tavily_keys = config.tavily_api_keys();
        if config.providers.tavily.enabled && !tavily_keys.is_empty() {
            let adapter: Arc<dyn searchos_provider::SearchProvider> = if tavily_keys.len() > 1 {
                Arc::new(RotatingProvider::new(
                    "tavily",
                    searchos_provider::SearchProvider::capabilities(&TavilyProvider::new(
                        tavily_keys[0].clone(),
                    )),
                    KeyPool::new(tavily_keys.clone(), config.rotation_policy("tavily")),
                    |k| Arc::new(TavilyProvider::new(k.to_string())),
                ))
            } else {
                Arc::new(TavilyProvider::new(tavily_keys[0].clone()))
            };
            registry.register(RegisteredProvider::from_search(
                Arc::clone(&adapter),
                5_000,
                "USD",
            ));
            search_providers.push(adapter);
        }
        let brave_keys = config.brave_api_keys();
        if config.providers.brave.enabled && !brave_keys.is_empty() {
            let adapter: Arc<dyn searchos_provider::SearchProvider> = if brave_keys.len() > 1 {
                Arc::new(RotatingProvider::new(
                    "brave",
                    searchos_provider::SearchProvider::capabilities(&BraveProvider::new(
                        brave_keys[0].clone(),
                    )),
                    KeyPool::new(brave_keys.clone(), config.rotation_policy("brave")),
                    |k| Arc::new(BraveProvider::new(k.to_string())),
                ))
            } else {
                Arc::new(BraveProvider::new(brave_keys[0].clone()))
            };
            registry.register(RegisteredProvider::from_search(
                Arc::clone(&adapter),
                3_000,
                "USD",
            ));
            search_providers.push(adapter);
        }
        {
            let provider_repo = repos::ProviderRepo::new(&sqlite);
            for id in registry.ids() {
                if registry.get(&id).is_some() {
                    let _ = provider_repo.upsert(
                        &id,
                        &id,
                        "search",
                        if registry
                            .health(&id)
                            .map(|(s, _)| s != searchos_provider::circuit::CircuitState::Open)
                            .unwrap_or(true)
                        {
                            "healthy"
                        } else {
                            "down"
                        },
                        "env:TAVILY_API_KEYS,BRAVE_API_KEYS", // secret reference, not the key
                    );
                }
            }
        }

        // Registries. Discovery failures are isolated, logged and counted;
        // a bad manifest never disables later extensions/crawlers (G-04).
        let extensions = Arc::new(searchos_extension::ExtensionRegistry::new());
        let extension_report = extensions
            .discover_with_policy(
                &config.extensions_path(),
                config.extensions.allow_native,
                config.extensions.wasm_enabled,
            )
            .unwrap_or_else(|e| {
                tracing::warn!(%e, "extension discovery failed");
                metrics.inc_counter("extensions.load_failed_total");
                searchos_extension::DiscoveryReport::default()
            });
        for error in &extension_report.errors {
            tracing::warn!(%error, "extension manifest quarantined");
            metrics.inc_counter("extensions.load_failed_total");
        }
        let crawlers = Arc::new(searchos_crawler::CrawlerRegistry::new());
        let crawler_report = crawlers
            .discover(&config.crawlers_path())
            .unwrap_or_else(|e| {
                tracing::warn!(%e, "crawler discovery failed");
                metrics.inc_counter("extensions.load_failed_total");
                searchos_crawler::DiscoveryReport::default()
            });
        for error in &crawler_report.errors {
            tracing::warn!(%error, "crawler manifest quarantined");
            metrics.inc_counter("extensions.load_failed_total");
        }
        metrics.set_gauge(
            "extensions.quarantined",
            (extension_report.quarantined.len() + crawler_report.quarantined.len()) as u64,
        );

        // Multi-layer caches + hybrid BM25/vector retrieval (RFC-007, G-02).
        let cache_ttl = config.search.cache_ttl_seconds;
        let exact_cache = Arc::new(ExactQueryCache::with_max_entries(
            cache_ttl,
            DEFAULT_MAX_CACHE_ENTRIES,
        ));
        let canonical_cache = Arc::new(CanonicalQueryCache::new(cache_ttl));
        let semantic_cache = Arc::new(SemanticCache::new(cache_ttl));
        let retrieval_cache = Arc::new(RetrievalCache::new(cache_ttl));
        let embedding_cache = Arc::new(EmbeddingCache::default());
        let rerank_cache = Arc::new(RerankCache::new(cache_ttl));
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(HashingEmbedder::new(256));
        let vector: Arc<dyn VectorIndex> = lancedb.clone();
        let hybrid = Arc::new(HybridRetriever::new(
            Arc::clone(&fulltext),
            Some(Arc::clone(&vector)),
            RrfConfig::default(),
        ));

        let singleflight = Arc::new(SingleflightCoordinator::new());
        let deps = Arc::new(searchos_application::search_service::SearchDependencies {
            profiles: searchos_policy::ProfileRegistry::builtin(),
            quality: searchos_policy::QualityPolicy::default(),
            freshness: searchos_policy::FreshnessPolicy::default_policy(),
            source: searchos_policy::SourcePolicy::default_policy(),
            cost_config: cost_config.clone(),
            bm25: Arc::clone(&fulltext),
            vector: Some(Arc::clone(&vector)),
            hybrid: Some(Arc::clone(&hybrid)),
            embedder: Arc::clone(&embedder),
            canonical_cache: Arc::clone(&canonical_cache),
            semantic_cache: Arc::clone(&semantic_cache),
            retrieval_cache: Arc::clone(&retrieval_cache),
            embedding_cache: Arc::clone(&embedding_cache),
            rerank_cache: Arc::clone(&rerank_cache),
            router: registry.clone(),
            search_providers,
            local_port: Arc::new(TantivyLocalPort {
                index: Arc::clone(&fulltext),
            }),
            traces: Arc::new(SqliteTraceCollector {
                store: Arc::clone(&sqlite),
            }),
            cost_sink: Arc::new(SqliteCostSink {
                store: Arc::clone(&sqlite),
            }),
            task_recorder: Arc::new(SqliteTaskRecorder {
                store: Arc::clone(&sqlite),
            }),
            singleflight: Arc::clone(&singleflight),
            cache: exact_cache.clone(),
            semantic_dedup: config.search.semantic_dedup,
            incremental: config.search.incremental,
            query_expansion: config.intelligence.query_expansion,
            reranking: config.intelligence.reranking,
            allow_external: config.tavily_api_key().is_some() || config.brave_api_key().is_some(),
        });

        let search_service: Arc<dyn searchos_application::search_service::SearchService> =
            Arc::new(searchos_application::search_service::SearchServiceImpl::new(deps));
        let fetch_service: Arc<dyn searchos_application::fetch_service::FetchService> =
            Arc::new(FetchServiceImpl::with_runtime(
                guard.clone(),
                robots.clone(),
                objects.clone(),
                doc_port.clone(),
                Some(Arc::clone(&crawlers)),
                fetch_limits.clone(),
            ));

        // RFC-029: local data import. Same DocumentPort as fetch, plus batch
        // job tracking over sqlite.
        let ingest_jobs: Arc<dyn searchos_application::ingest_service::IngestJobPort> =
            Arc::new(SqliteIngestJobPort {
                store: Arc::clone(&sqlite),
            });
        let ingest_service: Arc<dyn searchos_application::ingest_service::IngestService> = Arc::new(
            searchos_application::ingest_service::IngestServiceImpl::new(
                objects.clone(),
                doc_port.clone(),
            )
            .with_limits(
                config.ingest.max_bytes_per_document,
                config.ingest.max_bytes_per_batch,
            )
            .with_job_port(ingest_jobs),
        );

        // Memory service backed by sqlite, using the configured embedding
        // provider/model label and a local deterministic embedder (G-05).
        let memory_embedding_provider = config.memory.embedding.provider.clone();
        let memory_embedding_model = config.memory.embedding.model.clone();
        if memory_embedding_provider != "local" {
            tracing::warn!(
                provider = %memory_embedding_provider,
                "remote memory embeddings not wired; using local hashing embedder"
            );
        }
        let memory_sqlite = Arc::new(SqliteMemoryStore {
            store: Arc::clone(&sqlite),
            embedder: Arc::clone(&embedder),
            embedding_provider: memory_embedding_provider,
            embedding_model: memory_embedding_model,
            enabled: config.memory.enabled,
        });
        let memory_service: Arc<dyn searchos_memory::MemoryService> = memory_sqlite;
        let memory_facade = MemoryFacade::new(Arc::clone(&memory_service));
        let memory_service_facade: Arc<dyn MemoryUseCase> = Arc::new(memory_facade);

        let cost_service: Arc<dyn CostUseCase> = Arc::new(SqliteCostSink {
            store: Arc::clone(&sqlite),
        });

        let analytics_service: Arc<dyn AnalyticsQueryService> = Arc::new(SqliteAnalyticsService {
            store: Arc::clone(&sqlite),
            baseline_per_query_micros: config.budgets.baseline_per_query_micros,
        });
        if config.observability.metrics_enabled {
            // Best-effort initial Read Model build. Failure must not prevent
            // the runtime from starting (RFC-026 2.3).
            let _ = analytics_service.refresh().await;
        }

        let application = Arc::new(SearchOsApplication {
            search_service,
            fetch_service,
            ingest_service,
            memory_service: memory_service_facade,
            cost_service,
            analytics_service,
        });

        let runtime = Arc::new(Self {
            config: config.clone(),
            application,
            sqlite,
            fulltext,
            tantivy: concrete_tantivy,
            lancedb,
            vector,
            hybrid,
            embedder,
            canonical_cache,
            semantic_cache,
            retrieval_cache,
            embedding_cache,
            rerank_cache,
            objects,
            provider_registry: registry,
            extensions,
            crawlers,
            guard,
            robots,
            exact_cache,
            singleflight,
            config_consumption,
            metrics_enabled: config.observability.metrics_enabled,
            trace_retention_days: config.observability.trace_retention_days,
            semantic_dedup: config.search.semantic_dedup,
            incremental: config.search.incremental,
        });

        // Explicit config-driven index maintenance. Remediation never runs
        // implicitly unless these knobs are set (Part 30 §30.16).
        if config.indexes.rebuild_on_start {
            if let Err(e) = runtime.rebuild_index().await {
                tracing::warn!(%e, "rebuild_on_start failed");
            }
        }
        if config.indexes.verify_on_start {
            match runtime.index_consistency().await {
                Ok((index, sqlite)) if index != sqlite => {
                    tracing::warn!(index, sqlite, "index consistency drift on startup")
                }
                Err(e) => tracing::warn!(%e, "index verification failed"),
                _ => {}
            }
        }

        // RFC-018 18.4 recovery: drain pending outbox events so documents
        // fetched by a previous CLI run are searchable in this process.
        // Best-effort; failures are retried by the gateway worker or next run.
        let _ = runtime.process_outbox(200).await;

        Ok(runtime)
    }

    /// Drops every query/retrieval cache after an index mutation.
    async fn invalidate_caches(&self) {
        self.singleflight.clear().await;
        self.exact_cache.clear();
        self.semantic_cache.clear();
        self.canonical_cache.clear();
        self.retrieval_cache.bump_index_version();
        self.rerank_cache.clear();
    }

    /// Trace collector accessor (integration harness / diagnostics).
    pub fn traces(&self) -> Arc<dyn searchos_decision::trace::TraceCollector> {
        Arc::new(SqliteTraceCollector {
            store: Arc::clone(&self.sqlite),
        })
    }

    /// Cost event sink accessor (integration harness / diagnostics).
    pub fn cost_sink(&self) -> Arc<dyn searchos_application::search_service::CostEventSink> {
        Arc::new(SqliteCostSink {
            store: Arc::clone(&self.sqlite),
        })
    }

    /// Task recorder accessor.
    pub fn task_recorder(&self) -> Arc<dyn searchos_application::search_service::TaskRecorder> {
        Arc::new(SqliteTaskRecorder {
            store: Arc::clone(&self.sqlite),
        })
    }

    /// Local knowledge port accessor.
    pub fn local_port(&self) -> Arc<dyn searchos_decision::LocalKnowledgePort> {
        Arc::new(TantivyLocalPort {
            index: Arc::clone(&self.fulltext),
        })
    }

    /// Index consistency: full-text index chunk count vs sqlite chunk count
    /// (RFC-007 7.8; drift → rebuild guidance).
    pub async fn index_consistency(&self) -> Result<(u64, u64), SearchOsError> {
        // For the embedded backend the searcher count is authoritative
        // (survives restarts, RFC-018 18.4); remote backends report their
        // server-side count through the same port.
        let index_count = match &self.tantivy {
            Some(t) => t.num_docs(),
            None => self.fulltext.count().await?,
        };
        let sqlite_chunks: i64 = self.sqlite.with_conn(|c| {
            c.query_row("SELECT count(*) FROM document_chunks", [], |r| r.get(0))
                .map_err(|e| SearchOsError::Storage(e.to_string()))
        })?;
        Ok((index_count, sqlite_chunks as u64))
    }

    /// Rebuilds the tantivy index from authoritative document data
    /// (ADR-022): SQLite chunk rows are the metadata; chunk *content* is
    /// reconstructed from the content-addressed object store and re-chunked
    /// deterministically (chunking is deterministic ⇒ identical hashes).
    #[allow(clippy::type_complexity)]
    pub async fn rebuild_index(&self) -> Result<u64, SearchOsError> {
        // One JOIN query for every live version + chunk (INV-30-21: no O(V)
        // per-version SQL round-trips). SQLite remains the metadata source of
        // truth; content is reconstructed from the object store.
        #[derive(Debug)]
        struct RebuildRow {
            version_id: String,
            document_id: String,
            url: String,
            title: Option<String>,
            language: Option<String>,
            workspace_id: String,
            fetched_at: String,
            chunk_id: String,
            chunk_hash: String,
            content_ref: String,
            chunk_index: i64,
        }
        let rows: Vec<RebuildRow> = self.sqlite.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT dv.id, dv.document_id, d.canonical_url, d.title, d.language,
                                d.workspace_id, dv.fetched_at,
                                dc.id, dc.chunk_hash, dc.content_ref, dc.chunk_index
                         FROM document_versions dv
                         JOIN documents d ON d.id = dv.document_id
                         JOIN document_chunks dc ON dc.version_id = dv.id
                         WHERE d.status != 'deleted'
                         ORDER BY dv.id, dc.chunk_index ASC",
                )
                .map_err(|e| SearchOsError::Storage(e.to_string()))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(RebuildRow {
                        version_id: r.get(0)?,
                        document_id: r.get(1)?,
                        url: r.get(2)?,
                        title: r.get(3)?,
                        language: r.get(4)?,
                        workspace_id: r.get::<_, Option<String>>(5)?.unwrap_or_else(|| "*".into()),
                        fetched_at: r.get(6)?,
                        chunk_id: r.get(7)?,
                        chunk_hash: r.get(8)?,
                        content_ref: r.get(9)?,
                        chunk_index: r.get(10)?,
                    })
                })
                .map_err(|e| SearchOsError::Storage(e.to_string()))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| SearchOsError::Storage(e.to_string()))
        })?;

        let mut order: Vec<String> = Vec::new();
        let mut versions: HashMap<
            String,
            (
                String,
                String,
                Option<String>,
                Option<String>,
                String,
                String,
            ),
        > = HashMap::new();
        let mut chunks_by_version: HashMap<String, Vec<(String, String, i64)>> = HashMap::new();
        let mut content_ref_by_version: HashMap<String, String> = HashMap::new();
        for row in rows {
            if !versions.contains_key(&row.version_id) {
                order.push(row.version_id.clone());
                versions.insert(
                    row.version_id.clone(),
                    (
                        row.document_id,
                        row.url,
                        row.title,
                        row.language,
                        row.workspace_id,
                        row.fetched_at,
                    ),
                );
            }
            chunks_by_version
                .entry(row.version_id.clone())
                .or_default()
                .push((row.chunk_id, row.chunk_hash, row.chunk_index));
            content_ref_by_version
                .entry(row.version_id)
                .or_insert(row.content_ref);
        }

        let mut docs: Vec<searchos_domain::retrieval::IndexedDocument> = Vec::new();
        for version_id in order {
            let (document_id, url, title, language, workspace_id, fetched_at) = versions
                .get(&version_id)
                .cloned()
                .ok_or_else(|| SearchOsError::Internal("rebuild version grouping lost".into()))?;
            let fetched_at = chrono::DateTime::parse_from_rfc3339(&fetched_at)
                .map(|t| t.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());
            let stored = chunks_by_version
                .get(&version_id)
                .cloned()
                .unwrap_or_default();
            if stored.is_empty() {
                continue;
            }
            let content_ref = content_ref_by_version
                .get(&version_id)
                .cloned()
                .ok_or_else(|| SearchOsError::Internal("rebuild content ref missing".into()))?;
            let blob: Option<Vec<u8>> =
                searchos_storage_fs::ObjectStore::get(self.objects.as_ref(), &content_ref)
                    .await
                    .ok();
            let Some(blob) = blob else {
                tracing::warn!(version = %version_id, "content blob missing; skipping in rebuild");
                continue;
            };
            let text = String::from_utf8_lossy(&blob).to_string();
            let chunks = searchos_document::chunk_text(&text, &Default::default());
            for (i, ch) in chunks.iter().enumerate() {
                let Some((chunk_id, chunk_hash, _idx)) = stored.get(i) else {
                    break;
                };
                if *chunk_hash != ch.hash {
                    tracing::warn!(version = %version_id, index = i, "chunk hash drift; skipping chunk");
                    continue;
                }
                docs.push(searchos_domain::retrieval::IndexedDocument {
                    chunk_id: searchos_domain::ids::ChunkId::new(chunk_id.clone()),
                    document_id: searchos_domain::ids::DocumentId::new(document_id.clone()),
                    version_id: searchos_domain::ids::DocumentVersionId::new(version_id.clone()),
                    title: title.clone(),
                    body: ch.text.clone(),
                    url: url.clone(),
                    language: language.clone().or_else(|| Some("en".into())),
                    workspace_id: workspace_id.clone(),
                    fetched_at,
                });
            }
        }
        let n = docs.len() as u64;
        self.fulltext.rebuild_from(&docs).await?;
        Ok(n)
    }

    /// Processes pending outbox rows into the full-text index (RFC-006 6.7).
    /// Returns (processed, dead).
    pub async fn process_outbox(&self, limit: u32) -> Result<(u64, u64), SearchOsError> {
        self.process_outbox_with_index(self.fulltext.clone(), limit)
            .await
    }

    /// Soft-deletes a document and removes it from the full-text index.
    ///
    /// The metadata row is soft-deleted, its chunks are dropped and the index
    /// deletes are enqueued as outbox rows (chunk-id space, INV-30-08). The
    /// outbox is drained immediately so the deletion is observable right
    /// after the call — the console's "delete then verify" flow depends on it.
    pub async fn delete_document(
        &self,
        document_id: &searchos_domain::ids::DocumentId,
    ) -> Result<(), SearchOsError> {
        let repo = repos::DocumentRepo::new(&self.sqlite);
        repo.delete(document_id)?;
        self.process_outbox(256).await?;
        self.invalidate_caches().await;
        Ok(())
    }

    /// Re-indexes a document's current version into the full-text index.
    ///
    /// Returns the number of chunk payloads written. Reindexing never
    /// rewrites the metadata: the version and its chunks are already there,
    /// only the index entries are (re)written.
    pub async fn reindex_document(
        &self,
        document_id: &searchos_domain::ids::DocumentId,
        workspace_id: &searchos_domain::ids::WorkspaceId,
    ) -> Result<usize, SearchOsError> {
        let payloads = self
            .application
            .fetch_service
            .reindex_current_version(document_id, workspace_id)
            .await?;
        let mut written = 0usize;
        for payload in payloads {
            if self.fulltext.index(payload).await.is_ok() {
                written += 1;
            }
        }
        self.fulltext.commit().await?;
        self.invalidate_caches().await;
        Ok(written)
    }

    /// Outbox drain against an explicit index (tests / backend parity).
    pub async fn process_outbox_with_index(
        &self,
        index: Arc<dyn SearchIndex>,
        limit: u32,
    ) -> Result<(u64, u64), SearchOsError> {
        let outbox = repos::OutboxRepo::new(&self.sqlite);
        let rows = outbox.pending(limit)?;
        let mut processed = 0u64;
        let mut dead = 0u64;
        for row in rows {
            let outcome = if row.operation == "delete" {
                // Delete outbox rows carry chunk_id, not document_id. Keep
                // identifier space explicit and only mark processed when the
                // index confirms deletion (INV-30-08).
                let chunk_ids: Vec<searchos_domain::ids::ChunkId> = row
                    .payload
                    .as_deref()
                    .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
                    .and_then(|v| v.get("chunk_ids").cloned())
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_else(|| {
                        vec![searchos_domain::ids::ChunkId::new(row.entity_id.clone())]
                    });
                let deleted = index.delete_by_chunk_ids(&chunk_ids).await.is_ok();
                if deleted {
                    self.invalidate_caches().await;
                }
                deleted
            } else if row.operation == "refresh" {
                // RFC-004 v1.1 10.1: re-fetch the existing source instead of
                // deleting authoritative knowledge or buying a full search.
                let parsed: Result<serde_json::Value, _> =
                    serde_json::from_str(row.payload.as_deref().unwrap_or(""));
                match parsed {
                    Ok(v) => {
                        let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                        let ws = v
                            .get("workspace_id")
                            .and_then(|x| x.as_str())
                            .unwrap_or("default");
                        let workspace = searchos_domain::ids::WorkspaceId::new(ws);
                        let fetched = self
                            .application
                            .fetch_service
                            .fetch_and_persist(url, &workspace)
                            .await;
                        match fetched {
                            Ok(doc) => {
                                // A refresh only creates a new version when the
                                // content actually changed. Unchanged content
                                // is a metadata-only update, so no new index
                                // payloads are enqueued (RFC-006 6.4) and the
                                // existing index entries stay valid.
                                if doc.changed {
                                    // Refresh keeps the document row's owning
                                    // workspace; the old version's chunks stay
                                    // in the index until the rebuild worker
                                    // prunes them, so index the new payloads
                                    // directly and commit the writer.
                                    let payloads = self
                                        .application
                                        .fetch_service
                                        .reindex_current_version(&doc.document_id, &workspace)
                                        .await
                                        .unwrap_or_default();
                                    for p in payloads {
                                        let _ = index.index(p).await;
                                    }
                                    index.commit().await.is_ok()
                                } else {
                                    true
                                }
                            }
                            Err(_) => false,
                        }
                    }
                    Err(_) => false,
                }
            } else if let Ok(payload) =
                repos::parse_index_payload(row.payload.as_deref().unwrap_or(""))
            {
                index.index(payload).await.is_ok()
            } else {
                false
            };
            if outcome {
                outbox.mark_processed(&row.id)?;
                processed += 1;
                searchos_observability::MetricsRegistry::global().record_outbox(true, false);
            } else {
                match outbox.mark_failed(&row.id, 3)? {
                    repos::OutboxDisposition::Retry => {}
                    repos::OutboxDisposition::DeadLetter => {
                        dead += 1;
                        searchos_observability::MetricsRegistry::global()
                            .record_outbox(false, true);
                    }
                }
            }
        }
        index.commit().await?;
        Ok((processed, dead))
    }
}

/// LocalKnowledgePort over the active full-text index (application decision input).
struct TantivyLocalPort {
    index: Arc<dyn SearchIndex>,
}

#[async_trait::async_trait]
impl searchos_decision::LocalKnowledgePort for TantivyLocalPort {
    async fn retrieve(
        &self,
        text: &str,
        top_k: usize,
        workspace_id: Option<&str>,
    ) -> Result<Vec<searchos_domain::retrieval::RetrievalCandidate>, SearchOsError> {
        let mut q = searchos_domain::retrieval::RetrievalQuery::new(text, top_k);
        q.workspace_id = workspace_id.map(|s| s.to_string());
        SearchIndex::search(self.index.as_ref(), &q).await
    }

    async fn stale_document_ids(
        &self,
        candidates: &[searchos_domain::retrieval::RetrievalCandidate],
    ) -> Result<Vec<searchos_domain::ids::DocumentId>, SearchOsError> {
        let policy = searchos_policy::FreshnessPolicy::default_policy();
        let mut stale = Vec::new();
        for c in candidates {
            let category = searchos_policy::classify_content(&c.url, c.title.as_deref());
            let age = (chrono::Utc::now() - c.fetched_at).num_seconds().max(0);
            if !policy.is_fresh(category, age) {
                stale.push(c.document_id.clone());
            }
        }
        Ok(stale)
    }
}

/// TaskRecorder over sqlite.
struct SqliteTaskRecorder {
    store: Arc<SqliteStore>,
}

#[async_trait::async_trait]
impl searchos_application::search_service::TaskRecorder for SqliteTaskRecorder {
    async fn insert(
        &self,
        task_id: &searchos_domain::ids::TaskId,
        workspace_id: &searchos_domain::ids::WorkspaceId,
        agent_id: Option<&searchos_domain::ids::AgentId>,
        query: &str,
        normalized_query: &str,
        fingerprint: &str,
        profile_id: &str,
        profile_version: &str,
        mode: &str,
        trace_id: &searchos_domain::ids::TraceId,
    ) -> Result<(), SearchOsError> {
        repos::TaskRepo::new(&self.store).insert(
            task_id,
            workspace_id,
            agent_id,
            query,
            normalized_query,
            fingerprint,
            profile_id,
            profile_version,
            mode,
            trace_id,
        )
    }

    async fn finish(
        &self,
        task_id: &searchos_domain::ids::TaskId,
        status: &str,
    ) -> Result<(), SearchOsError> {
        repos::TaskRepo::new(&self.store).finish(task_id, status)
    }

    async fn idempotency_get(
        &self,
        request_id: &str,
    ) -> Result<Option<serde_json::Value>, SearchOsError> {
        let rec = repos::IdempotencyRepo::new(&self.store).get(request_id)?;
        Ok(rec
            .filter(|r| r.status == "completed")
            .and_then(|r| r.response_json)
            .and_then(|s| serde_json::from_str(&s).ok()))
    }

    async fn idempotency_begin(
        &self,
        request_id: &str,
        task_id: &str,
        workspace_id: &str,
    ) -> Result<IdempotencyClaim, SearchOsError> {
        let repo = repos::IdempotencyRepo::new(&self.store);
        if repo.begin(request_id, task_id, workspace_id)? {
            return Ok(IdempotencyClaim::Claimed);
        }
        let Some(record) = repo.get(request_id)? else {
            return Ok(IdempotencyClaim::Claimed);
        };
        match record.status.as_str() {
            "completed" => {
                let response = record
                    .response_json
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::Value::Null);
                Ok(IdempotencyClaim::Completed { response })
            }
            "failed" => Ok(IdempotencyClaim::Failed),
            _ => Ok(IdempotencyClaim::AlreadyRunning {
                task_id: record.claim_owner_task_id.unwrap_or(record.task_id),
            }),
        }
    }

    async fn idempotency_status(&self, request_id: &str) -> Result<Option<String>, SearchOsError> {
        repos::IdempotencyRepo::new(&self.store).status(request_id)
    }

    async fn idempotency_complete(
        &self,
        request_id: &str,
        payload: &serde_json::Value,
    ) -> Result<(), SearchOsError> {
        repos::IdempotencyRepo::new(&self.store).complete(request_id, &payload.to_string())
    }

    async fn idempotency_fail(&self, request_id: &str) -> Result<(), SearchOsError> {
        repos::IdempotencyRepo::new(&self.store).fail(request_id)
    }

    async fn record_search_completed(
        &self,
        response: &serde_json::Value,
    ) -> Result<(), SearchOsError> {
        repos::AnalyticsRepo::new(&self.store).record_event(
            &searchos_domain::new_id("evt"),
            "search.request.completed",
            response
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .unwrap_or("default"),
            response.get("request_id").and_then(|v| v.as_str()),
            response.get("task_id").and_then(|v| v.as_str()),
            "search_service",
            response.get("trace_id").and_then(|v| v.as_str()),
            &serde_json::json!({
                "status": response.get("status").and_then(|v| v.as_str()).unwrap_or("unknown"),
                "path": response.get("decision").and_then(|d| d.get("path")).and_then(|v| v.as_str()).unwrap_or("unknown"),
                "quality": response.get("quality").and_then(|q| q.get("status")).and_then(|v| v.as_str()).unwrap_or("unknown"),
                "cost_estimated": response.get("cost").and_then(|c| c.get("estimated")).and_then(|v| v.as_f64()).unwrap_or(0.0),
            })
            .to_string(),
        )
    }

    async fn enqueue_refresh(
        &self,
        urls: &[String],
        workspace_id: &str,
    ) -> Result<usize, SearchOsError> {
        let mut n = 0;
        for url in urls {
            repos::enqueue_refresh(&self.store, url, workspace_id)?;
            n += 1;
        }
        Ok(n)
    }
}

/// TraceCollector over sqlite.
struct SqliteTraceCollector {
    store: Arc<SqliteStore>,
}

#[async_trait::async_trait]
impl TraceCollector for SqliteTraceCollector {
    async fn record(
        &self,
        trace: searchos_domain::decision::DecisionTrace,
    ) -> Result<(), SearchOsError> {
        repos::DecisionRepo::new(&self.store).insert_trace(&trace)
    }

    async fn get(
        &self,
        trace_id: &str,
    ) -> Result<Option<searchos_domain::decision::DecisionTrace>, SearchOsError> {
        repos::DecisionRepo::new(&self.store).get_trace(trace_id)
    }
}

/// Cost sink over sqlite (ledger append-only).
struct SqliteCostSink {
    store: Arc<SqliteStore>,
}

/// Synchronous-capable budget settlement handle for RAII guards.
struct SqliteBudgetSettler {
    store: Arc<SqliteStore>,
}

#[async_trait::async_trait]
impl BudgetSettler for SqliteBudgetSettler {
    async fn settle(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        self.settle_sync(task_id, spent_micros)
    }
    fn settle_sync(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        repos::BudgetRepo::new(&self.store).settle(task_id, spent_micros)
    }
    async fn exceed(&self, task_id: &str) -> Result<(), SearchOsError> {
        self.exceed_sync(task_id)
    }
    fn exceed_sync(&self, task_id: &str) -> Result<(), SearchOsError> {
        repos::BudgetRepo::new(&self.store).mark_exceeded(task_id)
    }
    async fn release(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        self.release_sync(task_id, spent_micros)
    }
    fn release_sync(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        repos::BudgetRepo::new(&self.store).release(task_id, spent_micros)
    }
}

#[async_trait::async_trait]
impl searchos_application::search_service::CostEventSink for SqliteCostSink {
    async fn append(&self, ev: CostEvent) -> Result<(), SearchOsError> {
        repos::CostRepo::new(&self.store).append(&ev)
    }

    async fn record_quality_pass(&self, task_id: &str) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO audit_events (id, actor, action, target, at) VALUES (?1, 'quality_gate', 'task_passed', ?2, ?3)",
                [searchos_domain::new_id("aud"), task_id.to_string(), chrono::Utc::now().to_rfc3339()],
            )
            .map(|_| ())
            .map_err(|e| SearchOsError::Storage(e.to_string()))
        })
    }

    async fn singleflight_saved_micros(&self) -> Result<i64, SearchOsError> {
        repos::SingleflightRepo::new(&self.store).total_saved()
    }

    async fn record_singleflight(
        &self,
        fingerprint: &str,
        leader: &str,
        followers: &[String],
        saved: i64,
    ) -> Result<(), SearchOsError> {
        repos::SingleflightRepo::new(&self.store).record(
            fingerprint,
            &searchos_domain::ids::TaskId::new(leader),
            followers,
            saved,
        )
    }

    fn budget_guard_handle(&self) -> Arc<dyn BudgetSettler> {
        Arc::new(SqliteBudgetSettler {
            store: Arc::clone(&self.store),
        })
    }

    async fn reserve_budget(
        &self,
        task_id: &str,
        trace_id: &str,
        estimated_micros: i64,
        currency: &str,
    ) -> Result<BudgetGuard, SearchOsError> {
        repos::BudgetRepo::new(&self.store).reserve(
            &searchos_domain::new_id("res"),
            task_id,
            Some(trace_id),
            estimated_micros,
            currency,
        )?;
        Ok(BudgetGuard::new(self.budget_guard_handle(), task_id))
    }

    async fn settle_budget(&self, task_id: &str, actual_micros: i64) -> Result<(), SearchOsError> {
        repos::BudgetRepo::new(&self.store).settle(task_id, actual_micros)
    }

    async fn exceed_budget(&self, task_id: &str) -> Result<(), SearchOsError> {
        repos::BudgetRepo::new(&self.store).mark_exceeded(task_id)
    }
}

#[async_trait::async_trait]
impl CostUseCase for SqliteCostSink {
    async fn append_event(&self, ev: CostEvent) -> Result<(), SearchOsError> {
        repos::CostRepo::new(&self.store).append(&ev)
    }

    async fn summary(&self, q: &CostQuery) -> Result<CostSummary, SearchOsError> {
        let mut passing = std::collections::HashSet::new();
        self.store
            .with_conn(|c| {
                let mut stmt = c
                    .prepare("SELECT target FROM audit_events WHERE action = 'task_passed'")
                    .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                let v: Vec<String> = rows.filter_map(|r| r.ok()).collect();
                Ok(v)
            })?
            .into_iter()
            .for_each(|t| {
                passing.insert(t);
            });
        repos::CostRepo::new(&self.store).summarize(q, &passing)
    }

    async fn singleflight_saved_micros(&self) -> Result<i64, SearchOsError> {
        repos::SingleflightRepo::new(&self.store).total_saved()
    }

    async fn savings(
        &self,
        baseline_per_query_micros: i64,
    ) -> Result<searchos_domain::cost::SavingsReport, SearchOsError> {
        use searchos_domain::cost::{ReuseCostViews, SavingsReport};
        let (task_count, external_calls, total_estimated, total_actual, quality_passing, local_resolved, original_acquisition, reuse_execution) =
            self.store.with_conn(|c| {
                let q = |sql: &str| -> Result<i64, SearchOsError> {
                    c.query_row(sql, [], |r| r.get(0))
                        .map_err(|e| SearchOsError::Storage(e.to_string()))
                };
                Ok((
                    q("SELECT count(*) FROM search_tasks")?,
                    q("SELECT count(*) FROM cost_events WHERE operation IN ('web_search','web_fetch','web_crawl') AND shared = 0")?,
                    q("SELECT COALESCE(sum(estimated_cost_micros),0) FROM cost_events")?,
                    q("SELECT COALESCE(sum(actual_cost_micros),0) FROM cost_events")?,
                    q("SELECT count(DISTINCT target) FROM audit_events WHERE action = 'task_passed'")?,
                    q("SELECT count(*) FROM decision_traces dt
                       WHERE dt.selected_path IN ('local','local_with_refresh')
                         AND EXISTS (SELECT 1 FROM audit_events ae
                                     WHERE ae.action = 'task_passed' AND ae.target = dt.task_id)")?,
                    q("SELECT COALESCE(sum(estimated_cost_micros),0) FROM cost_events WHERE operation IN ('web_search','web_fetch','web_crawl') AND shared = 0")?,
                    q("SELECT COALESCE(sum(estimated_cost_micros),0) FROM cost_events WHERE operation LIKE 'local_%' OR shared = 1")?,
                ))
            })?;
        let task_count = task_count.max(0) as u64;
        let baseline = (task_count as i64).saturating_mul(baseline_per_query_micros);
        // Actual cost only replaces the estimate when billing data has been
        // reconciled (RFC-006 6.7).
        let searchos_micros = if total_actual > 0 {
            total_actual
        } else {
            total_estimated
        };
        let is_estimate = total_actual == 0;
        let savings_micros = baseline - searchos_micros;
        let savings_rate = if baseline > 0 {
            1.0 - (searchos_micros as f64 / baseline as f64)
        } else {
            0.0
        };
        let external_avoidance_rate = if task_count > 0 {
            (1.0 - (external_calls.max(0) as f64 / task_count as f64)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let valid_local_resolution_rate = if task_count > 0 {
            (local_resolved.max(0) as f64 / task_count as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let quality_passing_tasks = quality_passing.max(0) as u64;
        Ok(SavingsReport {
            currency: "USD".into(),
            task_count,
            baseline_micros: baseline,
            searchos_micros,
            savings_micros,
            savings_rate,
            quality_passing_tasks,
            cost_per_quality_passing_answer_micros: if quality_passing_tasks > 0 {
                Some(searchos_micros / quality_passing_tasks as i64)
            } else {
                None
            },
            external_avoidance_rate,
            valid_local_resolution_rate,
            external_calls: external_calls.max(0) as u64,
            local_resolved_tasks: local_resolved.max(0) as u64,
            reuse_views: ReuseCostViews {
                original_acquisition_micros: original_acquisition,
                reuse_execution_micros: reuse_execution,
                amortized_micros: if task_count > 0 {
                    original_acquisition / task_count as i64
                } else {
                    0
                },
                marginal_micros: (searchos_micros - original_acquisition).max(0),
            },
            is_estimate,
        })
    }

    async fn reservations(&self, limit: u32) -> Result<serde_json::Value, SearchOsError> {
        self.reservations_scoped(limit, None).await
    }

    async fn reservations_scoped(
        &self,
        limit: u32,
        workspace: Option<&str>,
    ) -> Result<serde_json::Value, SearchOsError> {
        let rows = repos::BudgetRepo::new(&self.store).list_scoped(limit, workspace)?;
        Ok(serde_json::json!({ "reservations": rows }))
    }

    async fn reconcile(
        &self,
        cost_event_id: &str,
        actual_cost_micros: i64,
        source: &str,
        currency: &str,
    ) -> Result<(), SearchOsError> {
        repos::ReconciliationRepo::new(&self.store).record(
            cost_event_id,
            actual_cost_micros,
            source,
            currency,
        )
    }
}

/// RFC-026/028 Analytics Query Service over the SQLite Read Model.
struct SqliteAnalyticsService {
    store: Arc<SqliteStore>,
    baseline_per_query_micros: i64,
}

/// Read Models the Dashboard can ask for. A key outside this list is a
/// validation error, not a silent `null` payload.
const ANALYTICS_MODELS: &[&str] = &[
    "overview",
    "search_requests",
    "search_timeseries",
    "search_strategies",
    "cost_breakdown",
    "quality_dimensions",
    "knowledge_growth",
    "knowledge_freshness",
    "providers",
    "crawlers_summary",
    "infrastructure_summary",
];

/// How a Read Model payload was produced. Reported in `meta` so the console
/// can tell a live, filtered SQL query from the precomputed fast path.
struct DatasetProvenance {
    query_mode: &'static str,
    applied_filters: Vec<String>,
    ignored_filters: Vec<(String, String)>,
    refresh_skipped: bool,
    model_age_seconds: Option<i64>,
}

fn analytics_filters(query: &AnalyticsQuery) -> repos::AnalyticsFilters {
    repos::AnalyticsFilters {
        workspace_id: query.workspace_id.clone(),
        agent_id: query.agent_id.clone(),
        provider_id: query.provider_id.clone(),
        search_mode: query.search_mode.clone(),
        quality_status: query.quality_status.clone(),
        project_id: query.project_id.clone(),
        from: query.from.clone(),
        to: query.to.clone(),
        interval: query.interval.clone(),
    }
}

#[async_trait::async_trait]
impl AnalyticsQueryService for SqliteAnalyticsService {
    async fn refresh(&self) -> Result<(), SearchOsError> {
        repos::AnalyticsRepo::new(&self.store).rebuild(self.baseline_per_query_micros)
    }

    async fn dataset(
        &self,
        model_key: &str,
        query: &AnalyticsQuery,
    ) -> Result<serde_json::Value, SearchOsError> {
        query.validate()?;
        if !ANALYTICS_MODELS.contains(&model_key) {
            return Err(SearchOsError::validation(
                "model_key",
                "unknown analytics read model",
            ));
        }
        let filters = analytics_filters(query);
        let repo = repos::AnalyticsRepo::new(&self.store);

        // Fast path: no predicate and the requested bucket granularity is the
        // precomputed one. Reuse the stored payload while it is fresh.
        let (data, provenance) = if filters.is_unfiltered() && !filters.needs_live_buckets() {
            let age = self.model_age_seconds(model_key);
            // Skip the rebuild only when the stored payload provably describes
            // the current operational rows.
            let rebuild_needed = repo.needs_rebuild();
            let refresh_skipped = !rebuild_needed;
            if rebuild_needed && self.refresh().await.is_err() {
                tracing::warn!(
                    "analytics read model rebuild failed; serving the previous snapshot"
                );
            }
            // `local_resolution` in the Overview must be quality-valid. A
            // local decision that did not pass the final gate is not reuse.
            if model_key == "overview" {
                self.constrain_local_resolution_to_quality_passed().await?;
            }
            let data = match model_key {
                "overview" => repo
                    .read_model("overview")?
                    .unwrap_or(serde_json::Value::Null),
                "search_timeseries" => {
                    serde_json::json!({ "items": repo.read_series(model_key, 90)? })
                }
                key => repo.read_model(key)?.unwrap_or(serde_json::Value::Null),
            };
            (
                data,
                DatasetProvenance {
                    query_mode: "precomputed",
                    applied_filters: Vec::new(),
                    ignored_filters: Vec::new(),
                    refresh_skipped,
                    model_age_seconds: age,
                },
            )
        } else {
            // Live path: every applicable predicate is pushed into SQL.
            let dataset = repo.query_dataset(
                model_key,
                &filters,
                self.baseline_per_query_micros,
                query.limit.unwrap_or(90),
                query.cursor.as_deref(),
            )?;
            let ignored = filters
                .present()
                .into_iter()
                .filter(|name| !dataset.applied.contains(name))
                .map(|name| (name.to_string(), repos::ignore_reason(name).to_string()))
                .collect();
            (
                dataset.data,
                DatasetProvenance {
                    query_mode: "sql",
                    applied_filters: dataset.applied.iter().map(|s| s.to_string()).collect(),
                    ignored_filters: ignored,
                    refresh_skipped: false,
                    model_age_seconds: None,
                },
            )
        };
        Ok(dashboard_envelope(data, query, provenance))
    }

    async fn overview(&self, query: &AnalyticsQuery) -> Result<serde_json::Value, SearchOsError> {
        self.dataset("overview", query).await
    }

    async fn record(&self, event: AnalyticsEvent) -> Result<(), SearchOsError> {
        repos::AnalyticsRepo::new(&self.store).record_event(
            &searchos_domain::new_id("evt"),
            &event.event_type,
            &event.workspace_id,
            event.request_id.as_deref(),
            event.execution_id.as_deref(),
            &event.source_module,
            event.correlation_id.as_deref(),
            &event.payload.to_string(),
        )
    }

    async fn events_since(
        &self,
        cursor: i64,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, SearchOsError> {
        repos::AnalyticsRepo::new(&self.store).events_since(cursor, limit)
    }
}

impl SqliteAnalyticsService {
    /// Age of the newest precomputed row for `key`, in seconds.
    fn model_age_seconds(&self, key: &str) -> Option<i64> {
        self.store
            .with_conn(|c| -> Result<Option<i64>, SearchOsError> {
                let mut stmt = c
                    .prepare(
                        "SELECT strftime('%s','now') - strftime('%s', updated_at)
                         FROM analytics_read_models
                         WHERE model_key = ?1 AND workspace_id = '*'
                         ORDER BY bucket_start DESC LIMIT 1",
                    )
                    .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                let mut rows = stmt
                    .query(rusqlite::params![key])
                    .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                match rows
                    .next()
                    .map_err(|e| SearchOsError::Storage(e.to_string()))?
                {
                    Some(row) => Ok(Some(
                        row.get::<_, i64>(0)
                            .map_err(|e| SearchOsError::Storage(e.to_string()))?,
                    )),
                    None => Ok(None),
                }
            })
            .ok()
            .flatten()
    }

    /// Recomputes the quality-valid local count inside the Overview Read Model.
    async fn constrain_local_resolution_to_quality_passed(&self) -> Result<(), SearchOsError> {
        let valid: i64 = self.store.with_conn(|c| {
            c.query_row(
                "SELECT count(*) FROM decision_traces dt
                 WHERE dt.selected_path IN ('local','local_with_refresh')
                   AND EXISTS (SELECT 1 FROM audit_events ae
                               WHERE ae.action = 'task_passed' AND ae.target = dt.task_id)",
                [],
                |r| r.get(0),
            )
            .map_err(|e| SearchOsError::Storage(e.to_string()))
        })?;
        let overview = repos::AnalyticsRepo::new(&self.store)
            .read_model("overview")?
            .ok_or_else(|| SearchOsError::Retrieval("overview Read Model missing".into()))?;
        let total = overview["searches"]["total"].as_i64().unwrap_or(0);
        let mut next = overview;
        next["local_resolution"]["count"] = serde_json::json!(valid);
        next["local_resolution"]["rate"] = serde_json::json!(if total > 0 {
            valid as f64 / total as f64
        } else {
            0.0
        });
        next["evidence_reuse_rate"] = next["local_resolution"]["rate"].clone();
        repos::AnalyticsRepo::new(&self.store).write_model("overview", &next)
    }
}

/// Standard Dashboard API response wrapper (RFC-028 3).
fn dashboard_envelope(
    data: serde_json::Value,
    query: &AnalyticsQuery,
    provenance: DatasetProvenance,
) -> serde_json::Value {
    let ignored_filters: Vec<serde_json::Value> = provenance
        .ignored_filters
        .iter()
        .map(|(filter, reason)| serde_json::json!({ "filter": filter, "reason": reason }))
        .collect();
    serde_json::json!({
        "data": data,
        "meta": {
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "time_range": {
                "from": query.from,
                "to": query.to,
                "timezone": query.timezone.clone().unwrap_or_else(|| "UTC".into()),
            },
            "interval": query.interval.clone().unwrap_or_else(|| "day".into()),
            "aggregation_version": "1",
            "data_freshness": match provenance.query_mode {
                "sql" => "live",
                _ if provenance.refresh_skipped => "fresh",
                _ => "complete",
            },
            "query_mode": provenance.query_mode,
            "applied_filters": provenance.applied_filters,
            "ignored_filters": ignored_filters,
            "refresh_skipped": provenance.refresh_skipped,
            "model_age_seconds": provenance.model_age_seconds,
            "workspace_scope": query.workspace_id.clone().unwrap_or_else(|| "*".into()),
        }
    })
}

/// Memory service over sqlite, with semantic embedding + RRF retrieval
/// (RFC-013 13.3, G-05).
struct SqliteMemoryStore {
    store: Arc<SqliteStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    embedding_provider: String,
    embedding_model: String,
    enabled: bool,
}

/// Normalizes obvious synonyms before feature hashing, so "asynchronous
/// executor" can recall an "async runtime" memory entry.
fn semantic_text(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| match t {
            "asynchronous" => "async",
            "executor" | "scheduler" => "runtime",
            "coroutines" | "coroutine" => "task",
            "message" | "messages" => "msg",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[async_trait::async_trait]
impl searchos_memory::MemoryService for SqliteMemoryStore {
    async fn remember(
        &self,
        namespace: searchos_domain::memory::MemoryNamespace,
        sharing: searchos_domain::memory::SharingPolicy,
        title: &str,
        content: &str,
        url: Option<&str>,
        tags: &[String],
    ) -> Result<MemoryEntry, SearchOsError> {
        if !self.enabled {
            return Err(SearchOsError::Policy("memory is disabled".into()));
        }
        let entry = MemoryEntry {
            id: searchos_domain::ids::MemoryEntryId::generate(),
            namespace,
            sharing,
            title: title.into(),
            content: content.into(),
            url: url.map(|s| s.into()),
            tags: tags.to_vec(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            embedding_status: format!(
                "{}:{}:{}",
                self.embedding_provider,
                self.embedding_model,
                self.embedder.model_id()
            ),
        };
        repos::MemoryRepo::new(&self.store).insert(&entry)?;
        Ok(entry)
    }

    async fn search(
        &self,
        query: &str,
        reader: &searchos_domain::memory::MemoryNamespace,
        top_k: usize,
    ) -> Result<Vec<searchos_memory::MemorySearchHit>, SearchOsError> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        let entries = self.export_internal().await?;
        let mut visible = searchos_memory::NamespaceGuard::visible_entries(&entries, reader);
        // Freshness Check: default retention sweep excludes expired entries.
        let stale: HashSet<String> = searchos_memory::NamespaceGuard::retention_sweep(
            &visible,
            &searchos_memory::RetentionPolicy::default(),
            chrono::Utc::now(),
        )
        .into_iter()
        .collect();
        visible.retain(|e| !stale.contains(&e.id.to_string()));
        if visible.is_empty() {
            return Ok(Vec::new());
        }

        // Metadata filter → lexical BM25-ish + vector → RRF fusion.
        let normalized_query = semantic_text(query);
        let query_terms: Vec<String> = normalized_query
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let entry_texts: Vec<String> = visible
            .iter()
            .map(|e| semantic_text(&format!("{} {}", e.title, e.content)))
            .collect();
        let mut texts = Vec::with_capacity(entry_texts.len() + 1);
        texts.push(normalized_query.clone());
        texts.extend(entry_texts.iter().cloned());
        let mut vectors = self
            .embedder
            .embed(&texts)
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("memory embedding: {e}")))?;
        if vectors.len() != texts.len() {
            return Err(SearchOsError::Retrieval(
                "memory embedding count mismatch".into(),
            ));
        }
        let query_vec = vectors.remove(0);
        let mut lexical: Vec<(usize, f32)> = Vec::new();
        let mut vector: Vec<(usize, f32)> = Vec::new();
        for (idx, text) in entry_texts.iter().enumerate() {
            let terms: Vec<&str> = text.split_whitespace().collect();
            let overlap = query_terms
                .iter()
                .filter(|q| terms.contains(&q.as_str()))
                .count();
            let lexical_score = overlap as f32 / query_terms.len().max(1) as f32;
            if lexical_score > 0.0 {
                lexical.push((idx, lexical_score));
            }
            let vector_score = cosine_similarity(&query_vec, &vectors[idx]);
            if vector_score > 0.02 {
                vector.push((idx, vector_score));
            }
        }
        lexical.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        vector.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut rrf: HashMap<usize, f32> = HashMap::new();
        for (rank, (idx, _)) in lexical.iter().enumerate() {
            *rrf.entry(*idx).or_insert(0.0) += 1.0 / (60.0 + rank as f32 + 1.0);
        }
        for (rank, (idx, _)) in vector.iter().enumerate() {
            *rrf.entry(*idx).or_insert(0.0) += 1.0 / (60.0 + rank as f32 + 1.0);
        }
        let mut hits: Vec<searchos_memory::MemorySearchHit> = rrf
            .into_iter()
            .map(|(idx, score)| searchos_memory::MemorySearchHit {
                entry: visible[idx].clone(),
                score,
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(top_k);
        Ok(hits)
    }

    async fn delete(&self, entry_id: &str) -> Result<(), SearchOsError> {
        let _ = repos::MemoryRepo::new(&self.store).delete(entry_id)?;
        Ok(())
    }

    async fn export(
        &self,
        reader: &searchos_domain::memory::MemoryNamespace,
    ) -> Result<Vec<MemoryEntry>, SearchOsError> {
        let entries = self.export_internal().await?;
        Ok(searchos_memory::NamespaceGuard::visible_entries(
            &entries, reader,
        ))
    }

    async fn stats(&self) -> Result<searchos_memory::MemoryStats, SearchOsError> {
        if !self.enabled {
            return Ok(searchos_memory::MemoryStats::default());
        }
        let entries = self.export_internal().await?;
        let mut by_ws = std::collections::HashMap::new();
        for e in &entries {
            *by_ws
                .entry(e.namespace.workspace.to_string())
                .or_insert(0u64) += 1;
        }
        let mut v: Vec<(String, u64)> = by_ws.into_iter().collect();
        v.sort();
        Ok(searchos_memory::MemoryStats {
            total_entries: entries.len() as u64,
            by_workspace: v,
            with_embedding: entries
                .iter()
                .filter(|e| e.embedding_status != "none")
                .count() as u64,
        })
    }
}

impl SqliteMemoryStore {
    async fn export_internal(&self) -> Result<Vec<MemoryEntry>, SearchOsError> {
        repos::MemoryRepo::new(&self.store).list_all()
    }
}

/// IngestJobPort over sqlite (RFC-029 7): batch / directory job tracking.
struct SqliteIngestJobPort {
    store: Arc<SqliteStore>,
}

#[async_trait::async_trait]
impl searchos_application::ingest_service::IngestJobPort for SqliteIngestJobPort {
    async fn create(
        &self,
        id: &str,
        workspace_id: &str,
        source_type: &str,
        total_items: i64,
    ) -> Result<(), SearchOsError> {
        repos::IngestJobRepo::new(&self.store).create(id, workspace_id, source_type, total_items)
    }

    async fn mark_running(&self, id: &str) -> Result<(), SearchOsError> {
        repos::IngestJobRepo::new(&self.store).mark_running(id)
    }

    async fn finish(
        &self,
        id: &str,
        state: &str,
        ok_items: i64,
        failed_items: i64,
        error_detail: Option<&str>,
    ) -> Result<(), SearchOsError> {
        repos::IngestJobRepo::new(&self.store).finish(
            id,
            state,
            ok_items,
            failed_items,
            error_detail,
        )
    }

    async fn insert_item(
        &self,
        id: &str,
        job_id: &str,
        source_key: &str,
        state: &str,
        document_id: Option<&str>,
        error_detail: Option<&str>,
    ) -> Result<(), SearchOsError> {
        repos::IngestJobRepo::new(&self.store).insert_item(
            id,
            job_id,
            source_key,
            state,
            document_id,
            error_detail,
        )
    }
}

/// DocumentPort over sqlite repos + object store.
struct SqliteDocumentPort {
    store: Arc<SqliteStore>,
    #[allow(dead_code)] // reserved for content GC / retention sweeps
    objects: Arc<LocalObjectStore>,
}

#[async_trait::async_trait]
impl DocumentPort for SqliteDocumentPort {
    async fn find_by_url(
        &self,
        canonical_url: &str,
    ) -> Result<Option<searchos_domain::document::Document>, SearchOsError> {
        repos::DocumentRepo::new(&self.store).find_by_url(canonical_url)
    }

    async fn find_by_id(
        &self,
        id: &searchos_domain::ids::DocumentId,
    ) -> Result<Option<searchos_domain::document::Document>, SearchOsError> {
        repos::DocumentRepo::new(&self.store).get(id)
    }

    async fn save_versioned(
        &self,
        document: &searchos_domain::document::Document,
        version: &searchos_domain::document::DocumentVersion,
        chunks: &[searchos_domain::document::DocumentChunk],
        index_payloads: &[searchos_domain::retrieval::IndexedDocument],
        workspace_id: &searchos_domain::ids::WorkspaceId,
    ) -> Result<searchos_document::VersionDiff, SearchOsError> {
        repos::DocumentRepo::new(&self.store).save_versioned(
            document,
            version,
            chunks,
            index_payloads,
            workspace_id,
        )
    }

    async fn get_current_version(
        &self,
        id: &searchos_domain::ids::DocumentId,
    ) -> Result<Option<searchos_domain::document::DocumentVersion>, SearchOsError> {
        repos::DocumentRepo::new(&self.store).get_current_version(id)
    }

    async fn list_chunks(
        &self,
        version_id: &searchos_domain::ids::DocumentVersionId,
    ) -> Result<Vec<searchos_domain::document::DocumentChunk>, SearchOsError> {
        repos::DocumentRepo::new(&self.store).list_chunks(version_id)
    }

    async fn reindex_current_version(
        &self,
        document_id: &searchos_domain::ids::DocumentId,
        workspace_id: &searchos_domain::ids::WorkspaceId,
    ) -> Result<Vec<searchos_domain::retrieval::IndexedDocument>, SearchOsError> {
        let repo = repos::DocumentRepo::new(&self.store);
        let Some(doc) = repo.get(document_id)? else {
            return Ok(Vec::new());
        };
        let Some(version_id) = doc.current_version_id.clone() else {
            return Ok(Vec::new());
        };
        let stored = repo.list_chunks(&version_id)?;
        if stored.is_empty() {
            return Ok(Vec::new());
        }
        let Some(first) = stored.first() else {
            return Ok(Vec::new());
        };
        let Some(blob) =
            searchos_storage_fs::ObjectStore::get(self.objects.as_ref(), &first.content_ref)
                .await
                .ok()
        else {
            return Ok(Vec::new());
        };
        let text = String::from_utf8_lossy(&blob).to_string();
        let chunks = searchos_document::chunk_text(&text, &Default::default());
        let mut payloads = Vec::with_capacity(stored.len());
        for (i, row) in stored.iter().enumerate() {
            let Some(ch) = chunks.get(i) else { break };
            if ch.hash != row.chunk_hash {
                continue;
            }
            payloads.push(searchos_domain::retrieval::IndexedDocument {
                chunk_id: row.id.clone(),
                document_id: doc.id.clone(),
                version_id: version_id.clone(),
                title: doc.title.clone(),
                body: ch.text.clone(),
                url: doc.canonical_url.clone(),
                language: doc.language.clone(),
                workspace_id: workspace_id.to_string(),
                fetched_at: chrono::Utc::now(),
            });
        }
        Ok(payloads)
    }
}
