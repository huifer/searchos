//! Runtime assembly (RFC-002 core crate responsibilities).

use std::path::Path;
use std::sync::Arc;

use searchos_application::analytics::{AnalyticsEvent, AnalyticsQuery, AnalyticsQueryService};
use searchos_application::fetch_service::{DocumentPort, FetchServiceImpl};
use searchos_application::singleflight::SingleflightCoordinator;
use searchos_application::{CostUseCase, MemoryFacade, MemoryUseCase, SearchOsApplication};
use searchos_cache::ExactQueryCache;

use searchos_decision::trace::TraceCollector;
use searchos_domain::cost::{CostEvent, CostQuery, CostSummary};
use searchos_domain::memory::MemoryEntry;
use searchos_domain::SearchOsError;
use searchos_fetch::{RobotsChecker, UrlGuard};
use searchos_provider::registry::{ProviderRegistry, RegisteredProvider};
use searchos_provider::{BraveProvider, TavilyProvider};
use searchos_retrieval::SearchIndex;
use searchos_storage_fs::LocalObjectStore;
use searchos_storage_sqlite::repos;
use searchos_storage_sqlite::store::SqliteStore;

use crate::config::SearchOsConfig;

/// The assembled runtime.
pub struct SearchOsRuntime {
    pub config: SearchOsConfig,
    pub application: Arc<SearchOsApplication>,
    pub sqlite: Arc<SqliteStore>,
    pub tantivy: Arc<searchos_index_tantivy::TantivyIndex>,
    pub lancedb: Arc<searchos_vector_lancedb::LanceVectorIndex>,
    pub objects: Arc<LocalObjectStore>,
    pub provider_registry: Arc<ProviderRegistry>,
    pub extensions: Arc<searchos_extension::ExtensionRegistry>,
    pub crawlers: Arc<searchos_crawler::CrawlerRegistry>,
    pub guard: Arc<UrlGuard>,
    pub robots: Arc<RobotsChecker>,
    pub exact_cache: Arc<ExactQueryCache>,
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

        // Indexes (rebuildable).
        let tantivy = Arc::new(searchos_index_tantivy::TantivyIndex::open(tantivy_path)?);
        let lancedb = Arc::new(
            searchos_vector_lancedb::LanceVectorIndex::open(lancedb_path, "embeddings").await?,
        );

        // Content store.
        let objects = Arc::new(LocalObjectStore::new(objects_path));

        // Fetch safety.
        let guard = Arc::new(UrlGuard::default());
        let robots = Arc::new(RobotsChecker::new());

        // Document port over sqlite repos.
        let doc_port = Arc::new(SqliteDocumentPort {
            store: Arc::clone(&sqlite),
            objects: objects.clone(),
        });

        // Providers (native extensions compiled in; RFC-011 11.2).
        let registry = Arc::new(ProviderRegistry::new());
        let tavily_key = config.tavily_api_key();
        if config.providers.tavily.enabled {
            if let Some(k) = &tavily_key {
                registry.register(RegisteredProvider::from_search(
                    Arc::new(TavilyProvider::new(k.clone())),
                    5_000,
                    "USD",
                ));
            }
        }
        let brave_key = config.brave_api_key();
        if config.providers.brave.enabled {
            if let Some(k) = &brave_key {
                registry.register(RegisteredProvider::from_search(
                    Arc::new(BraveProvider::new(k.clone())),
                    3_000,
                    "USD",
                ));
            }
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
                        "env:TAVILY_API_KEY,BRAVE_API_KEY", // secret reference, not the key
                    );
                }
            }
        }

        // Registries.
        let extensions = Arc::new(searchos_extension::ExtensionRegistry::new());
        let _ = extensions.discover(&config.app.data_dir.join("extensions"));
        let crawlers = Arc::new(searchos_crawler::CrawlerRegistry::new());
        let _ = crawlers.discover(&config.crawlers_path());

        // Search assembly.
        let exact_cache = Arc::new(ExactQueryCache::new(config.search.cache_ttl_seconds));
        let deps = Arc::new(searchos_application::search_service::SearchDependencies {
            profiles: searchos_policy::ProfileRegistry::builtin(),
            quality: searchos_policy::QualityPolicy::default(),
            freshness: searchos_policy::FreshnessPolicy::default_policy(),
            source: searchos_policy::SourcePolicy::default_policy(),
            cost_config: searchos_cost::CostModelConfig::default_config(),
            bm25: tantivy.clone(),
            router: registry.clone(),
            search_providers: vec![
                Arc::new(TavilyProvider::new(tavily_key.unwrap_or_default())),
                Arc::new(BraveProvider::new(brave_key.unwrap_or_default())),
            ],
            local_port: Arc::new(TantivyLocalPort {
                index: tantivy.clone(),
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
            singleflight: Arc::new(SingleflightCoordinator::new()),
            cache: exact_cache.clone(),
            allow_external: config.tavily_api_key().is_some() || config.brave_api_key().is_some(),
        });

        let search_service: Arc<dyn searchos_application::search_service::SearchService> =
            Arc::new(searchos_application::search_service::SearchServiceImpl::new(deps));
        let fetch_service: Arc<dyn searchos_application::fetch_service::FetchService> = Arc::new(
            FetchServiceImpl::with_guard(guard.clone(), robots.clone(), objects.clone(), doc_port),
        );

        // Memory service backed by sqlite.
        let memory_sqlite = Arc::new(SqliteMemoryStore {
            store: Arc::clone(&sqlite),
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
        // Best-effort initial Read Model build. Failure must not prevent the
        // runtime from starting (RFC-026 2.3).
        let _ = analytics_service.refresh().await;

        let application = Arc::new(SearchOsApplication {
            search_service,
            fetch_service,
            memory_service: memory_service_facade,
            cost_service,
            analytics_service,
        });

        Ok(Arc::new(Self {
            config: config.clone(),
            application,
            sqlite,
            tantivy,
            lancedb,
            objects,
            provider_registry: registry,
            extensions,
            crawlers,
            guard,
            robots,
            exact_cache,
        }))
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
            index: Arc::clone(&self.tantivy),
        })
    }

    /// Index consistency: tantivy chunk count vs sqlite chunk count
    /// (RFC-007 7.8; drift → rebuild guidance).
    pub async fn index_consistency(&self) -> Result<(u64, u64), SearchOsError> {
        let tantivy_count = searchos_retrieval::SearchIndex::count(self.tantivy.as_ref()).await?;
        let sqlite_chunks: i64 = self.sqlite.with_conn(|c| {
            c.query_row("SELECT count(*) FROM document_chunks", [], |r| r.get(0))
                .map_err(|e| SearchOsError::Storage(e.to_string()))
        })?;
        Ok((tantivy_count, sqlite_chunks as u64))
    }

    /// Rebuilds the tantivy index from authoritative document data
    /// (ADR-022): SQLite chunk rows are the metadata; chunk *content* is
    /// reconstructed from the content-addressed object store and re-chunked
    /// deterministically (chunking is deterministic ⇒ identical hashes).
    pub async fn rebuild_index(&self) -> Result<u64, SearchOsError> {
        // (version_id, document_id, url, title, raw_content_ref) per version.
        let versions: Vec<(String, String, String, Option<String>, String)> = self
            .sqlite
            .with_conn(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT DISTINCT dv.id, dv.document_id, d.canonical_url, sr.title, dv.raw_content_ref
                         FROM document_versions dv
                         JOIN documents d ON d.id = dv.document_id
                         LEFT JOIN search_results sr ON sr.url = d.canonical_url
                         WHERE d.status != 'deleted'",
                    )
                    .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, String>(4)?,
                        ))
                    })
                    .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|e| SearchOsError::Storage(e.to_string()))
            })?;

        let mut docs: Vec<searchos_domain::retrieval::IndexedDocument> = Vec::new();
        let now = chrono::Utc::now();
        for (version_id, document_id, url, title, _raw_ref) in versions {
            // Chunk metadata rows (stored ids + hashes).
            let stored: Vec<(String, String, i64)> = self
                .sqlite
                .with_conn(|c| {
                    let mut stmt = c
                        .prepare("SELECT id, chunk_hash, chunk_index FROM document_chunks WHERE version_id = ?1 ORDER BY chunk_index ASC")
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let rows = stmt
                        .query_map([&version_id], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
                        })
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    rows.collect::<Result<Vec<_>, _>>()
                        .map_err(|e| SearchOsError::Storage(e.to_string()))
                })?;
            if stored.is_empty() {
                continue;
            }
            // Chunk text lives in the content-addressed store under the
            // version's raw ref (chunks store the same doc-level ref).
            let chunk_ref: Option<String> = self
                .sqlite
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT content_ref FROM document_chunks WHERE version_id = ?1 LIMIT 1",
                        [&version_id],
                        |r| r.get::<_, String>(0),
                    )
                    .ok())
                })
                .unwrap_or(None);
            let blob: Option<Vec<u8>> = match &chunk_ref {
                Some(r) => searchos_storage_fs::ObjectStore::get(self.objects.as_ref(), r)
                    .await
                    .ok(),
                None => None,
            };
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
                    language: Some("en".into()),
                    workspace_id: "*".into(),
                    fetched_at: now,
                });
            }
        }
        let n = docs.len() as u64;
        self.tantivy.rebuild_from(&docs).await?;
        Ok(n)
    }

    /// Processes pending outbox rows into the tantivy index (RFC-006 6.7).
    /// Returns (processed, dead).
    pub async fn process_outbox(&self, limit: u32) -> Result<(u64, u64), SearchOsError> {
        let outbox = repos::OutboxRepo::new(&self.sqlite);
        let rows = outbox.pending(limit)?;
        let mut processed = 0u64;
        let mut dead = 0u64;
        for row in rows {
            let outcome = if row.operation == "delete" {
                searchos_retrieval::SearchIndex::delete(
                    self.tantivy.as_ref(),
                    &searchos_domain::ids::DocumentId::new(row.entity_id.clone()),
                )
                .await
                .is_ok()
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
                        !url.is_empty()
                            && self
                                .application
                                .fetch_service
                                .fetch_and_persist(url, &searchos_domain::ids::WorkspaceId::new(ws))
                                .await
                                .is_ok()
                    }
                    Err(_) => false,
                }
            } else if let Ok(payload) =
                repos::parse_index_payload(row.payload.as_deref().unwrap_or(""))
            {
                searchos_retrieval::SearchIndex::index(self.tantivy.as_ref(), payload)
                    .await
                    .is_ok()
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
        self.tantivy.commit().await?;
        Ok((processed, dead))
    }
}

/// LocalKnowledgePort over tantivy (application decision input).
struct TantivyLocalPort {
    index: Arc<searchos_index_tantivy::TantivyIndex>,
}

#[async_trait::async_trait]
impl searchos_decision::LocalKnowledgePort for TantivyLocalPort {
    async fn retrieve(
        &self,
        text: &str,
        top_k: usize,
    ) -> Result<Vec<searchos_domain::retrieval::RetrievalCandidate>, SearchOsError> {
        let mut q = searchos_domain::retrieval::RetrievalQuery::new(text, top_k);
        q.workspace_id = Some("*".into());
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
    ) -> Result<(), SearchOsError> {
        repos::IdempotencyRepo::new(&self.store).begin(request_id, task_id, workspace_id)?;
        Ok(())
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

    async fn reserve_budget(
        &self,
        task_id: &str,
        trace_id: &str,
        estimated_micros: i64,
        currency: &str,
    ) -> Result<(), SearchOsError> {
        repos::BudgetRepo::new(&self.store).reserve(
            &searchos_domain::new_id("res"),
            task_id,
            Some(trace_id),
            estimated_micros,
            currency,
        )
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
        let rows = repos::BudgetRepo::new(&self.store).list(limit)?;
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
        // Refresh on read so a fresh gateway always serves complete data.
        // A refresh failure is reported in meta, never as a fake zero.
        let refresh_ok = self.refresh().await.is_ok();
        // `local_resolution` in the Overview must be quality-valid. A local
        // decision that did not pass the final gate is not a valid reuse.
        if model_key == "overview" {
            self.constrain_local_resolution_to_quality_passed().await?;
        }
        let repo = repos::AnalyticsRepo::new(&self.store);
        let data = match model_key {
            "overview" => repo
                .read_model("overview")?
                .unwrap_or(serde_json::Value::Null),
            "search_timeseries" | "knowledge_growth" => {
                serde_json::json!({ "items": repo.read_series(model_key, 90)? })
            }
            key => repo.read_model(key)?.unwrap_or(serde_json::Value::Null),
        };
        Ok(dashboard_envelope(data, query, refresh_ok))
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
    refresh_ok: bool,
) -> serde_json::Value {
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
            "data_freshness": if refresh_ok { "complete" } else { "stale" },
            "workspace_scope": query.workspace_id.clone().unwrap_or_else(|| "*".into()),
        }
    })
}

/// Memory service over sqlite.
struct SqliteMemoryStore {
    store: Arc<SqliteStore>,
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
            embedding_status: "none".into(),
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
        let entries = self.export_internal().await?;
        let visible = searchos_memory::NamespaceGuard::visible_entries(&entries, reader);
        // BM25-ish scoring: simple term overlap (memory search MVP; vector
        // path arrives with semantic reuse phase).
        let mut hits: Vec<searchos_memory::MemorySearchHit> = visible
            .into_iter()
            .map(|e| {
                let text = format!("{} {}", e.title, e.content).to_lowercase();
                let query_lower = query.to_lowercase();
                let terms: Vec<&str> = query_lower.split_whitespace().collect();
                let hits = terms.iter().filter(|t| text.contains(*t)).count();
                let score = if terms.is_empty() {
                    0.0
                } else {
                    hits as f32 / terms.len() as f32
                };
                searchos_memory::MemorySearchHit { entry: e, score }
            })
            .filter(|h| h.score > 0.0)
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

    async fn save_versioned(
        &self,
        document: &searchos_domain::document::Document,
        version: &searchos_domain::document::DocumentVersion,
        chunks: &[searchos_domain::document::DocumentChunk],
        index_payloads: &[searchos_domain::retrieval::IndexedDocument],
    ) -> Result<searchos_document::VersionDiff, SearchOsError> {
        repos::DocumentRepo::new(&self.store).save_versioned(
            document,
            version,
            chunks,
            index_payloads,
        )
    }

    async fn get_current_version(
        &self,
        id: &searchos_domain::ids::DocumentId,
    ) -> Result<Option<searchos_domain::document::DocumentVersion>, SearchOsError> {
        repos::DocumentRepo::new(&self.store).get_current_version(id)
    }
}
