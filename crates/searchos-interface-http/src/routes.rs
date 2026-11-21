//! Axum routes (RFC-012 12.4, RFC-021 21.2 API surface).
//!
//! ```text
//! POST /search          POST /extract          POST /documents
//! POST /ingest           POST /ingest/file      POST /ingest/batch
//! GET  /ingest/jobs/:id  GET  /documents/:id   POST /memory/search    GET  /cost/summary
//! GET  /cost/providers  GET  /stats/search     GET  /decisions/:trace_id
//! GET  /trace/:trace_id GET  /stats/savings    GET  /stats/retrieval
//! GET  /cost/savings     GET /cost/reservations POST /cost/reconcile
//! GET  /profiles         GET /profiles/:id      POST /documents/search
//! GET  /traces           GET /system/status     GET  /health
//! ```
//! Every route is also mounted under `/v1` (RFC-013 13.7). Handlers only
//! authenticate, validate DTOs, call the application, and map responses
//! (RFC-012 12.3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::tavily::{
    TavilyExtractRequest, TavilyExtractResponse, TavilySearchRequest, TavilySearchResponse,
};
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Multipart, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put, MethodRouter};
use axum::{Json, Router};
use futures_util::stream;
use searchos_application::analytics::AnalyticsQuery;
use searchos_application::ingest_service::{IngestBatchItem, IngestPayload, IngestRequest};
use searchos_core::SearchOsRuntime;
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::memory::MemoryNamespace;
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::SearchOsError;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Simple in-process fixed-window rate limiter (RFC-013 13.3).
///
/// Keys are process-local hashes, never raw bearer tokens. The map is
/// bounded: once it grows past [`MAX_TRACKED_KEYS`] only the current
/// minute's entries are retained, so a flood of spoofed Authorization
/// headers cannot grow gateway memory without limit.
/// A Gateway cluster would use a shared store; Local Mode stays dependency-free.
const MAX_TRACKED_KEYS: usize = 4096;

/// Live Dashboard SSE streams. One operator workstation normally holds one
/// browser tab; the cap keeps a reconnect loop or a stuck proxy from pinning
/// an unbounded number of polling tasks (P1-3).
const MAX_SSE_CONNECTIONS: usize = 8;
static ACTIVE_SSE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
/// Cumulative SSE streams since process start (exported as a counter).
static SSE_CONNECTIONS_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Process start, used for the `searchos_uptime_seconds` gauge.
static GATEWAY_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn gateway_uptime_seconds() -> f64 {
    let start = GATEWAY_START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs_f64()
}

#[derive(Debug)]
pub struct RateLimiter {
    per_minute: u32,
    windows: Mutex<HashMap<u64, (u64, u32)>>,
}

impl RateLimiter {
    /// New limiter; `0` disables limiting.
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    fn hash_key(key: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    fn current_window() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() / 60)
            .unwrap_or(0)
    }

    /// Number of tracked client buckets (diagnostics / tests).
    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.windows.lock().unwrap().len()
    }

    /// Drops windows from prior minutes (bounded memory, INV-30-21).
    fn evict_expired(&self) {
        let window = Self::current_window();
        let mut map = self.windows.lock().unwrap();
        let before = map.len();
        map.retain(|_, (seen, _)| *seen == window);
        let evicted = before.saturating_sub(map.len());
        if evicted > 0 {
            searchos_observability::MetricsRegistry::global()
                .inc_counter("ratelimit.windows_evicted_total");
        }
    }

    /// Returns whether this client key may proceed.
    pub fn check(&self, key: &str) -> bool {
        if self.per_minute == 0 {
            return true;
        }
        let window = Self::current_window();
        self.evict_expired();
        let hashed = Self::hash_key(key);
        let mut map = self.windows.lock().unwrap();
        if map.len() >= MAX_TRACKED_KEYS {
            // The cap applies to the live map: drop stale buckets first,
            // and if the current minute alone is already at capacity, drop
            // everything. Either way memory stays bounded; a spoofed-token
            // flood can never grow it without limit (P1-2).
            map.retain(|_, (seen, _)| *seen == window);
            if map.len() >= MAX_TRACKED_KEYS {
                map.clear();
            }
        }
        let entry = map.entry(hashed).or_insert((window, 0));
        if entry.0 != window {
            entry.0 = window;
            entry.1 = 0;
        }
        if entry.1 >= self.per_minute {
            false
        } else {
            entry.1 += 1;
            true
        }
    }
}

/// Shared gateway state.
#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<SearchOsRuntime>,
    /// Bearer token; empty disables auth (local dev).
    pub auth_token: String,
    /// Explicit transport API version (RFC-013 13.7).
    pub api_version: String,
    /// token -> workspace id; empty map means dev mode (no binding).
    pub workspace_tokens: std::collections::BTreeMap<String, String>,
    /// Effective CORS allowlist (`*` or a comma-separated list).
    pub cors_origin: String,
    pub limiter: Arc<RateLimiter>,
    /// Flipped when the server starts shutting down. Long-lived streams (SSE)
    /// subscribe to it so they terminate instead of pinning the connection
    /// drain forever.
    pub shutdown: Arc<tokio::sync::watch::Sender<bool>>,
}

impl AppState {
    /// State over an assembled runtime.
    pub fn new(runtime: Arc<SearchOsRuntime>) -> Self {
        Self::new_with_config(Arc::clone(&runtime), runtime.config.clone())
    }

    /// State over an assembled runtime with an explicit config view. Used by
    /// tests that need a different CORS allowlist or auth settings without
    /// mutating the shared runtime.
    pub fn new_with_config(
        runtime: Arc<SearchOsRuntime>,
        config: searchos_core::SearchOsConfig,
    ) -> Self {
        let cfg = &config.server;
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        Self {
            runtime,
            auth_token: cfg.auth_token.clone(),
            api_version: cfg.api_version.clone(),
            workspace_tokens: cfg.workspace_tokens.clone(),
            cors_origin: cfg.cors_origin.clone(),
            limiter: Arc::new(RateLimiter::new(cfg.rate_limit_per_minute)),
            shutdown: Arc::new(shutdown_tx),
        }
    }

    fn client_key(&self, headers: &HeaderMap) -> String {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("anonymous")
            .to_string()
    }

    /// CORS allowlist from the effective server config.
    pub fn cors_origin(&self) -> String {
        self.cors_origin.clone()
    }

    /// Number of live SSE connection slots (diagnostics / tests).
    pub fn active_sse_connections() -> usize {
        ACTIVE_SSE_CONNECTIONS.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn authorize(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        // Authenticate first: an unauthenticated request must not consume
        // rate-limit budget from a legitimate authenticated client.
        if !self.auth_token.is_empty() {
            let provided = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or("");
            if provided != self.auth_token {
                return Err(ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "UNAUTHORIZED",
                    "invalid bearer token",
                ));
            }
        }
        if !self.limiter.check(&self.client_key(headers)) {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "gateway rate limit exceeded",
            ));
        }
        Ok(())
    }

    /// Workspace authorization: a token bound to workspace A cannot read or
    /// write workspace B (RFC-017 17.6, RFC-013 8.7).
    fn authorize_workspace(&self, headers: &HeaderMap, workspace: &str) -> Result<(), ApiError> {
        if self.workspace_tokens.is_empty() {
            return Ok(());
        }
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        match self.workspace_tokens.get(token) {
            Some(ws) if ws == workspace => Ok(()),
            _ => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "WORKSPACE_ACCESS_DENIED",
                "token is not authorized for this workspace",
            )),
        }
    }

    /// SQL-level read scope. A token bound to ws-a must not read ws-b rows
    /// from routes that do not carry an explicit workspace parameter.
    fn workspace_scope(
        &self,
        headers: &HeaderMap,
        requested: Option<&str>,
    ) -> Result<Option<String>, ApiError> {
        if self.workspace_tokens.is_empty() {
            return Ok(requested.map(|s| s.to_string()));
        }
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        let bound = self.workspace_tokens.get(token).cloned().ok_or_else(|| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                "WORKSPACE_ACCESS_DENIED",
                "token is not authorized for this workspace",
            )
        })?;
        if let Some(req) = requested {
            if req != bound {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "WORKSPACE_ACCESS_DENIED",
                    "token is not authorized for this workspace",
                ));
            }
        }
        Ok(Some(bound))
    }
}

/// Error mapped to the API error envelope (RFC-012 12.7 error mapping).
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn from_domain(e: &SearchOsError) -> Self {
        let class = e.class();
        let status = match e {
            SearchOsError::Validation(_) => StatusCode::BAD_REQUEST,
            SearchOsError::Policy(_) => StatusCode::FORBIDDEN,
            SearchOsError::Budget(_) => StatusCode::PAYMENT_REQUIRED,
            SearchOsError::Security(_) => StatusCode::FORBIDDEN,
            SearchOsError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            SearchOsError::Provider(_) | SearchOsError::Retrieval(_) => StatusCode::BAD_GATEWAY,
            SearchOsError::Storage(_) | SearchOsError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            code: class.code,
            message: e.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        });
        (self.status, Json(body)).into_response()
    }
}

/// The unversioned API surface. Static prefixes are distinct, so no
/// matchit dynamic-segment collisions exist (`/documents/{id}`,
/// `/decisions/{trace_id}`, `/trace/{trace_id}`, `/profiles/{id}`).
/// The unversioned API surface.
///
/// The table is the single source of truth for the HTTP contract: the router
/// is built from it, and the OpenAPI drift test compares `docs/openapi.yaml`
/// against it in both directions. Adding a route without documenting it (or
/// documenting a route that does not exist) fails that test.
#[derive(Clone)]
pub struct RouteSpec {
    pub method: &'static str,
    pub path: String,
    pub handler: MethodRouter<AppState>,
}

impl RouteSpec {
    fn new(method: &'static str, path: &str, handler: MethodRouter<AppState>) -> Self {
        Self {
            method,
            path: path.to_string(),
            handler,
        }
    }

    /// OpenAPI-style path: axum `{id}` segments become `{id}` (already the
    /// same shape), so no rewrite is needed here.
    pub fn spec_path(&self) -> &str {
        &self.path
    }
}

/// Every operational API route. Static prefixes are distinct, so no matchit
/// dynamic-segment collisions exist (`/documents/{id}`,
/// `/decisions/{trace_id}`, `/trace/{trace_id}`, `/profiles/{id}`).
pub fn api_route_table() -> Vec<RouteSpec> {
    vec![
        RouteSpec::new("GET", "/health", get(health)),
        RouteSpec::new("GET", "/metrics", get(metrics)),
        RouteSpec::new("POST", "/search", post(search)),
        // RFC-056: per-result SSE rendering (search contract is shared).
        RouteSpec::new("POST", "/search/stream", post(search_stream)),
        // Agent-facing search surface (plan before spending, follow-up turns).
        RouteSpec::new("POST", "/search/plan", post(search_plan)),
        RouteSpec::new("POST", "/search/follow-up", post(search_follow_up)),
        RouteSpec::new("POST", "/extract", post(extract)),
        // RFC-051: polite cross-host batch fetch through the crawl frontier.
        RouteSpec::new("POST", "/fetch/batch", post(fetch_batch)),
        RouteSpec::new("POST", "/ingest", post(ingest)),
        RouteSpec::new("POST", "/ingest/file", post(ingest_file)),
        RouteSpec::new("POST", "/ingest/batch", post(ingest_batch)),
        RouteSpec::new("GET", "/ingest/jobs/{id}", get(get_ingest_job)),
        RouteSpec::new("POST", "/documents", post(create_document)),
        RouteSpec::new("GET", "/documents", get(list_documents)),
        RouteSpec::new("GET", "/documents/{id}", get(get_document)),
        RouteSpec::new("DELETE", "/documents/{id}", delete(delete_document)),
        // RFC-087: append-only deletion-propagation audit.
        RouteSpec::new("GET", "/deletions", get(list_deletions)),
        // RFC-040/075: search replay and policy simulation (offline).
        RouteSpec::new("POST", "/replay/{trace_id}", post(replay_trace)),
        // RFC-086: batch replay evaluation.
        RouteSpec::new("POST", "/evaluation/replay", post(evaluate_replays)),
        // RFC-030: knowledge collections (workspace-scoped groupings).
        RouteSpec::new("GET", "/collections", get(list_collections)),
        RouteSpec::new("POST", "/collections", post(create_collection)),
        RouteSpec::new(
            "POST",
            "/collections/{id}/documents",
            post(add_collection_document),
        ),
        RouteSpec::new(
            "DELETE",
            "/collections/{id}/documents/{document_id}",
            delete(remove_collection_document),
        ),
        RouteSpec::new(
            "GET",
            "/collections/{id}/documents",
            get(collection_documents),
        ),
        // RFC-060: declared agent identities (audit/disable lifecycle).
        RouteSpec::new("GET", "/agents", get(list_agents)),
        RouteSpec::new("POST", "/agents", post(upsert_agent)),
        RouteSpec::new("GET", "/agents/{id}", get(get_agent)),
        RouteSpec::new("POST", "/agents/{id}/enabled", post(set_agent_enabled)),
        // RFC-058: entity alias registry (read + register + resolve).
        RouteSpec::new("GET", "/entities", get(list_entities)),
        RouteSpec::new("POST", "/entities", post(register_entity)),
        RouteSpec::new("POST", "/entities/resolve", post(resolve_entity)),
        // RFC-071: bounded deep-search research runs + workspace inspection.
        RouteSpec::new("POST", "/research/run", post(research_run)),
        RouteSpec::new("GET", "/research/{session_id}", get(research_workspace)),
        // RFC-070: memory lifecycle (expire + consolidate).
        RouteSpec::new("POST", "/memory/lifecycle", post(memory_lifecycle)),
        // RFC-034: versioned policy management (read + write + history).
        RouteSpec::new("GET", "/policies", get(list_policies)),
        RouteSpec::new(
            "GET",
            "/policies/{kind}/versions",
            get(list_policy_versions),
        ),
        RouteSpec::new("PUT", "/policies/{kind}", put(submit_policy)),
        RouteSpec::new("POST", "/policies/{kind}/rollback", post(rollback_policy)),
        RouteSpec::new(
            "GET",
            "/documents/{id}/versions",
            get(get_document_versions),
        ),
        RouteSpec::new("POST", "/documents/{id}/reindex", post(reindex_document)),
        RouteSpec::new("POST", "/documents/search", post(search_documents)),
        RouteSpec::new("POST", "/memory/search", post(memory_search)),
        RouteSpec::new("GET", "/cost/summary", get(cost_summary)),
        RouteSpec::new("GET", "/cost/providers", get(cost_providers)),
        RouteSpec::new("GET", "/cost/savings", get(cost_savings)),
        RouteSpec::new("GET", "/cost/reservations", get(cost_reservations)),
        // RFC-042: invoice view over the append-only ledger.
        RouteSpec::new("GET", "/cost/invoices", get(cost_invoices)),
        RouteSpec::new("POST", "/cost/reconcile", post(cost_reconcile)),
        RouteSpec::new("GET", "/stats/search", get(stats_search)),
        RouteSpec::new("GET", "/stats/savings", get(stats_savings)),
        RouteSpec::new("GET", "/stats/retrieval", get(stats_retrieval)),
        RouteSpec::new("GET", "/decisions/{trace_id}", get(get_decision)),
        RouteSpec::new("GET", "/trace/{trace_id}", get(get_decision)),
        RouteSpec::new("GET", "/traces", get(list_traces)),
        RouteSpec::new("GET", "/system/status", get(system_status)),
        // RFC-054: durable background jobs (scheduler surface).
        RouteSpec::new("POST", "/jobs", post(enqueue_job)),
        RouteSpec::new("GET", "/jobs", get(list_jobs)),
        RouteSpec::new("GET", "/jobs/{id}", get(get_job)),
        // RFC-063: the federated source catalog (read-only observability).
        RouteSpec::new("GET", "/sources", get(list_sources)),
        // RFC-050: automatic source discovery + promotion.
        RouteSpec::new("POST", "/sources/discover", post(discover_sources)),
        RouteSpec::new("GET", "/sources/discoveries", get(list_source_discoveries)),
        RouteSpec::new(
            "POST",
            "/sources/discoveries/{host}/register",
            post(register_discovered_source),
        ),
        RouteSpec::new(
            "POST",
            "/sources/discoveries/{host}/dismiss",
            post(dismiss_discovered_source),
        ),
        // RFC-049: deterministic structured extraction.
        RouteSpec::new("POST", "/structures", post(extract_structures)),
        // RFC-033: human feedback -> graded qrels -> evaluation dataset.
        RouteSpec::new("POST", "/feedback", post(submit_feedback)),
        RouteSpec::new("GET", "/feedback", get(list_feedback)),
        RouteSpec::new("GET", "/feedback/qrels", get(feedback_qrels)),
        RouteSpec::new("GET", "/feedback/dataset", get(feedback_dataset)),
        // RFC-032: publisher-level trust derived from judged results.
        RouteSpec::new("GET", "/publishers", get(list_publishers)),
        // RFC-035: deterministic quality watchdog (run + append-only history).
        RouteSpec::new("POST", "/watch/run", post(watch_run)),
        RouteSpec::new("GET", "/watch/snapshots", get(watch_snapshots)),
        RouteSpec::new("GET", "/profiles", get(list_profiles)),
        RouteSpec::new("GET", "/profiles/{id}", get(get_profile)),
        // RFC-031 external evidence promotion (read-only observability).
        RouteSpec::new("GET", "/evidence/acquisitions", get(list_acquisitions)),
        RouteSpec::new("GET", "/evidence/acquisitions/{id}", get(get_acquisition)),
        RouteSpec::new("GET", "/evidence/reuse", get(evidence_reuse)),
    ]
}

/// Registers a route table. Entries that share a path are merged into one
/// method router because axum rejects two `.route()` calls for the same path.
fn register_routes(router: Router<AppState>, table: Vec<RouteSpec>) -> Router<AppState> {
    let mut router: Router<AppState> = router;
    let mut index = 0;
    while index < table.len() {
        let path = table[index].path.clone();
        let mut methods = MethodRouter::new();
        while index < table.len() && table[index].path == path {
            methods = methods.merge(table[index].handler.clone());
            index += 1;
        }
        router = router.route(&path, methods);
    }
    router
}

fn api_routes(state: AppState) -> Router {
    register_routes(Router::<AppState>::new(), api_route_table()).with_state(state)
}

/// Builds the router: unversioned routes plus the explicitly versioned
/// `/v1` mount (RFC-013 13.7). No catch-all routes are used.
pub fn router(state: AppState) -> Router {
    let versioned = state.api_version.clone();
    let cors_origin = state.cors_origin();
    let unversioned = api_routes(state.clone());
    let v1 = api_routes(state.clone());
    // RFC-028 3: dedicated Dashboard API, backed by the Analytics Read
    // Model. Full paths are used so no nested router duplicates a shorter
    // path from the operational API (also keeps the route scanner clean).
    let dashboard = dashboard_routes(state);
    Router::new()
        .merge(unversioned)
        .nest("/v1", v1)
        .merge(dashboard)
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024))
        .layer(from_fn(move |req: Request, next: Next| {
            let version = versioned.clone();
            let origin = cors_origin.clone();
            async move {
                let allowed_origin = if origin.is_empty() {
                    None
                } else {
                    // `cors_origin` accepts `*` or a comma-separated allowlist
                    // so local dev, staging and production can share one config.
                    let origins: Vec<&str> = origin
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect();
                    req.headers()
                        .get("origin")
                        .and_then(|v| v.to_str().ok())
                        .filter(|o| {
                            origins
                                .iter()
                                .any(|allowed| *allowed == "*" || allowed == o)
                        })
                        .map(|o| o.to_string())
                };
                let mut resp = if req.method() == Method::OPTIONS {
                    axum::http::Response::builder()
                        .status(StatusCode::NO_CONTENT)
                        .body(Body::empty())
                        .unwrap()
                } else {
                    next.run(req).await
                };
                if let Ok(v) = HeaderValue::from_str(&version) {
                    resp.headers_mut().insert("x-searchos-api-version", v);
                }
                if let Some(o) = allowed_origin {
                    if let Ok(v) = HeaderValue::from_str(&o) {
                        resp.headers_mut().insert("access-control-allow-origin", v);
                    }
                    resp.headers_mut().insert(
                        "access-control-allow-headers",
                        HeaderValue::from_static("authorization, content-type"),
                    );
                    resp.headers_mut().insert(
                        "access-control-allow-methods",
                        HeaderValue::from_static("GET, POST, OPTIONS"),
                    );
                    resp.headers_mut()
                        .insert("vary", HeaderValue::from_static("Origin"));
                }
                resp
            }
        }))
}

/// Signals received since process start. The first starts a graceful
/// shutdown; the second forces an immediate exit.
static SHUTDOWN_SIGNALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Installs SIGTERM / SIGINT handlers.
///
/// The first signal starts a graceful shutdown (stop accepting, drain
/// in-flight requests, flush the outbox); a second signal forces an immediate
/// exit so an operator can always recover a wedged process.
fn install_shutdown_handlers(tx: tokio::sync::watch::Sender<bool>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        for kind in [SignalKind::terminate(), SignalKind::interrupt()] {
            if let Ok(mut stream) = signal(kind) {
                let tx = tx.clone();
                tokio::spawn(async move {
                    while stream.recv().await.is_some() {
                        let count = SHUTDOWN_SIGNALS.fetch_add(1, Ordering::SeqCst) + 1;
                        let _ = tx.send(true);
                        if count >= 2 {
                            tracing::warn!("second shutdown signal received; forcing exit");
                            std::process::exit(0);
                        }
                    }
                });
            }
        }
    }
    #[cfg(not(unix))]
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let count = SHUTDOWN_SIGNALS.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = tx.send(true);
                if count >= 2 {
                    tracing::warn!("second shutdown signal received; forcing exit");
                    std::process::exit(0);
                }
            }
        });
    }
}

/// Resolves once the first shutdown signal arrives.
async fn shutdown_signal(mut rx: tokio::sync::watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    let _ = rx.changed().await;
}

/// The background outbox worker (RFC-006 6.7): fetched documents must reach
/// the full-text index without blocking the request path.
///
/// Runs until `stop` is signalled, then performs one final drain so work
/// enqueued just before shutdown is not lost. Failure is logged and retried on
/// the next tick; the worker never panics the gateway.
/// RFC-035: runs the watch on a fixed interval until the gateway stops.
///
/// A tick that fails (analytics unavailable, dataset unreadable) is LOGGED
/// and the loop continues: a broken tick must not silence the schedule. The
/// run itself already records `degraded`/`skipped` honestly, so the schedule
/// adds cadence, not judgement.
pub async fn run_watch_scheduler(
    runtime: Arc<SearchOsRuntime>,
    interval_seconds: u64,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let period = Duration::from_secs(interval_seconds.max(1));
    let mut ticker = tokio::time::interval(period);
    // Skip the immediate first tick: an operator restarting the gateway
    // should not trigger an instant evaluation storm.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.changed() => {
                if *stop.borrow() {
                    break;
                }
                continue;
            }
        }
        let Some(watch) = runtime.application.watch_service.as_ref() else {
            break;
        };
        match watch.run().await {
            Ok(report) => {
                if report.status != searchos_application::watch::WatchStatus::Pass {
                    tracing::warn!(
                        status = report.status.as_str(),
                        "scheduled search watch finished without a pass"
                    );
                } else {
                    tracing::debug!("scheduled search watch passed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "scheduled search watch failed"),
        }
    }
    tracing::info!("search watch scheduler stopped");
}

pub(crate) async fn run_outbox_worker(
    runtime: Arc<SearchOsRuntime>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_millis(1_500));
    let mut capture_ticker = tokio::time::interval(Duration::from_millis(1_500));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = runtime.process_outbox(100).await {
                    tracing::warn!(error = %e, "index outbox worker tick failed");
                }
            }
            // RFC-031: captures run on their own worker so a slow page fetch
            // never delays index-write draining (AC16).
            _ = capture_ticker.tick() => {
                if let Err(e) = runtime.process_capture_queue(4).await {
                    tracing::warn!(error = %e, "capture worker tick failed");
                }
            }
            _ = stop.changed() => {
                if *stop.borrow() {
                    break;
                }
            }
        }
    }
    match runtime.process_outbox(500).await {
        Ok((processed, dead)) if processed > 0 || dead > 0 => {
            tracing::info!("shutdown drain: {processed} outbox row(s) processed, {dead} dead");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "shutdown drain failed"),
    }
    // Final capture drain.
    if let Err(e) = runtime.process_capture_queue(64).await {
        tracing::warn!(error = %e, "shutdown capture drain failed");
    }
}

/// RFC-054: durable background-job worker tick loop. Shared with the outbox
/// worker's shape: log-and-continue on failure, one final drain on shutdown.
pub(crate) async fn run_job_worker(
    runtime: Arc<SearchOsRuntime>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_millis(1_000));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                match runtime.process_jobs(8).await {
                    Ok((0, 0)) => {}
                    Ok((done, failed)) => {
                        tracing::debug!(done, failed, "background jobs processed");
                    }
                    Err(e) => tracing::warn!(error = %e, "background job worker tick failed"),
                }
            }
            _ = stop.changed() => {
                if *stop.borrow() {
                    break;
                }
            }
        }
    }
    // Final drain: finish anything claimable within the shutdown window.
    match runtime.process_jobs(64).await {
        Ok((done, failed)) if done > 0 || failed > 0 => {
            tracing::info!("shutdown job drain: {done} completed, {failed} failed");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "shutdown job drain failed"),
    }
}

/// Binds and serves the gateway.
///
/// Graceful shutdown (RFC-015 15.4): on SIGTERM/SIGINT the listener stops
/// accepting, in-flight requests drain, SSE streams close, and the outbox
/// worker performs a final flush so documents fetched just before shutdown
/// reach the full-text index. A second signal forces an immediate exit.
pub async fn serve(state: AppState) -> Result<(), String> {
    let host = state.runtime.config.server.host.clone();
    let port = state.runtime.config.server.port;
    let _ = GATEWAY_START.set(std::time::Instant::now());

    // `stop` is set once the HTTP connections have drained, telling the
    // worker to flush and exit. `state.shutdown` is flipped by the signal
    // handlers and is what long-lived SSE streams watch.
    let (stop_tx, stop_rx_worker) = tokio::sync::watch::channel(false);
    install_shutdown_handlers((*state.shutdown).clone());

    // Background outbox worker (RFC-006 6.7): fetched documents must reach
    // the full-text index without blocking the request path. Failure is
    // logged and retried on the next tick; the worker never panics the
    // gateway. On shutdown it performs one final drain.
    let worker_runtime = Arc::clone(&state.runtime);
    let worker = tokio::spawn(async move {
        run_outbox_worker(worker_runtime, stop_rx_worker).await;
    });

    // RFC-054: durable background-job worker. It only acts on explicitly
    // enqueued rows, so an always-on tick is safe; unknown kinds fail
    // terminally instead of retrying forever.
    let (job_stop_tx, job_stop_rx) = tokio::sync::watch::channel(false);
    let job_runtime = Arc::clone(&state.runtime);
    let job_worker = tokio::spawn(async move {
        run_job_worker(job_runtime, job_stop_rx).await;
    });

    // RFC-035: optional built-in watch schedule. `schedule_interval_seconds
    // = 0` (the default) keeps the watchdog on cron/CI instead, so the
    // gateway never surprises an operator with background evaluations.
    let watch_interval = state.runtime.config.watch.schedule_interval_seconds;
    let watch_worker = if watch_interval > 0 {
        let (watch_stop_tx, watch_stop_rx) = tokio::sync::watch::channel(false);
        let watch_runtime = Arc::clone(&state.runtime);
        let handle = tokio::spawn(async move {
            run_watch_scheduler(watch_runtime, watch_interval, watch_stop_rx).await;
        });
        Some((watch_stop_tx, handle))
    } else {
        None
    };

    let app = router(state.clone());
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    tracing::info!("gateway listening on http://{addr}");

    let begin_rx_serve = state.shutdown.subscribe();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(begin_rx_serve))
        .await;

    tracing::info!("shutdown signal received; draining connections and the outbox");
    // Connections are drained: stop the worker with its final flush.
    let _ = stop_tx.send(true);
    match tokio::time::timeout(Duration::from_secs(5), worker).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("outbox worker task failed: {e}"),
        Err(_) => tracing::warn!("timed out waiting for the outbox worker to stop"),
    }
    // RFC-035: stop the watch schedule (it exits on the next select).
    if let Some((watch_stop_tx, watch_handle)) = watch_worker {
        let _ = watch_stop_tx.send(true);
        match tokio::time::timeout(Duration::from_secs(5), watch_handle).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("watch scheduler task failed: {e}"),
            Err(_) => tracing::warn!("timed out waiting for the watch scheduler to stop"),
        }
    }
    // RFC-054: stop the job worker with a final drain.
    let _ = job_stop_tx.send(true);
    match tokio::time::timeout(Duration::from_secs(5), job_worker).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("job worker task failed: {e}"),
        Err(_) => tracing::warn!("timed out waiting for the job worker to stop"),
    }
    searchos_observability::MetricsRegistry::global().inc_counter("shutdown.graceful_total");
    tracing::info!("gateway shut down cleanly");
    result.map_err(|e| format!("serve: {e}"))
}

// ---------------------------------------------------------------- handlers

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let (index_chunks, sqlite) = state.runtime.index_consistency().await.unwrap_or((0, 0));
    Json(serde_json::json!({
        "status": "ok",
        "version": searchos_core::VERSION,
        "index": {
            "fulltext_backend": state.runtime.fulltext_backend(),
            "fulltext_chunks": index_chunks,
            "tantivy_chunks": index_chunks,
            "sqlite_chunks": sqlite,
            "consistent": index_chunks == sqlite
        },
        "providers": state.runtime.provider_registry.ids(),
    }))
}

/// `GET /metrics` — Prometheus text exposition format 0.0.4.
///
/// Same authentication as every other endpoint (a scraper supplies the bearer
/// token). Server-level gauges (uptime, live SSE streams, pending outbox rows,
/// schema version) are joined with the process-wide counters, so a single
/// scrape shows both execution correctness and process health.
async fn metrics(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    state.authorize(&headers)?;
    let snapshot = searchos_observability::MetricsRegistry::global().snapshot();
    let (outbox_pending, _, _) =
        searchos_storage_sqlite::repos::OutboxRepo::new(&state.runtime.sqlite)
            .counts()
            .map_err(|e| ApiError::from_domain(&e))?;
    let schema_version = state.runtime.sqlite.schema_version().unwrap_or(0);
    let server = searchos_observability::ServerMetrics {
        uptime_seconds: gateway_uptime_seconds(),
        sse_active: ACTIVE_SSE_CONNECTIONS.load(std::sync::atomic::Ordering::Relaxed) as u64,
        sse_connections_total: SSE_CONNECTIONS_TOTAL.load(std::sync::atomic::Ordering::Relaxed),
        outbox_pending: outbox_pending.max(0) as u64,
        version: searchos_core::VERSION,
        schema_version: schema_version.max(0) as u64,
    };
    let body = searchos_observability::render_prometheus(&snapshot, &server);
    Ok((
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response())
}

/// Search request DTO (RFC-012 12.5).
#[derive(Debug, serde::Deserialize)]
pub struct SearchDto {
    pub query: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub max_external_queries: Option<u32>,
    /// Bounded iterative acquisition rounds (default 2, 1 disables).
    #[serde(default)]
    pub max_acquisition_rounds: Option<u32>,
    #[serde(default)]
    pub max_total_cost: Option<f64>,
    /// Client idempotency key (RFC-014 14.7).
    #[serde(default)]
    pub request_id: Option<String>,
    /// Optional output cap, applied after the quality gate.
    #[serde(default)]
    pub max_results: Option<u32>,
    /// RFC-057: historical view instant (RFC3339). Freshness is judged as
    /// of this moment; documents fetched later are excluded.
    #[serde(default)]
    pub as_of: Option<String>,
    /// Agent session id: turns are recorded so a follow-up can target the
    /// requirements the previous turn left open.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Explicit focus requirements for a follow-up turn.
    #[serde(default)]
    pub focus_requirements: Option<Vec<String>>,
    /// Caller-declared context budget (RFC-067/068). When present the
    /// response carries an Agent-facing `context` block assembled under it.
    #[serde(default)]
    pub context_budget: Option<searchos_domain::context::ContextBudget>,
}

/// Tavily-shaped bodies are routed through the compatibility layer; plain
/// `{ "query": ... }` bodies stay on the native SearchOS contract.
fn is_tavily_request(body: &serde_json::Value) -> bool {
    let Some(object) = body.as_object() else {
        return false;
    };
    [
        "search_depth",
        "topic",
        "include_answer",
        "include_raw_content",
        "include_domains",
        "exclude_domains",
        "include_images",
        "include_image_descriptions",
        "chunks_per_source",
        "time_range",
        "country",
        "include_favicon",
        "days",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
}

/// Tavily's extract endpoint sends `{ "urls": [...] }`; the native
/// SearchOS extract takes a single `url`. Detecting the array keeps the two
/// contracts from being confused.
fn is_tavily_extract_request(body: &serde_json::Value) -> bool {
    body.as_object()
        .map(|object| object.contains_key("urls"))
        .unwrap_or(false)
}

/// `POST /search/plan` — plan-only: what WOULD be searched, at what cost.
/// No external call, no billing, no document write.
async fn search_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    state.authorize(&headers)?;
    let dto: SearchDto = serde_json::from_value(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", e.to_string()))?;
    if dto.query.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "query must not be empty",
        ));
    }
    state.authorize_workspace(&headers, dto.workspace_id.as_deref().unwrap_or("default"))?;
    let mode = match dto.mode.as_deref() {
        None => SearchMode::LocalFirst,
        Some(value) => SearchMode::parse(value).map_err(|message| {
            ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", message)
        })?,
    };
    let profile = dto
        .profile
        .clone()
        .map(ProfileId::new)
        .unwrap_or_else(ProfileId::general);
    let ws = dto
        .workspace_id
        .clone()
        .map(WorkspaceId::new)
        .unwrap_or_else(|| WorkspaceId::new("default"));
    let mut req =
        SearchRequest::new(dto.query, profile, mode, ws).map_err(|e| ApiError::from_domain(&e))?;
    if let Some(focus) = dto.focus_requirements.as_ref() {
        req.focus_requirements = focus.clone();
    }
    if let Some(as_of) = dto.as_of.as_deref() {
        let parsed = chrono::DateTime::parse_from_rfc3339(as_of)
            .map(|t| t.with_timezone(&chrono::Utc))
            .map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "VALIDATION_FAILED",
                    "invalid as_of",
                )
            })?;
        req.constraints.as_of = Some(parsed);
    }
    let report = state
        .runtime
        .application
        .search_service
        .plan(&req)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok((StatusCode::OK, Json(serde_json::json!(report))).into_response())
}

/// `POST /search/follow-up` — continue a session, focusing on the
/// requirements the previous turn left open.
async fn search_follow_up(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    state.authorize(&headers)?;
    let dto: SearchDto = serde_json::from_value(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", e.to_string()))?;
    let session_id = dto.session_id.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "session_id is required for a follow-up",
        )
    })?;
    let session = state
        .runtime
        .session_store
        .get(&session_id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "NOT_FOUND", "unknown session_id"))?;
    // The follow-up targets the open requirements of the previous turn
    // unless the caller narrows them explicitly.
    let focus = if dto
        .focus_requirements
        .as_ref()
        .is_some_and(|f| !f.is_empty())
    {
        dto.focus_requirements.clone().unwrap_or_default()
    } else {
        session
            .open_requirements
            .iter()
            .map(|g| g.question.clone())
            .collect()
    };
    let query = if dto.query.trim().is_empty() {
        session
            .turns
            .last()
            .map(|t| t.query.clone())
            .unwrap_or_default()
    } else {
        dto.query.clone()
    };
    if query.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "query must not be empty",
        ));
    }
    let mode = match dto.mode.as_deref() {
        None => SearchMode::LocalFirst,
        Some(value) => SearchMode::parse(value).map_err(|message| {
            ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", message)
        })?,
    };
    let profile = dto
        .profile
        .clone()
        .map(ProfileId::new)
        .unwrap_or_else(ProfileId::general);
    let ws = dto
        .workspace_id
        .clone()
        .map(WorkspaceId::new)
        .unwrap_or_else(|| WorkspaceId::new("default"));
    let mut req =
        SearchRequest::new(query, profile, mode, ws).map_err(|e| ApiError::from_domain(&e))?;
    req.session_id = Some(session_id);
    req.focus_requirements = focus;
    if let Some(n) = dto.max_external_queries {
        req.budget.max_external_queries = n;
    }
    if let Some(n) = dto.max_acquisition_rounds {
        req.budget.max_acquisition_rounds = n.max(1);
    }
    let resp = state
        .runtime
        .application
        .search_service
        .search(req)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok((StatusCode::OK, Json(serde_json::json!(resp))).into_response())
}

async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    state.authorize(&headers)?;
    if is_tavily_request(&body) {
        return tavily_search(state, headers, body)
            .await
            .map(IntoResponse::into_response);
    }
    let dto: SearchDto = serde_json::from_value(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", e.to_string()))?;
    if dto.query.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "query must not be empty",
        ));
    }
    state.authorize_workspace(&headers, dto.workspace_id.as_deref().unwrap_or("default"))?;
    let mode = match dto.mode.as_deref() {
        None => SearchMode::LocalFirst,
        Some(value) => SearchMode::parse(value).map_err(|message| {
            ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", message)
        })?,
    };
    let profile = dto
        .profile
        .clone()
        .map(ProfileId::new)
        .unwrap_or_else(ProfileId::general);
    let ws = dto
        .workspace_id
        .clone()
        .map(WorkspaceId::new)
        .unwrap_or_else(|| WorkspaceId::new("default"));
    let mut req =
        SearchRequest::new(dto.query, profile, mode, ws).map_err(|e| ApiError::from_domain(&e))?;
    if let Some(n) = dto.max_external_queries {
        req.budget.max_external_queries = n;
    }
    if let Some(n) = dto.max_acquisition_rounds {
        req.budget.max_acquisition_rounds = n.max(1);
    }
    if let Some(sid) = dto.session_id.as_deref() {
        if !sid.trim().is_empty() {
            req.session_id = Some(sid.to_string());
        }
    }
    if let Some(focus) = dto.focus_requirements.as_ref() {
        req.focus_requirements = focus.clone();
    }
    if let Some(budget) = dto.context_budget.clone() {
        if let Err(message) = budget.validate() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "VALIDATION_FAILED",
                message,
            ));
        }
        req.context_budget = Some(budget);
    }
    if let Some(cost) = dto.max_total_cost {
        req.budget.max_total_cost_micros = (cost * 1_000_000.0).round() as i64;
    }
    if let Some(as_of) = dto.as_of.as_deref() {
        let parsed = chrono::DateTime::parse_from_rfc3339(as_of)
            .map(|t| t.with_timezone(&chrono::Utc))
            .map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "VALIDATION_FAILED",
                    "invalid as_of",
                )
            })?;
        req.constraints.as_of = Some(parsed);
    }
    if let Some(rid) = dto.request_id.as_deref() {
        req = req.with_request_id(rid);
    }
    if let Some(max) = dto.max_results {
        req = req.with_max_results(max);
    }
    let mut resp = state
        .runtime
        .application
        .search_service
        .search(req)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    if let Some(max) = dto.max_results {
        resp.results
            .truncate(searchos_domain::request::clamp_max_results(max) as usize);
    }
    let value = serde_json::to_value(resp).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            e.to_string(),
        )
    })?;
    Ok(Json(value).into_response())
}

/// `POST /search/stream` — RFC-056 per-result SSE stream.
///
/// The search pipeline currently computes the full gated response before the
/// first evidence event. Once available, results are emitted individually so
/// clients can render them progressively; this does not claim stage/token
/// streaming.
async fn search_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    let response = search(State(state), headers, Json(body)).await?;
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                e.to_string(),
            )
        })?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            e.to_string(),
        )
    })?;
    let mut events = Vec::new();
    events.push((
        "start",
        serde_json::json!({"task_id": value.get("task_id"), "status": value.get("status")}),
    ));
    if let Some(results) = value.get("results").and_then(|v| v.as_array()) {
        for (index, result) in results.iter().enumerate() {
            events.push((
                "result",
                serde_json::json!({"index": index, "result": result}),
            ));
        }
    }
    events.push((
        "quality",
        serde_json::json!({
            "quality": value.get("quality"),
            "status": value.get("status"),
            "warnings": value.get("warnings"),
            "coverage_gaps": value.get("coverage_gaps"),
        }),
    ));
    events.push(("final", value));
    events.push(("done", serde_json::json!({"complete": true})));
    let stream = futures_util::stream::iter(events.into_iter().map(|(event, data)| {
        let data = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
        Ok::<SseEvent, std::convert::Infallible>(SseEvent::default().event(event).data(data))
    }));
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// Tavily-compatible `/search` path (RFC-012 12.7, ADR-006). Tavily DTOs
/// never enter the core; unsupported knobs are reported back explicitly.
async fn tavily_search(
    state: AppState,
    headers: HeaderMap,
    body: serde_json::Value,
) -> Result<Json<TavilySearchResponse>, ApiError> {
    let started = Instant::now();
    let dto: TavilySearchRequest = serde_json::from_value(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", e.to_string()))?;
    if dto.query.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "query must not be empty",
        ));
    }
    let workspace = dto.workspace_id.as_deref().unwrap_or("tavily");
    state.authorize_workspace(&headers, workspace)?;
    let (mut req, unsupported) = dto
        .to_domain()
        .map_err(|message| ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", message))?;
    // An explicit workspace keeps token-bound deployments consistent; the
    // compat layer still defaults to the dedicated `tavily` workspace.
    if dto.workspace_id.is_some() {
        req.workspace_id = WorkspaceId::new(workspace);
    }
    let resp = state
        .runtime
        .application
        .search_service
        .search(req)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(TavilySearchResponse::from_domain(
        &resp,
        dto.effective_max_results(),
        unsupported,
        started.elapsed().as_millis() as u64,
    )))
}

/// Extract request DTO.
#[derive(Debug, serde::Deserialize)]
pub struct ExtractDto {
    pub url: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// When false only previews (no persistence).
    #[serde(default)]
    pub persist: Option<bool>,
}

/// `POST /fetch/batch` — RFC-051 batch fetch through the crawl frontier.
#[derive(Debug, serde::Deserialize)]
pub struct FetchBatchDto {
    pub urls: Vec<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub persist: Option<bool>,
    #[serde(default)]
    pub max_urls: Option<u32>,
    #[serde(default)]
    pub max_per_host: Option<usize>,
    #[serde(default)]
    pub default_interval_ms: Option<u64>,
    #[serde(default)]
    pub max_total_ms: Option<u64>,
}

async fn fetch_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(dto): Json<FetchBatchDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    if dto.urls.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "urls must not be empty",
        ));
    }
    let ws = dto
        .workspace_id
        .clone()
        .map(WorkspaceId::new)
        .unwrap_or_else(|| WorkspaceId::new("default"));
    state.authorize_workspace(&headers, ws.as_str())?;
    let mut options = searchos_application::FetchManyOptions::default();
    if let Some(v) = dto.persist {
        options.persist = v;
    }
    if let Some(v) = dto.max_urls {
        options.max_urls = v.clamp(1, 200);
    }
    if let Some(v) = dto.max_per_host {
        options.max_per_host = v.clamp(1, 1_024);
    }
    if let Some(v) = dto.default_interval_ms {
        options.default_interval_ms = v.max(1);
    }
    if let Some(v) = dto.max_total_ms {
        options.max_total_ms = v.max(1);
    }
    let report = state
        .runtime
        .application
        .fetch_service
        .fetch_many(&dto.urls, &ws, options)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::to_value(report).unwrap_or_default()))
}

async fn extract(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    state.authorize(&headers)?;
    if is_tavily_extract_request(&body) {
        return tavily_extract(state, headers, body)
            .await
            .map(IntoResponse::into_response);
    }
    let dto: ExtractDto = serde_json::from_value(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", e.to_string()))?;
    let ws = dto
        .workspace_id
        .clone()
        .map(WorkspaceId::new)
        .unwrap_or_else(|| WorkspaceId::new("default"));
    state.authorize_workspace(&headers, ws.as_str())?;
    if dto.persist.unwrap_or(true) {
        let doc = state
            .runtime
            .application
            .fetch_service
            .fetch_and_persist(&dto.url, &ws)
            .await
            .map_err(|e| ApiError::from_domain(&e))?;
        Ok(Json(serde_json::to_value(doc).unwrap_or_default()).into_response())
    } else {
        let content = state
            .runtime
            .application
            .fetch_service
            .fetch_preview(&dto.url)
            .await
            .map_err(|e| ApiError::from_domain(&e))?;
        Ok(Json(serde_json::json!({
            "url": dto.url,
            "title": content.title,
            "content": content.text,
            "markdown": content.markdown,
        }))
        .into_response())
    }
}

/// Tavily-compatible `/extract` (RFC-012 12.7). SearchOS extracts one URL per
/// call: the first URL is honored, the rest are returned as explicit failures
/// rather than being silently dropped.
async fn tavily_extract(
    state: AppState,
    headers: HeaderMap,
    body: serde_json::Value,
) -> Result<Json<TavilyExtractResponse>, ApiError> {
    let started = Instant::now();
    let dto: TavilyExtractRequest = serde_json::from_value(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", e.to_string()))?;
    if dto.urls.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "urls must not be empty",
        ));
    }
    let ws = WorkspaceId::new(dto.workspace_id.as_deref().unwrap_or("default"));
    state.authorize_workspace(&headers, ws.as_str())?;
    let mut results = Vec::new();
    let mut failed = Vec::new();
    let first = dto.urls[0].clone();
    match state
        .runtime
        .application
        .fetch_service
        .fetch_and_persist(&first, &ws)
        .await
    {
        Ok(doc) => results.push(crate::tavily::TavilyExtractResult {
            url: first,
            title: doc.title.clone(),
            raw_content: None,
        }),
        Err(e) => failed.push(crate::tavily::TavilyExtractFailure {
            url: first,
            error: e.to_string(),
        }),
    }
    for url in dto.urls.iter().skip(1) {
        failed.push(crate::tavily::TavilyExtractFailure {
            url: url.clone(),
            error: "not processed in this call: SearchOS extracts one URL per call".into(),
        });
    }
    Ok(Json(TavilyExtractResponse {
        results,
        failed_results: failed,
        unsupported_features: dto.unsupported(dto.urls.len().saturating_sub(1)),
        response_time: started.elapsed().as_secs_f64(),
    }))
}

/// Ingest text DTO (POST /ingest, RFC-029 6.1).
#[derive(Debug, serde::Deserialize)]
pub struct IngestDto {
    pub content: String,
    #[serde(default)]
    pub source_key: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

fn ingest_request_from_dto(dto: &IngestDto) -> IngestRequest {
    let mut req = IngestRequest::new(WorkspaceId::new(
        dto.workspace_id.clone().unwrap_or_else(|| "default".into()),
    ));
    req.source_key = dto.source_key.clone();
    req.title = dto.title.clone();
    req.content_type = dto.content_type.clone();
    req.language = dto.language.clone();
    req.metadata = dto.metadata.clone().unwrap_or(serde_json::Value::Null);
    req
}

/// `POST /ingest` — import pasted / typed text (RFC-029 6.1).
async fn ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(dto): Json<IngestDto>,
) -> Result<Response, ApiError> {
    state.authorize(&headers)?;
    let ws = dto.workspace_id.clone().unwrap_or_else(|| "default".into());
    state.authorize_workspace(&headers, &ws)?;
    let req = ingest_request_from_dto(&dto);
    let doc = state
        .runtime
        .application
        .ingest_service
        .ingest_text(req, &dto.content)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(doc).unwrap_or_default()),
    )
        .into_response())
}

/// `POST /ingest/file` — multipart file upload (RFC-029 6.1). The client
/// filename is a label only; it never touches the server filesystem.
async fn ingest_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    state.authorize(&headers)?;
    let mut bytes: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = None;
    let mut source_key: Option<String> = None;
    let mut title: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut language: Option<String> = None;
    let mut workspace_id = "default".to_string();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "INVALID_MULTIPART", e.to_string()))?
    {
        let name = field.name().map(|s| s.to_string());
        let field_name = field.file_name().map(|s| s.to_string());
        let data = field.bytes().await.map_err(|e| {
            ApiError::new(StatusCode::BAD_REQUEST, "INVALID_MULTIPART", e.to_string())
        })?;
        match name.as_deref() {
            Some("file") => {
                file_name = field_name;
                bytes = Some(data.to_vec());
            }
            Some("source_key") => source_key = Some(String::from_utf8_lossy(&data).to_string()),
            Some("title") => title = Some(String::from_utf8_lossy(&data).to_string()),
            Some("content_type") => content_type = Some(String::from_utf8_lossy(&data).to_string()),
            Some("language") => language = Some(String::from_utf8_lossy(&data).to_string()),
            Some("workspace_id") => workspace_id = String::from_utf8_lossy(&data).to_string(),
            _ => {}
        }
    }
    state.authorize_workspace(&headers, &workspace_id)?;
    let bytes = bytes.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "EMPTY_CONTENT",
            "missing 'file' field",
        )
    })?;
    let mut req = IngestRequest::new(WorkspaceId::new(&workspace_id));
    req.source_key = source_key;
    req.title = title;
    req.content_type = content_type;
    req.language = language;
    let doc = state
        .runtime
        .application
        .ingest_service
        .ingest_upload(req, &bytes, file_name.as_deref())
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(doc).unwrap_or_default()),
    )
        .into_response())
}

/// Batch ingest DTO (POST /ingest/batch, RFC-029 6.1).
#[derive(Debug, serde::Deserialize)]
pub struct IngestBatchDto {
    #[serde(default)]
    pub items: Vec<IngestDto>,
    /// Directory / bulk variant: in-memory file contents (a server-side
    /// filesystem path is never accepted from HTTP).
    #[serde(default)]
    pub directory: Option<DirectoryIngestDto>,
}

/// In-memory directory batch description.
#[derive(Debug, serde::Deserialize)]
pub struct DirectoryIngestDto {
    /// Files in the virtual directory; each carries its own request metadata.
    pub files: Vec<DirectoryFileDto>,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub extensions: Vec<String>,
}

/// One file entry of a directory batch.
#[derive(Debug, serde::Deserialize)]
pub struct DirectoryFileDto {
    #[serde(default)]
    pub source_key: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    /// File name label; never touches the server filesystem.
    pub file_name: String,
    /// Raw UTF-8 text content.
    pub content: String,
}

/// `POST /ingest/batch` — import several text documents in one call.
async fn ingest_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(dto): Json<IngestBatchDto>,
) -> Result<Response, ApiError> {
    state.authorize(&headers)?;
    if dto.items.is_empty() && dto.directory.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "EMPTY_CONTENT",
            "no items or directory",
        ));
    }
    let mut items = Vec::new();
    for item in dto.items {
        let ws = item
            .workspace_id
            .clone()
            .unwrap_or_else(|| "default".into());
        state.authorize_workspace(&headers, &ws)?;
        let req = ingest_request_from_dto(&item);
        items.push(IngestBatchItem {
            request: req,
            payload: IngestPayload::Text(item.content),
        });
    }
    if let Some(directory) = dto.directory {
        // Directory batches are workspace-homogeneous; the HTTP contract has
        // no workspace field, so the default workspace is used and the
        // request is authorized exactly like a text batch.
        state.authorize_workspace(&headers, "default")?;
        for file in directory.files {
            let mut req = IngestRequest::new(WorkspaceId::new("default"));
            req.source_key = file.source_key.clone();
            req.title = file.title.clone();
            req.content_type = file.content_type.clone();
            req.language = file.language.clone();
            items.push(IngestBatchItem {
                request: req,
                payload: IngestPayload::Upload {
                    file_name: file.file_name,
                    bytes: file.content.into_bytes(),
                },
            });
        }
    }
    let result = state
        .runtime
        .application
        .ingest_service
        .ingest_batch(items)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(result).unwrap_or_default()),
    )
        .into_response())
}

/// `GET /ingest/jobs/{id}` — batch job status (RFC-029 6.1).
async fn get_ingest_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, None)?;
    let repo = searchos_storage_sqlite::repos::IngestJobRepo::new(&state.runtime.sqlite);
    let job = repo
        .get(&id)
        .map_err(|e| ApiError::from_domain(&e))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "NOT_FOUND", "ingest job not found"))?;
    if let Some(ws) = scope.as_deref() {
        let job_ws = job.workspace_id.as_str();
        if job_ws != ws && job_ws != "*" {
            searchos_observability::MetricsRegistry::global()
                .inc_counter("auth.workspace_cross_read_total");
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "ingest job not found",
            ));
        }
    }
    let items = repo
        .list_items(&id)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "job": job, "items": items })))
}

/// Document creation DTO (POST /documents).
#[derive(Debug, serde::Deserialize)]
pub struct DocumentDto {
    pub url: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
}

async fn create_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(dto): Json<DocumentDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    state.authorize_workspace(&headers, dto.workspace_id.as_deref().unwrap_or("default"))?;
    let ws = dto
        .workspace_id
        .clone()
        .map(WorkspaceId::new)
        .unwrap_or_else(|| WorkspaceId::new("default"));
    let doc = state
        .runtime
        .application
        .fetch_service
        .fetch_and_persist(&dto.url, &ws)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::to_value(doc).unwrap_or_default()))
}

async fn get_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    // Soft-deleted documents are gone for every read path; the audit trail is
    // reachable through `/documents?status=deleted`.
    let summary = scoped_document(&state, &headers, &id).await?;
    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&state.runtime.sqlite);
    let doc = repo
        .get(&summary.id)
        .map_err(|e| ApiError::from_domain(&e))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                format!("document {id} not found"),
            )
        })?;
    let version = repo
        .get_current_version(&doc.id)
        .map_err(|e| ApiError::from_domain(&e))?;
    let versions = repo
        .list_versions(&doc.id)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "document": doc,
        "summary": summary,
        "current_version": version,
        "versions": versions,
    })))
}

/// `GET /documents` — console document list, workspace-scoped.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct DocumentListQuery {
    pub workspace_id: Option<String>,
    /// `web`, `local` (any local kind) or an exact `local_*` kind.
    pub source_type: Option<String>,
    /// Lifecycle status: `normalized` / `indexed` / `deleted` / …
    pub status: Option<String>,
    pub limit: Option<u32>,
}

/// Query for the RFC-031 evidence endpoints.
#[derive(Debug, serde::Deserialize)]
pub struct EvidenceQuery {
    pub workspace_id: Option<String>,
    pub limit: Option<u32>,
}

/// `GET /evidence/acquisitions` — acquisition list with promotion state
/// counts. Read-only: promotion is an internal consequence of a search.
async fn list_acquisitions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(dto): Query<EvidenceQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, dto.workspace_id.as_deref())?;
    let repo = searchos_storage_sqlite::repos::ExternalEvidenceRepo::new(&state.runtime.sqlite);
    let acquisitions = repo
        .list_acquisitions(scope.as_deref(), dto.limit.unwrap_or(50))
        .map_err(|e| ApiError::from_domain(&e))?;
    let stats = repo
        .stats(scope.as_deref())
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "acquisitions": acquisitions,
        "stats": stats,
    })))
}

/// `GET /evidence/acquisitions/{id}` — one acquisition and its items.
async fn get_acquisition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, None)?;
    let repo = searchos_storage_sqlite::repos::ExternalEvidenceRepo::new(&state.runtime.sqlite);
    let all = repo
        .list_acquisitions(None, 1000)
        .map_err(|e| ApiError::from_domain(&e))?;
    let found = all
        .into_iter()
        .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(id.as_str()));
    let Some(acquisition) = found else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            format!("acquisition {id} not found"),
        ));
    };
    // Workspace scope is enforced like every other read: a foreign id is
    // indistinguishable from a missing one.
    if let Some(ws) = scope.as_deref() {
        if acquisition.get("workspace_id").and_then(|v| v.as_str()) != Some(ws) {
            searchos_observability::MetricsRegistry::global()
                .inc_counter("auth.workspace_cross_read_total");
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                format!("acquisition {id} not found"),
            ));
        }
    }
    let items = repo
        .list_items(&id)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "acquisition": acquisition,
        "items": items,
    })))
}

/// `GET /evidence/reuse` — the compound report: how many future tasks one
/// purchase served.
async fn evidence_reuse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(dto): Query<EvidenceQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, dto.workspace_id.as_deref())?;
    let repo = searchos_storage_sqlite::repos::ExternalEvidenceRepo::new(&state.runtime.sqlite);
    let stats = repo
        .stats(scope.as_deref())
        .map_err(|e| ApiError::from_domain(&e))?;
    let acquisitions = stats
        .get("acquisitions")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let reuse = stats
        .get("reuse_events")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let compound_ratio = if acquisitions > 0 {
        reuse as f64 / acquisitions as f64
    } else {
        0.0
    };
    Ok(Json(serde_json::json!({
        "stats": stats,
        "compound_ratio": compound_ratio,
        "definition": "reuse events per acquisition — how many future local resolutions one purchase served",
    })))
}

async fn list_documents(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(dto): Query<DocumentListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, dto.workspace_id.as_deref())?;
    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&state.runtime.sqlite);
    let documents = repo
        .list_summaries(
            dto.limit.unwrap_or(50),
            dto.source_type.as_deref(),
            dto.status.as_deref(),
            scope.as_deref(),
        )
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "documents": documents })))
}

/// Loads a document summary and enforces the caller's workspace scope.
/// Returns 404 for a missing document *and* for a document owned by another
/// workspace, so the endpoint cannot be used to probe foreign ids.
async fn scoped_document(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
) -> Result<searchos_domain::document::DocumentSummary, ApiError> {
    let scope = state.workspace_scope(headers, None)?;
    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&state.runtime.sqlite);
    let doc = repo
        .get_summary(&searchos_domain::ids::DocumentId::new(id.to_string()))
        .map_err(|e| ApiError::from_domain(&e))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                format!("document {id} not found"),
            )
        })?;
    if let Some(ws) = scope.as_deref() {
        if doc.workspace_id != ws && doc.workspace_id != "*" {
            searchos_observability::MetricsRegistry::global()
                .inc_counter("auth.workspace_cross_read_total");
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                format!("document {id} not found"),
            ));
        }
    }
    if doc.status == "deleted" {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            format!("document {id} not found"),
        ));
    }
    Ok(doc)
}

/// `GET /documents/{id}/versions` — the document's version history.
async fn get_document_versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let doc = scoped_document(&state, &headers, &id).await?;
    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&state.runtime.sqlite);
    let versions = repo
        .list_versions(&doc.id)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(
        serde_json::json!({ "document": doc, "versions": versions }),
    ))
}

/// `DELETE /documents/{id}` — soft-delete a document and drop it from the
/// full-text index (RFC-006 6.7 deletion flow).
async fn delete_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let doc = scoped_document(&state, &headers, &id).await?;
    state
        .runtime
        .delete_document(&doc.id)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(
        serde_json::json!({ "deleted": true, "document_id": doc.id.as_str() }),
    ))
}

/// `POST /documents/{id}/reindex` — rewrite the document's index entries.
///
/// Metadata is never rewritten: the version and its chunks already exist,
/// only the full-text index entries are (re)written.
async fn reindex_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let doc = scoped_document(&state, &headers, &id).await?;
    let written = state
        .runtime
        .reindex_document(&doc.id, &WorkspaceId::new(&doc.workspace_id))
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "document_id": doc.id.as_str(),
        "reindexed": written,
        "version_count": doc.version_count,
        "chunk_count": doc.chunk_count,
    })))
}

/// Memory search DTO.
#[derive(Debug, serde::Deserialize)]
pub struct MemorySearchDto {
    pub query: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}

fn default_top_k() -> usize {
    5
}

async fn memory_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(dto): Json<MemorySearchDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    state.authorize_workspace(&headers, dto.workspace_id.as_deref().unwrap_or("default"))?;
    let reader = MemoryNamespace {
        workspace: dto
            .workspace_id
            .clone()
            .map(WorkspaceId::new)
            .unwrap_or_else(|| WorkspaceId::new("default")),
        project: dto.project.clone(),
        agent: dto.agent.clone(),
    };
    let hits = state
        .runtime
        .application
        .memory_service
        .search(&dto.query, &reader, dto.top_k)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "results": hits })))
}

async fn cost_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, None)?;
    let query = searchos_domain::cost::CostQuery {
        workspace_id: scope.map(searchos_domain::ids::WorkspaceId::new),
        ..Default::default()
    };
    let summary = state
        .runtime
        .application
        .cost_service
        .summary(&query)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    let saved = state
        .runtime
        .application
        .cost_service
        .singleflight_saved_micros()
        .await
        .unwrap_or(0);
    Ok(Json(serde_json::json!({
        "summary": summary,
        "singleflight_saved_micros": saved,
        "is_estimate": true,
    })))
}

async fn cost_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, None)?;
    let providers = state
        .runtime
        .application
        .cost_service
        .summary(&searchos_domain::cost::CostQuery {
            workspace_id: scope.map(searchos_domain::ids::WorkspaceId::new),
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    let mut list = Vec::new();
    for id in state.runtime.provider_registry.ids() {
        let health = state.runtime.provider_registry.health(&id);
        // Per-key rotation status (RFC-009 9.7): present only for
        // multi-key providers; keys are masked.
        let key_status = state
            .runtime
            .provider_registry
            .get(&id)
            .and_then(|p| p.search.as_ref().and_then(|s| s.key_status()))
            .map(|ks| {
                serde_json::json!(ks
                    .iter()
                    .map(|k| serde_json::json!({
                        "index": k.index,
                        "masked_key": k.masked_key,
                        "state": k.state,
                        "available_at": k.available_at,
                        "consecutive_failures": k.consecutive_failures,
                    }))
                    .collect::<Vec<_>>())
            });
        let mut entry = serde_json::json!({
            "provider_id": id,
            "health": health.map(|(s, _rl)| format!("{s:?}")).unwrap_or_else(|| "unknown".into()),
            "rate_limited": health.map(|(_, rl)| rl).unwrap_or(false),
            "cost_micros": providers
                .by_provider
                .iter()
                .find(|(p, _)| p == &id)
                .map(|(_, m)| m.amount_micros)
                .unwrap_or(0),
        });
        if let Some(ks) = key_status {
            entry["keys"] = ks;
        }
        list.push(entry);
    }
    Ok(Json(serde_json::json!({ "providers": list })))
}

async fn stats_search(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let snapshot = searchos_observability::MetricsRegistry::global().snapshot();
    Ok(Json(serde_json::to_value(snapshot).unwrap_or_default()))
}

async fn get_decision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(trace_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, None)?;
    let repo = searchos_storage_sqlite::repos::DecisionRepo::new(&state.runtime.sqlite);
    let trace = repo
        .get_trace_scoped(&trace_id, scope.as_deref())
        .map_err(|e| ApiError::from_domain(&e))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                format!("trace {trace_id} not found"),
            )
        })?;
    Ok(Json(serde_json::to_value(trace).unwrap_or_default()))
}

// ---------------------------------------------------------------- new endpoints

/// `GET /traces` — recent decision traces for the visual console.
#[derive(Debug, serde::Deserialize)]
pub struct TraceListQuery {
    #[serde(default = "default_trace_limit")]
    pub limit: u32,
}

fn default_trace_limit() -> u32 {
    20
}

async fn list_traces(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TraceListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, None)?;
    let traces = searchos_storage_sqlite::repos::DecisionRepo::new(&state.runtime.sqlite)
        .list_traces_scoped(q.limit.min(100), scope.as_deref())
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "traces": traces })))
}

/// `GET /system/status` — one aggregated read-only view for the console
/// (equivalent to `searchos doctor`, RFC-019 19.5).
async fn extract_structures(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let url = body.get("url").and_then(|v| v.as_str()).map(str::to_string);
    let text = match (text, url) {
        (Some(text), _) => text,
        (None, Some(url)) => {
            // Preview only: nothing is fetched into the knowledge store.
            let preview = state
                .runtime
                .application
                .fetch_service
                .fetch_preview(&url)
                .await
                .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "provider", e.to_string()))?;
            preview.markdown
        }
        (None, None) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "validation",
                "text or url is required",
            ))
        }
    };
    let extraction = searchos_document::structures::extract_structures(&text);
    Ok(Json(serde_json::json!({
        "blocks": extraction.blocks,
        "tables": extraction.tables,
        "counts": {
            "blocks": extraction.blocks.len(),
            "tables": extraction.tables.len(),
        },
    })))
}

fn feedback_service(
    state: &AppState,
) -> Result<Arc<searchos_application::feedback::FeedbackService>, ApiError> {
    state
        .runtime
        .application
        .feedback_service
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "feedback service is not wired",
            )
        })
}

async fn submit_feedback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let verdict = body
        .get("verdict")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ApiError::new(StatusCode::BAD_REQUEST, "validation", "verdict is required")
        })?;
    let verdict = searchos_application::feedback::FeedbackVerdict::parse(verdict)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation", e))?;
    let get = |key: &str| {
        body.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let input = searchos_application::feedback::NewFeedback {
        workspace_id: get("workspace_id"),
        task_id: body
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        query: get("query"),
        url: get("url"),
        verdict,
        note: body
            .get("note")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    };
    let event = feedback_service(&state)?
        .submit(input)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation", e.to_string()))?;
    Ok(Json(serde_json::to_value(event).unwrap_or_default()))
}

async fn list_feedback(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let workspace_id = query
        .get("workspace_id")
        .cloned()
        .unwrap_or_else(|| "default".into());
    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(50);
    let events = feedback_service(&state)?
        .list(&workspace_id, limit)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    Ok(Json(serde_json::json!({ "feedback": events })))
}

fn feedback_query(query: &HashMap<String, String>) -> (String, usize) {
    (
        query
            .get("workspace_id")
            .cloned()
            .unwrap_or_else(|| "default".into()),
        query
            .get("min_agreement")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2),
    )
}

async fn feedback_qrels(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let (workspace, min_agreement) = feedback_query(&query);
    let qrels = feedback_service(&state)?
        .qrels(&workspace, min_agreement)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    Ok(Json(serde_json::to_value(qrels).unwrap_or_default()))
}

async fn feedback_dataset(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    state.authorize(&headers)?;
    let (workspace, min_agreement) = feedback_query(&query);
    let dataset = feedback_service(&state)?
        .dataset(&workspace, min_agreement)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    let mut jsonl = String::new();
    for q in &dataset.questions {
        jsonl.push_str(&serde_json::to_string(q).unwrap_or_default());
        jsonl.push('\n');
    }
    Ok((
        StatusCode::OK,
        [("content-type", "application/x-ndjson")],
        jsonl,
    ))
}

async fn list_publishers(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let workspace = query
        .get("workspace_id")
        .cloned()
        .unwrap_or_else(|| "default".into());
    state.authorize_workspace(&headers, &workspace)?;
    let publishers = searchos_storage_sqlite::repos::FeedbackRepo::new(&state.runtime.sqlite)
        .publisher_trust(Some(&workspace))
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "publishers": publishers,
        "min_samples": searchos_domain::publisher::MIN_PUBLISHER_TRUST_SAMPLES,
        "workspace_id": workspace,
    })))
}

async fn watch_run(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let watch = state
        .runtime
        .application
        .watch_service
        .as_ref()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "watch service is not wired",
            )
        })?;
    let report = watch
        .run()
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    Ok(Json(serde_json::to_value(report).unwrap_or_default()))
}

async fn watch_snapshots(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let watch = state
        .runtime
        .application
        .watch_service
        .as_ref()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "watch service is not wired",
            )
        })?;
    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(20);
    let snapshots = watch
        .history(limit)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    Ok(Json(serde_json::json!({ "snapshots": snapshots })))
}

async fn system_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let (tantivy_chunks, sqlite_chunks) = state.runtime.index_consistency().await.unwrap_or((0, 0));
    let schema_version = state
        .runtime
        .sqlite
        .schema_version()
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "version": searchos_core::VERSION,
        "api_version": state.api_version,
        "config_version": state.runtime.config.config_version,
        "schema_version": schema_version,
        "database_ok": true,
        "index": {
            "fulltext_backend": state.runtime.fulltext_backend(),
            "fulltext_chunks": tantivy_chunks,
            "tantivy_chunks": tantivy_chunks,
            "sqlite_chunks": sqlite_chunks,
            "consistent": tantivy_chunks == sqlite_chunks,
        },
        "providers": state.runtime.provider_registry.ids(),
        "crawlers": state.runtime.crawlers.list().len(),
        "crawlers_quarantined": state.runtime.crawlers.quarantined().len(),
        "extensions": state.runtime.extensions.list().len(),
        "extensions_quarantined": state.runtime.extensions.quarantined().len(),
        "auth_required": !state.auth_token.is_empty(),
        "rate_limit_per_minute": state.runtime.config.server.rate_limit_per_minute,
        // RFC-062: the federated source catalog, resolved for the caller's
        // default workspace view. `legacy` means the kernel is not consulted.
        "federation": {
            "strategy": state.runtime.config.federation.strategy,
            "max_sources": state.runtime.config.federation.max_sources,
            "max_parallel_sources": state.runtime.config.federation.max_parallel_sources,
            // RFC-080: the source ordering policy in force.
            "selection": state.runtime.config.federation.selection,
            "sources": state.runtime.federation_registry.list(
                &searchos_federated::SourceFilter { enabled_only: true, ..Default::default() },
                &searchos_domain::ids::WorkspaceId::new("default"),
            ).iter().map(|d| serde_json::json!({
                "source_id": d.source_id.as_str(),
                "category": d.category.as_str(),
                "enabled": d.enabled,
                "billable": d.cost.billable,
            })).collect::<Vec<_>>(),
        },
    })))
}

/// `GET /sources` — the federated source catalog (RFC-063).
///
/// Resolved for the caller's workspace, so a workspace can never enumerate
/// another workspace's private sources. `?enabled_only=true` filters disabled
/// registrations; `?persisted=true` reads the SQLite projection instead of the
/// live registry, which is what an operator audits after a restart.
async fn list_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let workspace = params
        .get("workspace_id")
        .map(|s| s.as_str())
        .unwrap_or("default");
    state.authorize_workspace(&headers, workspace)?;
    let enabled_only = params
        .get("enabled_only")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let persisted = params
        .get("persisted")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if persisted {
        let catalog = searchos_storage_sqlite::repos::SourceCatalogRepo::new(&state.runtime.sqlite);
        let rows = catalog
            .list(enabled_only)
            .map_err(|e| ApiError::from_domain(&e))?;
        let sources: Vec<serde_json::Value> = rows
            .into_iter()
            .filter(|row| match row.workspace_id.as_deref() {
                // Global sources are visible to everyone; workspace-bound
                // rows only to their own workspace.
                None => true,
                Some(ws) => ws == workspace,
            })
            .map(|row| {
                serde_json::json!({
                    "source_id": row.source_id,
                    "name": row.name,
                    "category": row.category,
                    "scope": row.scope,
                    "workspace_id": row.workspace_id,
                    "enabled": row.enabled,
                    "billable": row.billable,
                    "version": row.version,
                    "health": row.health,
                })
            })
            .collect();
        return Ok(Json(serde_json::json!({
            "strategy": state.runtime.config.federation.strategy,
            "projection": "persisted",
            "sources": sources,
        })));
    }

    let descriptors = state.runtime.federation_registry.list(
        &searchos_federated::SourceFilter {
            enabled_only,
            ..Default::default()
        },
        &searchos_domain::ids::WorkspaceId::new(workspace),
    );
    let sources: Vec<serde_json::Value> = descriptors
        .iter()
        .map(|d| {
            serde_json::json!({
                "source_id": d.source_id.as_str(),
                "name": d.name,
                "category": d.category.as_str(),
                "scope": searchos_federated::scope_label(&d.scope),
                "enabled": d.enabled,
                "billable": d.cost.billable,
                "version": d.version,
                "priority": d.priority,
                "capabilities": d.capabilities,
                "estimated_cost_micros": d.cost.estimated_cost_micros,
                "timeout_ms": d.latency.timeout_ms,
                // RFC-080: observed p50/p95 from live execution feedback.
                "latency": {
                    "p50_ms": d.latency.p50_ms,
                    "p95_ms": d.latency.p95_ms,
                    "timeout_ms": d.latency.timeout_ms,
                },
                // RFC-080 slice: operators must see WHY a source stopped
                // being selected. The breaker is authoritative here —
                // `list()` already derived these from the live breaker.
                "reliability": {
                    "success_rate": d.reliability.success_rate,
                    "circuit_open": d.reliability.circuit_open,
                    "consecutive_failures": d.reliability.consecutive_failures,
                },
                "health": state.runtime.federation_registry.health(
                    &d.source_id,
                    &searchos_domain::ids::WorkspaceId::new(workspace),
                ).map(|h| format!("{h:?}").to_lowercase()),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "strategy": state.runtime.config.federation.strategy,
        "projection": "live",
        "sources": sources,
    })))
}

/// `GET /deletions` — append-only deletion-propagation audit (RFC-087).
///
/// Each row proves what a delete actually propagated: chunk count, enqueued
/// index events, whether the drain left the index verifiably clean, and
/// whether caches were invalidated. Rows are never deleted by the deletion
/// path — they ARE the audit trail.
async fn list_deletions(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let workspace = params.get("workspace_id").map(|s| s.as_str());
    if let Some(workspace) = workspace {
        state.authorize_workspace(&headers, workspace)?;
    }
    let document_id = params.get("document_id").map(|s| s.as_str());
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(50)
        .clamp(1, 200);
    let rows = searchos_storage_sqlite::repos::DeletionAuditRepo::new(&state.runtime.sqlite)
        .list(limit, workspace, document_id)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "deletions": rows })))
}

/// RFC-040/075: re-execute a recorded trace offline, optionally under a
/// quality-policy override. No provider is ever called.
async fn replay_trace(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(trace_id): axum::extract::Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let policy = body
        .and_then(|Json(v)| v.get("policy").cloned())
        .map(serde_json::from_value::<searchos_application::replay::PolicyOverride>)
        .transpose()
        .map_err(|e| {
            ApiError::from_domain(&searchos_domain::SearchOsError::Validation(
                searchos_domain::ValidationError {
                    field: "policy".into(),
                    message: e.to_string(),
                },
            ))
        })?;
    let outcome = state
        .runtime
        .replay
        .replay(&trace_id, policy)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::to_value(outcome).map_err(|e| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Internal(e.to_string()))
    })?))
}

/// RFC-086: aggregate offline replays over the most recent recorded traces.
async fn evaluate_replays(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let value = body.map(|Json(v)| v).unwrap_or_default();
    let limit = value
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 200) as u32;
    let workspace = value
        .get("workspace")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if let Some(workspace) = workspace.as_deref() {
        state.authorize_workspace(&headers, workspace)?;
    }
    let policy = value
        .get("policy")
        .cloned()
        .map(serde_json::from_value::<searchos_application::replay::PolicyOverride>)
        .transpose()
        .map_err(|e| {
            ApiError::from_domain(&searchos_domain::SearchOsError::Validation(
                searchos_domain::ValidationError {
                    field: "policy".into(),
                    message: e.to_string(),
                },
            ))
        })?;
    let report = state
        .runtime
        .replay
        .evaluate(limit, workspace.as_deref(), policy)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::to_value(report).map_err(|e| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Internal(e.to_string()))
    })?))
}

/// RFC-030: list workspace-scoped collections with LIVE document counts.
async fn list_collections(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let workspace = params.get("workspace_id").map(|s| s.as_str());
    if let Some(ws) = workspace {
        state.authorize_workspace(&headers, ws)?;
    }
    let rows = searchos_storage_sqlite::repos::CollectionRepo::new(&state.runtime.sqlite)
        .list(workspace)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "collections": rows })))
}

/// RFC-030: create (or fetch) a named collection in one workspace.
async fn create_collection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let name = body.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
        ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", "missing name")
    })?;
    let workspace = body
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    state.authorize_workspace(&headers, workspace)?;
    let collection = searchos_domain::collection::Collection::new(
        &WorkspaceId::new(workspace),
        name,
        body.get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    );
    let repo = searchos_storage_sqlite::repos::CollectionRepo::new(&state.runtime.sqlite);
    let collection = repo
        .ensure(&collection)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "collection": collection })))
}

/// RFC-030: add an existing document to a collection (membership only).
async fn add_collection_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let document_id = body
        .get("document_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "VALIDATION_FAILED",
                "missing document_id",
            )
        })?;
    let repo = searchos_storage_sqlite::repos::CollectionRepo::new(&state.runtime.sqlite);
    let collection_id = searchos_domain::collection::CollectionId(id.clone());
    let exists = repo
        .list(None)
        .map_err(|e| ApiError::from_domain(&e))?
        .into_iter()
        .any(|c| c.collection.id == collection_id);
    if !exists {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "UNKNOWN_COLLECTION",
            id,
        ));
    }
    let doc = searchos_domain::ids::DocumentId::new(document_id);
    let doc_exists = searchos_storage_sqlite::repos::DocumentRepo::new(&state.runtime.sqlite)
        .get(&doc)
        .map_err(|e| ApiError::from_domain(&e))?
        .is_some();
    if !doc_exists {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "UNKNOWN_DOCUMENT",
            document_id,
        ));
    }
    repo.add_document(&collection_id, &doc)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(
        serde_json::json!({ "added": document_id, "collection_id": id }),
    ))
}

async fn remove_collection_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path((id, document_id)): axum::extract::Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&headers)?;
    searchos_storage_sqlite::repos::CollectionRepo::new(&state.runtime.sqlite)
        .remove_document(
            &searchos_domain::collection::CollectionId(id),
            &searchos_domain::ids::DocumentId::new(&document_id),
        )
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn collection_documents(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let documents = searchos_storage_sqlite::repos::CollectionRepo::new(&state.runtime.sqlite)
        .documents(&searchos_domain::collection::CollectionId(id.clone()))
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(
        serde_json::json!({ "collection_id": id, "documents": documents }),
    ))
}

/// RFC-060: list declared agents, newest update first.
async fn list_agents(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let workspace = params.get("workspace_id").map(|s| s.as_str());
    if let Some(ws) = workspace {
        state.authorize_workspace(&headers, ws)?;
    }
    let rows = searchos_storage_sqlite::repos::AgentRepo::new(&state.runtime.sqlite)
        .list(workspace)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "agents": rows })))
}

/// RFC-060: create or update a declared agent. The id is normalized; a
/// display name defaults to the id. Quota is advisory metadata in v1.
async fn upsert_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", "missing id"))?;
    let workspace = body
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    state.authorize_workspace(&headers, workspace)?;
    let mut agent = searchos_domain::agent::Agent::new(
        workspace,
        id,
        body.get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        body.get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        match body.get("max_requests_per_minute") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => {
                let quota = value.as_u64().ok_or_else(|| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "VALIDATION_FAILED",
                        "max_requests_per_minute must be a positive integer",
                    )
                })?;
                Some(u32::try_from(quota).map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "VALIDATION_FAILED",
                        "max_requests_per_minute is out of range",
                    )
                })?)
            }
        },
    )
    .map_err(|message| {
        ApiError::from_domain(&SearchOsError::Validation(
            searchos_domain::ValidationError {
                field: "id".into(),
                message,
            },
        ))
    })?;
    if let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) {
        agent.enabled = enabled;
    }
    let existed = searchos_storage_sqlite::repos::AgentRepo::new(&state.runtime.sqlite)
        .get(workspace, agent.id.as_str())
        .map_err(|e| ApiError::from_domain(&e))?
        .is_some();
    let saved = searchos_storage_sqlite::repos::AgentRepo::new(&state.runtime.sqlite)
        .upsert(&agent)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(
        serde_json::json!({ "agent": saved, "created": !existed }),
    ))
}

/// RFC-060: one declared agent.
async fn get_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let workspace = params
        .get("workspace_id")
        .map(|s| s.as_str())
        .unwrap_or("default");
    state.authorize_workspace(&headers, workspace)?;
    let agent = searchos_storage_sqlite::repos::AgentRepo::new(&state.runtime.sqlite)
        .get(workspace, &id)
        .map_err(|e| ApiError::from_domain(&e))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "UNKNOWN_AGENT", id))?;
    Ok(Json(serde_json::json!({ "agent": agent })))
}

/// RFC-060: enable/disable an agent. History keeps its `agent_id`; only new
/// work is affected once enforcement is wired to this flag.
async fn set_agent_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "VALIDATION_FAILED",
                "missing enabled",
            )
        })?;
    let workspace = body
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    state.authorize_workspace(&headers, workspace)?;
    let agent = searchos_storage_sqlite::repos::AgentRepo::new(&state.runtime.sqlite)
        .set_enabled(workspace, &id, enabled)
        .map_err(|e| ApiError::from_domain(&e))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "UNKNOWN_AGENT", id))?;
    Ok(Json(serde_json::json!({ "agent": agent })))
}

/// RFC-058: the current alias read model.
async fn list_entities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let aliases = state.runtime.entity_registry.snapshot();
    let entities = state
        .runtime
        .entity_store
        .entities()
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "aliases": aliases,
        "alias_count": aliases.len(),
        "entities": entities,
    })))
}

/// RFC-058: register an entity (canonical name + aliases). Conflicts are
/// reported in the response — first registration keeps an alias.
async fn register_entity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let canonical = body
        .get("canonical_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "VALIDATION_FAILED",
                "missing canonical_name",
            )
        })?;
    let aliases: Vec<String> = body
        .get("aliases")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    let source = match body
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("operator")
    {
        "operator" => searchos_domain::entity::EntitySource::Operator,
        "heuristic" => searchos_domain::entity::EntitySource::Heuristic,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "VALIDATION_FAILED",
                "unknown source",
            ))
        }
    };
    let entity = searchos_domain::entity::Entity::new(canonical, aliases, source);
    let conflicts = state
        .runtime
        .register_entity(&entity)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "entity": entity,
        "conflicts": conflicts,
    })))
}

/// RFC-058: resolve text against the alias read model (diagnostics).
async fn resolve_entity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let text = body.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
        ApiError::new(StatusCode::BAD_REQUEST, "VALIDATION_FAILED", "missing text")
    })?;
    let matched = searchos_application::entity::EntityLookup::resolve(
        state.runtime.entity_registry.as_ref(),
        text,
    );
    Ok(Json(serde_json::to_value(matched).map_err(|e| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Internal(e.to_string()))
    })?))
}

/// `POST /research/run` — bounded deep search (RFC-071).
///
/// Each round is a full search recorded as a turn; the final gate's open
/// requirements become the next round's focus. Stops on coverage, the round
/// cap, or TOTAL budget exhaustion.
async fn research_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let workspace = body
        .get("workspace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    state.authorize_workspace(&headers, workspace)?;
    let query = body.get("query").and_then(|v| v.as_str()).ok_or_else(|| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Validation(
            searchos_domain::ValidationError {
                field: "query".into(),
                message: "MISSING_QUERY".into(),
            },
        ))
    })?;
    let profile = body
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("general");
    let mode = body
        .get("mode")
        .and_then(|v| v.as_str())
        .map(|m| m.to_string())
        .unwrap_or_else(|| "local_first".into());
    let mode = searchos_domain::request::SearchMode::parse(&mode).map_err(|_| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Validation(
            searchos_domain::ValidationError {
                field: "mode".into(),
                message: "UNKNOWN_MODE".into(),
            },
        ))
    })?;
    let max_rounds = body
        .get("max_rounds")
        .and_then(|v| v.as_u64())
        .unwrap_or(3)
        .clamp(1, 8) as u32;
    let max_total_cost_micros = body
        .get("max_total_cost_micros")
        .and_then(|v| v.as_i64())
        .unwrap_or(100_000);
    let max_external_queries = body
        .get("max_external_queries")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as u32;
    let session_id = body
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| searchos_domain::new_id("research"));
    let req = searchos_application::research::research_request(
        query,
        profile,
        mode,
        workspace,
        max_total_cost_micros,
        max_external_queries,
    )
    .map_err(|e| ApiError::from_domain(&e))?;
    let run = state
        .runtime
        .research
        .deep_search(req, &session_id, max_rounds)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::to_value(run).map_err(|e| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Internal(e.to_string()))
    })?))
}

/// `GET /research/{session_id}` — the accumulated research workspace.
async fn research_workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let session = state
        .runtime
        .session_store
        .get(&session_id)
        .ok_or_else(|| {
            ApiError::from_domain(&searchos_domain::SearchOsError::Validation(
                searchos_domain::ValidationError {
                    field: "session_id".into(),
                    message: "UNKNOWN_SESSION".into(),
                },
            ))
        })?;
    state.authorize_workspace(&headers, &session.workspace_id)?;
    Ok(Json(serde_json::to_value(session).map_err(|e| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Internal(e.to_string()))
    })?))
}

/// RFC-070: run the memory lifecycle (expire + merge duplicates).
/// `apply = false` (default) is a dry run — nothing is deleted.
async fn memory_lifecycle(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let apply = body
        .map(|Json(v)| v)
        .unwrap_or_default()
        .get("apply")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let expired = state
        .runtime
        .application
        .memory_service
        .expire(apply)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    let consolidated = state
        .runtime
        .application
        .memory_service
        .consolidate(apply)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "expired": expired.expired.len(),
        "expired_ids": expired.expired,
        "merged_groups": consolidated.merged_groups,
        "removed_duplicates": consolidated.removed_duplicates,
        "applied": apply,
    })))
}

/// RFC-034: currently active policy versions (one per kind).
async fn list_policies(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let repo = searchos_storage_sqlite::repos::PolicyRepo::new(&state.runtime.sqlite);
    let mut active = Vec::new();
    for kind in searchos_application::policy::POLICY_KINDS {
        if let Some(row) = repo.active(kind).map_err(|e| ApiError::from_domain(&e))? {
            active.push(row);
        }
    }
    Ok(Json(serde_json::json!({
        "policies": active,
        "live": state.runtime.policy_state.versions(),
    })))
}

fn policy_kind(kind: &str) -> Result<searchos_application::policy::PolicyKind, ApiError> {
    searchos_application::policy::PolicyKind::parse(kind).ok_or_else(|| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Validation(
            searchos_domain::ValidationError {
                field: "kind".into(),
                message: "UNKNOWN_POLICY_KIND".into(),
            },
        ))
    })
}

async fn list_policy_versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(kind): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let kind = policy_kind(&kind)?;
    let rows = searchos_storage_sqlite::repos::PolicyRepo::new(&state.runtime.sqlite)
        .versions(kind.as_str())
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(
        serde_json::json!({ "kind": kind.as_str(), "versions": rows }),
    ))
}

/// RFC-034: submit a NEW policy version and activate it atomically.
async fn submit_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(kind): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let kind = policy_kind(&kind)?;
    let payload = body.get("policy").ok_or_else(|| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Validation(
            searchos_domain::ValidationError {
                field: "policy".into(),
                message: "MISSING_POLICY".into(),
            },
        ))
    })?;
    let payload_json = searchos_application::policy::validate_payload(kind, payload)
        .map_err(|e| ApiError::from_domain(&e))?;
    let notes = body.get("notes").and_then(|v| v.as_str());
    let row = searchos_storage_sqlite::repos::PolicyRepo::new(&state.runtime.sqlite)
        .submit(kind.as_str(), &payload_json, notes)
        .map_err(|e| ApiError::from_domain(&e))?;
    state
        .runtime
        .activate_policy(kind, &row)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::to_value(&row).map_err(|e| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Internal(e.to_string()))
    })?))
}

/// RFC-034: roll back to an EXISTING version (history is never deleted).
async fn rollback_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(kind): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let kind = policy_kind(&kind)?;
    let version = body
        .get("version")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| {
            ApiError::from_domain(&searchos_domain::SearchOsError::Validation(
                searchos_domain::ValidationError {
                    field: "version".into(),
                    message: "MISSING_VERSION".into(),
                },
            ))
        })?;
    let repo = searchos_storage_sqlite::repos::PolicyRepo::new(&state.runtime.sqlite);
    let row = repo
        .activate(kind.as_str(), version)
        .map_err(|e| ApiError::from_domain(&e))?
        .ok_or_else(|| {
            ApiError::from_domain(&searchos_domain::SearchOsError::Validation(
                searchos_domain::ValidationError {
                    field: "version".into(),
                    message: "UNKNOWN_POLICY_VERSION".into(),
                },
            ))
        })?;
    state
        .runtime
        .activate_policy(kind, &row)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::to_value(&row).map_err(|e| {
        ApiError::from_domain(&searchos_domain::SearchOsError::Internal(e.to_string()))
    })?))
}

/// `GET /profiles` — versioned built-in search profiles (RFC-019 19.5).
async fn list_profiles(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let registry = searchos_policy::ProfileRegistry::builtin();
    Ok(Json(serde_json::json!({ "profiles": registry.all() })))
}

/// `GET /profiles/{id}`.
async fn get_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let registry = searchos_policy::ProfileRegistry::builtin();
    let profile = registry.get(&id).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            format!("profile {id} not found"),
        )
    })?;
    Ok(Json(serde_json::to_value(profile).unwrap_or_default()))
}

/// `POST /documents/search` — persisted document lookup (RFC-021 21.5 MCP
/// `document_search` parity).
#[derive(Debug, serde::Deserialize)]
pub struct DocumentSearchDto {
    pub query: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default = "default_doc_limit")]
    pub limit: u32,
}

fn default_doc_limit() -> u32 {
    20
}

async fn search_documents(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(dto): Json<DocumentSearchDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, dto.workspace_id.as_deref())?;
    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&state.runtime.sqlite);
    let docs = repo
        .search_summaries(&dto.query, dto.limit, scope.as_deref())
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "documents": docs })))
}

/// Savings / CPQPA report against an equivalent-quality external-only
/// baseline (RFC-006 6.10, Appendix C).
async fn savings_payload(
    state: &AppState,
    workspace: Option<&str>,
) -> Result<serde_json::Value, ApiError> {
    let baseline = state.runtime.config.budgets.baseline_per_query_micros;
    let report = state
        .runtime
        .application
        .cost_service
        .savings(baseline)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    let mut payload = serde_json::to_value(report).unwrap_or_default();
    if let Some(ws) = workspace {
        payload["workspace_scope"] = serde_json::Value::String(ws.to_string());
    }
    Ok(payload)
}

/// `GET /stats/savings` and `GET /cost/savings`.
async fn stats_savings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, None)?;
    Ok(Json(savings_payload(&state, scope.as_deref()).await?))
}

async fn cost_savings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, None)?;
    Ok(Json(savings_payload(&state, scope.as_deref()).await?))
}

/// `GET /stats/retrieval` — retrieval / reuse rates (RFC-015 15.3).
async fn stats_retrieval(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let snapshot = searchos_observability::MetricsRegistry::global().snapshot();
    let (hits, misses) = state.runtime.exact_cache.stats();
    Ok(Json(serde_json::json!({
        "cache_hit_rate": snapshot.cache_hit_rate,
        "local_resolution_rate": snapshot.local_resolution_rate,
        "valid_local_resolution_rate": snapshot.valid_local_resolution_rate,
        "external_avoidance_rate": snapshot.external_avoidance_rate,
        "semantic_reuse_rate": snapshot.semantic_reuse_rate,
        "singleflight_reuse_rate": snapshot.singleflight_reuse_rate,
        "exact_cache": { "hits": hits, "misses": misses },
        "status_counts": snapshot.status_counts,
    })))
}

/// `GET /cost/reservations` — reserve -> execute -> settle audit trail.
async fn cost_reservations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, None)?;
    let rows = state
        .runtime
        .application
        .cost_service
        .reservations_scoped(50, scope.as_deref())
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(rows))
}

/// Query for `GET /cost/invoices` (RFC-042).
#[derive(Debug, Default, serde::Deserialize)]
pub struct CostInvoiceQueryDto {
    pub workspace_id: Option<String>,
    /// Inclusive lower bound on `occurred_at` (RFC 3339).
    pub from: Option<String>,
    /// Inclusive upper bound on `occurred_at` (RFC 3339).
    pub to: Option<String>,
}

/// `GET /cost/invoices` — RFC-042 invoice view over the append-only ledger,
/// grouped by currency then provider + operation. Estimates are copied,
/// never rewritten; `unreconciled_events` counts rows with no real bill.
async fn cost_invoices(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CostInvoiceQueryDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, q.workspace_id.as_deref())?;
    let invoices = state
        .runtime
        .application
        .cost_service
        .invoices(scope.as_deref(), q.from.as_deref(), q.to.as_deref())
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "invoices": invoices,
        "note": "invoice view over the append-only ledger; estimated amounts are never rewritten",
    })))
}

/// `POST /cost/reconcile` — provider billing import (RFC-006 6.7).
#[derive(Debug, serde::Deserialize)]
pub struct ReconcileDto {
    pub cost_event_id: String,
    /// Actual billed amount in integer micros, matching the append-only ledger.
    pub actual_cost_micros: i64,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
}

async fn cost_reconcile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(dto): Json<ReconcileDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    if dto.actual_cost_micros < 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "actual_cost_micros must be >= 0",
        ));
    }
    let currency = dto.currency.unwrap_or_else(|| "USD".into());
    let source = dto.source.unwrap_or_else(|| "provider_billing".into());
    state
        .runtime
        .application
        .cost_service
        .reconcile(
            &dto.cost_event_id,
            dto.actual_cost_micros,
            &source,
            &currency,
        )
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "reconciled": dto.cost_event_id,
        "actual_cost_micros": dto.actual_cost_micros,
        "source": source,
        "currency": currency,
    })))
}

// ---------------------------------------------------------------------------
// RFC-054 durable background jobs.
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct EnqueueJobDto {
    pub kind: String,
    /// Opaque JSON consumed by the job handler.
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    /// RFC 3339 time the job becomes claimable; default: now.
    #[serde(default)]
    pub run_after: Option<String>,
}

async fn enqueue_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(dto): Json<EnqueueJobDto>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    state.authorize(&headers)?;
    let ws = dto
        .workspace_id
        .clone()
        .map(WorkspaceId::new)
        .unwrap_or_else(|| WorkspaceId::new("default"));
    state.authorize_workspace(&headers, ws.as_str())?;
    let kind = dto.kind.trim();
    if kind.is_empty()
        || kind.len() > 64
        || !kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "kind must be 1-64 ascii alphanumeric or '_'",
        ));
    }
    let job = searchos_storage_sqlite::repos::JobRepo::new(&state.runtime.sqlite)
        .enqueue(
            &format!("job_{}", searchos_domain::ids::CostEventId::generate()),
            kind,
            ws.as_str(),
            &dto.payload,
            dto.max_attempts.unwrap_or(3),
            dto.run_after.as_deref(),
        )
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(job).unwrap_or_default()),
    ))
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct JobListQueryDto {
    pub workspace_id: Option<String>,
    pub state: Option<String>,
    pub limit: Option<u32>,
}

async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<JobListQueryDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, q.workspace_id.as_deref())?;
    let jobs = searchos_storage_sqlite::repos::JobRepo::new(&state.runtime.sqlite)
        .list(scope.as_deref(), q.state.as_deref(), q.limit.unwrap_or(50))
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "jobs": jobs })))
}

async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let job = searchos_storage_sqlite::repos::JobRepo::new(&state.runtime.sqlite)
        .get(&id)
        .map_err(|e| ApiError::from_domain(&e))?;
    let Some(job) = job else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            format!("job {id} not found"),
        ));
    };
    Ok(Json(serde_json::to_value(job).unwrap_or_default()))
}

// ---------------------------------------------------------------------------
// RFC-050 automatic source discovery.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct DiscoverSourcesDto {
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// `POST /sources/discover` — scan the workspace corpus for publisher hosts.
async fn discover_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<DiscoverSourcesDto>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let requested = body.and_then(|Json(dto)| dto.workspace_id);
    let scope = state.workspace_scope(&headers, requested.as_deref())?;
    let report = state
        .runtime
        .discover_sources(scope.as_deref())
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(report))
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct SourceDiscoveryQueryDto {
    /// `discovered` | `registered` | `dismissed`
    pub status: Option<String>,
    pub limit: Option<u32>,
}

async fn list_source_discoveries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SourceDiscoveryQueryDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let rows = searchos_storage_sqlite::repos::SourceDiscoveryRepo::new(&state.runtime.sqlite)
        .list(q.status.as_deref(), q.limit.unwrap_or(100))
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "discoveries": rows })))
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct RegisterDiscoveredSourceDto {
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// `POST /sources/discoveries/{host}/register` — promote a discovered
/// publisher into a real federated source.
async fn register_discovered_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host): Path<String>,
    body: Option<Json<RegisterDiscoveredSourceDto>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let requested = body.and_then(|Json(dto)| dto.workspace_id);
    let scope = state.workspace_scope(&headers, requested.as_deref())?;
    let report = state
        .runtime
        .register_discovered_source(&host, scope.as_deref())
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(report))
}

/// `POST /sources/discoveries/{host}/dismiss` — drop a proposal.
async fn dismiss_discovered_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    searchos_storage_sqlite::repos::SourceDiscoveryRepo::new(&state.runtime.sqlite)
        .dismiss(&host)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({ "host": host, "dismissed": true })))
}

// ---------------------------------------------------------------------------
// RFC-028 Dashboard API. Handlers only authenticate, validate, call the
// Analytics Query Service and wrap the standard response envelope.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct DashboardQueryDto {
    pub workspace_id: Option<String>,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub provider_id: Option<String>,
    pub search_mode: Option<String>,
    pub quality_status: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub interval: Option<String>,
    pub timezone: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub format: Option<String>,
}

impl DashboardQueryDto {
    fn to_query(&self, workspace_scope: Option<String>) -> AnalyticsQuery {
        AnalyticsQuery {
            workspace_id: workspace_scope.or_else(|| self.workspace_id.clone()),
            project_id: self.project_id.clone(),
            agent_id: self.agent_id.clone(),
            provider_id: self.provider_id.clone(),
            search_mode: self.search_mode.clone(),
            quality_status: self.quality_status.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            interval: self.interval.clone(),
            timezone: self.timezone.clone(),
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}

/// Single source of truth for dataset routes: the route table and the
/// Read Model mapping are generated from this list, so a new path cannot
/// silently 404 (P2-10).
const DASHBOARD_DATASETS: &[(&str, &str)] = &[
    ("search/summary", "search_requests"),
    ("search/requests", "search_requests"),
    ("search/timeseries", "search_timeseries"),
    ("search/strategies", "search_strategies"),
    ("cost/summary", "cost_breakdown"),
    ("cost/breakdown", "cost_breakdown"),
    ("cost/savings", "cost_breakdown"),
    ("knowledge/summary", "knowledge_growth"),
    ("knowledge/documents", "knowledge_growth"),
    ("knowledge/freshness", "knowledge_freshness"),
    ("quality/summary", "quality_dimensions"),
    ("quality/dimensions", "quality_dimensions"),
    ("quality/failures", "quality_dimensions"),
    ("providers", "providers"),
    ("crawlers/summary", "crawlers_summary"),
    ("infrastructure/summary", "infrastructure_summary"),
];

/// Every Dashboard route. The table is the single source of truth for the
/// OpenAPI contract; `dashboard_routes` is built from it.
pub fn dashboard_route_table() -> Vec<RouteSpec> {
    let mut table = vec![RouteSpec::new(
        "GET",
        "/api/v1/dashboard/overview",
        get(dashboard_overview),
    )];
    for (path, _) in DASHBOARD_DATASETS {
        table.push(RouteSpec::new(
            "GET",
            &format!("/api/v1/dashboard/{path}"),
            get(dashboard_dataset),
        ));
    }
    table.push(RouteSpec::new(
        "GET",
        "/api/v1/dashboard/export",
        get(dashboard_export),
    ));
    table.push(RouteSpec::new(
        "GET",
        "/api/v1/dashboard/events",
        get(dashboard_events),
    ));
    table
}

fn dashboard_routes(state: AppState) -> Router {
    register_routes(Router::<AppState>::new(), dashboard_route_table()).with_state(state)
}

/// Maps an explicit Dashboard path to a Read Model key.
fn read_model_for_path(path: &str) -> Option<&'static str> {
    let suffix = path.split("/api/v1/dashboard/").nth(1)?;
    DASHBOARD_DATASETS
        .iter()
        .find(|(route, _)| *route == suffix)
        .map(|(_, model)| *model)
}

async fn dashboard_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(dto): Query<DashboardQueryDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, dto.workspace_id.as_deref())?;
    let query = dto.to_query(scope);
    query.validate().map_err(|e| ApiError::from_domain(&e))?;
    let report = state
        .runtime
        .application
        .analytics_service
        .overview(&query)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(report))
}

async fn dashboard_dataset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(dto): Query<DashboardQueryDto>,
    req: Request,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, dto.workspace_id.as_deref())?;
    let model = read_model_for_path(req.uri().path()).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "unknown dashboard dataset",
        )
    })?;
    let query = dto.to_query(scope);
    query.validate().map_err(|e| ApiError::from_domain(&e))?;
    let report = state
        .runtime
        .application
        .analytics_service
        .dataset(model, &query)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(report))
}

/// CSV / JSON export of the Overview Read Model (RFC-028 10).
async fn dashboard_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(dto): Query<DashboardQueryDto>,
) -> Result<Response, ApiError> {
    state.authorize(&headers)?;
    let scope = state.workspace_scope(&headers, dto.workspace_id.as_deref())?;
    let query = dto.to_query(scope);
    let report = state
        .runtime
        .application
        .analytics_service
        .overview(&query)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    if dto.format.as_deref() == Some("csv") {
        let data = &report["data"];
        let csv = format!(
            "metric,value,denominator\nsearches,{},{}\nlocal_resolution,{},{}\nexternal_search,{},{}\nquality_pass_rate,{},{}\n",
            data["searches"]["total"],
            data["searches"]["total"],
            data["local_resolution"]["count"],
            data["local_resolution"]["denominator"],
            data["external_search"]["count"],
            data["external_search"]["denominator"],
            data["quality"]["pass_rate"],
            data["quality"]["denominator"],
        );
        return Ok((
            StatusCode::OK,
            [("content-type", "text/csv; charset=utf-8")],
            csv,
        )
            .into_response());
    }
    Ok(Json(report).into_response())
}

/// Releases one SSE connection slot when the stream is dropped.
struct SseGuard;

impl Drop for SseGuard {
    fn drop(&mut self) {
        ACTIVE_SSE_CONNECTIONS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Stream adapter that holds the SSE connection slot for its whole
/// lifetime and releases it when the response stream is dropped.
struct GuardedStream<S: futures_util::stream::Stream> {
    inner: Pin<Box<S>>,
    _guard: SseGuard,
}

impl<S: futures_util::stream::Stream> futures_util::stream::Stream for GuardedStream<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.inner.as_mut().poll_next(cx)
    }
}

fn with_sse_guard<S>(stream: S) -> impl futures_util::stream::Stream<Item = S::Item>
where
    S: futures_util::stream::Stream + 'static,
{
    GuardedStream {
        inner: Box::pin(stream),
        _guard: SseGuard,
    }
}

/// One-way realtime notification channel (RFC-028 6.2). SSE is a hint to
/// refetch the Read Model, never the source of historical truth.
async fn dashboard_events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::stream::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    state.authorize(&headers)?;
    let _scope = state.workspace_scope(&headers, None)?;
    let previous = ACTIVE_SSE_CONNECTIONS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    SSE_CONNECTIONS_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if previous >= MAX_SSE_CONNECTIONS {
        ACTIVE_SSE_CONNECTIONS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "SSE_CONNECTION_LIMIT",
            format!("at most {MAX_SSE_CONNECTIONS} concurrent dashboard streams"),
        ));
    }
    // Kept alive for the whole handler body; released when the response is
    // dropped (client closed / proxy timeout). See SseGuard below.
    let analytics = Arc::clone(&state.runtime.application.analytics_service);
    let shutdown = Arc::clone(&state.shutdown);
    let stream = stream::unfold(0i64, move |cursor| {
        let analytics = Arc::clone(&analytics);
        let shutdown = Arc::clone(&shutdown);
        async move {
            // A graceful shutdown must terminate the stream: otherwise the
            // SSE connection stays open forever and pins the connection drain.
            let mut stop = shutdown.subscribe();
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                _ = stop.changed() => {
                    if *stop.borrow() {
                        return None;
                    }
                }
            }
            let events = analytics.events_since(cursor, 20).await.unwrap_or_default();
            let next = events
                .last()
                .and_then(|e| e.get("id"))
                .and_then(|v| v.as_i64())
                .unwrap_or(cursor);
            // Empty polls keep the connection alive without pushing a
            // refetch event; keep-alive handles liveness (P1-3).
            let event = if events.is_empty() {
                SseEvent::default().comment("keepalive")
            } else {
                let data = serde_json::to_string(&events).unwrap_or_else(|_| "[]".into());
                SseEvent::default().event("analytics.events").data(data)
            };
            Some((Ok(event), next))
        }
    });
    Ok(Sse::new(with_sse_guard(stream)).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn state() -> (AppState, tempfile::TempDir) {
        state_with_ingest_limits(20 * 1024 * 1024, 200 * 1024 * 1024).await
    }

    async fn state_with_ingest_limits(
        per_document: usize,
        per_batch: usize,
    ) -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = searchos_core::SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        cfg.ingest.max_bytes_per_document = per_document;
        cfg.ingest.max_bytes_per_batch = per_batch;
        let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        (AppState::new(rt), dir)
    }

    /// Part 30 §30.11: expired rate-limit windows are evicted, not retained
    /// for the process lifetime.
    #[test]
    fn rate_limit_windows_are_evicted() {
        let limiter = RateLimiter::new(10);
        assert!(limiter.check("client-a"));
        let stale_window = RateLimiter::current_window().saturating_sub(1);
        limiter
            .windows
            .lock()
            .unwrap()
            .insert(999_999, (stale_window, 1));
        assert_eq!(limiter.tracked(), 2);
        let before = searchos_observability::MetricsRegistry::global()
            .counter("ratelimit.windows_evicted_total");
        limiter.evict_expired();
        assert_eq!(limiter.tracked(), 1, "stale window removed");
        assert!(
            searchos_observability::MetricsRegistry::global()
                .counter("ratelimit.windows_evicted_total")
                > before
        );
    }

    #[tokio::test]
    async fn health_endpoint_ok() {
        let (st, _d) = state().await;
        let app = router(st);
        let body = call(app, "/health").await;
        assert_eq!(body["status"], "ok");
        assert!(body["index"]["consistent"].as_bool().unwrap());
    }

    /// `GET /metrics` is Prometheus text exposition 0.0.4 and carries both the
    /// execution counters and the process-health gauges.
    #[tokio::test]
    async fn metrics_endpoint_serves_prometheus_text() {
        use tower::ServiceExt;
        let (st, _d) = state().await;
        let app = router(st);
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        for expected in [
            "# TYPE searchos_search_requests_total counter",
            "# TYPE searchos_search_latency_p50_seconds gauge",
            "# TYPE searchos_uptime_seconds gauge",
            "# TYPE searchos_sse_active gauge",
            "# TYPE searchos_index_outbox_pending gauge",
            "searchos_build_info{",
            "searchos_invariant_auth_workspace_cross_read_total",
        ] {
            assert!(
                text.contains(expected),
                "/metrics is missing '{expected}':\n{text}"
            );
        }
    }

    /// The shutdown drain must flush work that was enqueued just before the
    /// signal, so a document fetched at the last moment still becomes
    /// searchable instead of silently pending after exit.
    #[tokio::test]
    async fn outbox_worker_drains_pending_rows_on_shutdown() {
        let (st, _d) = state().await;
        let doc = st
            .runtime
            .application
            .ingest_service
            .ingest_text(
                searchos_application::ingest_service::IngestRequest::new(WorkspaceId::new(
                    "default",
                )),
                "shutdown drain body",
            )
            .await
            .unwrap();
        // Deleting a document enqueues index delete events in the outbox.
        searchos_storage_sqlite::repos::DocumentRepo::new(&st.runtime.sqlite)
            .delete(&doc.document_id)
            .unwrap();
        let outbox = searchos_storage_sqlite::repos::OutboxRepo::new(&st.runtime.sqlite);
        let (pending, _, _) = outbox.counts().unwrap();
        assert!(pending > 0, "the delete must enqueue index work");

        // Signal shutdown immediately: the worker must stop ticking, perform its
        // final drain and return.
        let (tx, rx) = tokio::sync::watch::channel(false);
        tx.send(true).unwrap();
        run_outbox_worker(Arc::clone(&st.runtime), rx).await;

        let (pending, processed, _) = outbox.counts().unwrap();
        assert_eq!(pending, 0, "the shutdown drain must flush the outbox");
        assert!(processed > 0, "the drain must report processed rows");
    }

    /// The shutdown watcher resolves when the channel is flipped.
    #[tokio::test]
    async fn shutdown_signal_resolves_when_flipped() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        assert!(!*rx.borrow());
        tx.send(true).unwrap();
        shutdown_signal(rx).await;
    }

    #[tokio::test]
    async fn search_validation_rejects_empty() {
        let (st, _d) = state().await;
        let app = router(st);
        let (status, body) = call_post(app, "/search", serde_json::json!({"query": "  "})).await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "VALIDATION_FAILED");
    }

    // RFC-067/068 (RFC-062 §12): the request contract accepts a context
    // budget on every transport path, validates it at the edge, and the
    // response carries the assembled block.
    // RFC-062: plan_only federation is observable on the plan surface and
    // legacy stays the default (no federation fields populated).
    /// RFC-063: the catalog is readable live and from the persisted
    /// projection, and a workspace cannot enumerate another workspace's
    /// workspace-bound sources.
    #[tokio::test]
    async fn sources_endpoint_reports_live_and_persisted_catalogs() {
        let (st, _d) = state().await;
        let registry = st.runtime.federation_registry.clone();
        let app = router(st);

        let get = |app: Router, uri: &'static str| async move {
            use tower::ServiceExt;
            let resp = app
                .oneshot(
                    axum::http::Request::builder()
                        .method("GET")
                        .uri(uri)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (status, body)
        };

        let (status, live) = get(app.clone(), "/sources").await;
        assert_eq!(status, 200, "{live}");
        assert_eq!(live["projection"], "live");
        assert_eq!(live["strategy"], "legacy");
        let ids: Vec<&str> = live["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["source_id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"local_knowledge"), "{ids:?}");
        assert!(ids.contains(&"agent_memory"), "memory is a source: {ids:?}");
        // Capabilities are declared, not inferred.
        let local = live["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["source_id"] == "local_knowledge")
            .unwrap();
        assert_eq!(local["capabilities"]["lexical_search"], true);
        assert_eq!(local["billable"], false);
        // RFC-080: declared + observed latency are visible to operators.
        assert!(local["latency"]["p50_ms"].as_u64().unwrap() > 0);
        assert!(local["latency"]["p95_ms"].as_u64().unwrap() > 0);
        assert_eq!(local["latency"]["timeout_ms"], local["timeout_ms"]);
        // RFC-080: live reliability is visible to operators.
        assert!(local["reliability"]["success_rate"].as_f64().unwrap() > 0.0);
        assert_eq!(local["reliability"]["circuit_open"], false);
        assert_eq!(local["reliability"]["consecutive_failures"], 0);

        // A tripped circuit must be observable through the same endpoint,
        // not hidden inside the planner.
        for _ in 0..3 {
            registry.record_outcome(
                &searchos_domain::ids::SourceId::new("local_knowledge"),
                false,
            );
        }
        let (_, tripped) = get(app.clone(), "/sources").await;
        let tripped_local = tripped["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["source_id"] == "local_knowledge")
            .unwrap();
        assert_eq!(tripped_local["reliability"]["circuit_open"], true);
        assert_eq!(tripped_local["reliability"]["consecutive_failures"], 3);

        // The persisted projection recorded the same catalog at startup.
        let (status, persisted) = get(app.clone(), "/sources?persisted=true").await;
        assert_eq!(status, 200, "{persisted}");
        assert_eq!(persisted["projection"], "persisted");
        let persisted_ids: Vec<&str> = persisted["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["source_id"].as_str().unwrap())
            .collect();
        assert!(persisted_ids.contains(&"local_knowledge"));

        // enabled_only keeps the enabled sources.
        let (status, enabled) = get(app, "/sources?enabled_only=true").await;
        assert_eq!(status, 200, "{enabled}");
        assert!(enabled["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["enabled"] == true));
    }

    #[tokio::test]
    async fn collection_endpoints_create_add_and_list() {
        let (st, _d) = state().await;
        // Seed a document through the runtime so membership has a real target.
        st.runtime
            .application
            .ingest_service
            .ingest_text(
                searchos_application::ingest_service::IngestRequest::new(WorkspaceId::new(
                    "default",
                )),
                "A document placed into the HTTP collection gate.",
            )
            .await
            .unwrap();
        let doc_id = searchos_storage_sqlite::repos::DocumentRepo::new(&st.runtime.sqlite)
            .list_summaries(10, None, None, None)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .id;
        let app = router(st);

        use tower::ServiceExt;
        let call = |app: axum::Router, method: &str, uri: &str, body: serde_json::Value| {
            let (method, uri) = (method.to_string(), uri.to_string());
            async move {
                let resp = app
                    .oneshot(
                        axum::http::Request::builder()
                            .method(method.as_str())
                            .uri(uri)
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status().as_u16();
                let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                    .await
                    .unwrap();
                (
                    status,
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                        .unwrap_or(serde_json::Value::Null),
                )
            }
        };

        let (status, created) = call(
            app.clone(),
            "POST",
            "/collections",
            serde_json::json!({ "name": "HTTP Gate" }),
        )
        .await;
        assert_eq!(status, 200, "{created}");
        let id = created["collection"]["id"].as_str().unwrap().to_string();

        let (status, _) = call(
            app.clone(),
            "POST",
            &format!("/collections/{id}/documents"),
            serde_json::json!({ "document_id": doc_id.to_string() }),
        )
        .await;
        assert_eq!(status, 200);

        let (status, list) = call(app.clone(), "GET", "/collections", serde_json::json!({})).await;
        assert_eq!(status, 200, "{list}");
        let row = list["collections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["collection"]["id"] == id)
            .unwrap();
        assert_eq!(row["document_count"], 1);

        let (status, docs) = call(
            app,
            "GET",
            &format!("/collections/{id}/documents"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(docs["documents"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn agent_registry_endpoints_normalize_upsert_and_disable() {
        let (st, _d) = state().await;
        let app = router(st);
        use tower::ServiceExt;
        let call = |app: axum::Router, method: &str, uri: &str, body: serde_json::Value| {
            let (method, uri) = (method.to_string(), uri.to_string());
            async move {
                let resp = app
                    .oneshot(
                        axum::http::Request::builder()
                            .method(method.as_str())
                            .uri(uri)
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status().as_u16();
                let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                    .await
                    .unwrap();
                (
                    status,
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                        .unwrap_or(serde_json::Value::Null),
                )
            }
        };

        let (status, created) = call(
            app.clone(),
            "POST",
            "/agents",
            serde_json::json!({
                "id": " Claude-Code ",
                "name": "Claude Code",
                "description": "nightly research",
                "max_requests_per_minute": 30
            }),
        )
        .await;
        assert_eq!(status, 200, "{created}");
        assert_eq!(created["created"], true);
        assert_eq!(created["agent"]["id"], "claude-code");
        assert_eq!(created["agent"]["max_requests_per_minute"], 30);

        let (_, list) = call(
            app.clone(),
            "GET",
            "/agents?workspace_id=default",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(list["agents"].as_array().unwrap().len(), 1, "{list}");

        let (_, disabled) = call(
            app.clone(),
            "POST",
            "/agents/claude-code/enabled",
            serde_json::json!({ "enabled": false }),
        )
        .await;
        assert_eq!(disabled["agent"]["enabled"], false, "{disabled}");

        let (_, shown) = call(
            app,
            "GET",
            "/agents/claude-code?workspace_id=default",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(shown["agent"]["id"], "claude-code");
        assert_eq!(shown["agent"]["enabled"], false);
    }

    #[tokio::test]
    async fn publisher_trust_endpoint_scores_hosts_from_feedback() {
        let (st, _d) = state().await;
        let repo = searchos_storage_sqlite::repos::FeedbackRepo::new(&st.runtime.sqlite);
        let stamp = chrono::Utc::now().to_rfc3339();
        for (i, verdict) in ["canonical", "helpful", "related"].iter().enumerate() {
            let url = format!("https://publisher.example.com/{i}");
            repo.append(
                &format!("fb_{i}"),
                &stamp,
                "default",
                None,
                "query",
                &url,
                verdict,
                None,
                "query",
                &url,
            )
            .unwrap();
        }
        let app = router(st);
        let body = call(app, "/publishers?workspace_id=default").await;
        let row = body["publishers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["publisher"] == "publisher.example.com")
            .unwrap_or_else(|| panic!("publisher row missing: {body}"));
        assert_eq!(row["judged"], 3);
        assert_eq!(row["relevant"], 2);
        assert!(row["trust"].as_f64().unwrap() > 0.66);
        assert_eq!(body["min_samples"], 2);
    }

    #[tokio::test]
    async fn cost_invoices_endpoint_groups_and_counts_unreconciled() {
        use searchos_domain::cost::{CostEvent, CostOperation};
        let (st, _d) = state().await;
        let repo = searchos_storage_sqlite::repos::CostRepo::new(&st.runtime.sqlite);
        let append = |provider: &str, estimated: &str, actual: Option<&str>| {
            let task = searchos_domain::ids::TaskId::generate();
            let ws = searchos_domain::ids::WorkspaceId::new("default");
            searchos_storage_sqlite::repos::TaskRepo::new(&st.runtime.sqlite)
                .insert(
                    &task,
                    &ws,
                    None,
                    "q",
                    "q",
                    "fp",
                    "general",
                    "1.0.0",
                    "local_first",
                    &searchos_domain::ids::TraceId::generate(),
                )
                .unwrap();
            let ev = CostEvent {
                id: searchos_domain::ids::CostEventId::generate(),
                task_id: task,
                workspace_id: ws,
                agent_id: None,
                operation: CostOperation::WebSearch,
                provider_id: Some(provider.into()),
                input_units: 1,
                output_units: 0,
                estimated_cost: searchos_domain::cost::Money::parse(estimated, "USD").unwrap(),
                actual_cost: actual.map(|a| searchos_domain::cost::Money::parse(a, "USD").unwrap()),
                duration_ms: 5,
                occurred_at: chrono::Utc::now(),
                trace_id: None,
                shared: false,
                pricing_model_version: "1".into(),
                status: Default::default(),
                error_code: None,
            };
            repo.append(&ev).unwrap();
        };
        append("tavily", "0.005", None);
        append("tavily", "0.005", Some("0.006"));
        let app = router(st);
        let body = call(app, "/cost/invoices").await;
        let invoices = body["invoices"].as_array().unwrap();
        assert_eq!(invoices.len(), 1, "one currency per invoice: {body}");
        let inv = &invoices[0];
        assert_eq!(inv["currency"], "USD");
        assert_eq!(inv["events"], 2);
        assert_eq!(inv["unreconciled_events"], 1);
        assert_eq!(inv["total_estimated_micros"], 10_000);
        assert_eq!(inv["total_actual_micros"], 6_000);
        assert_eq!(inv["fully_reconciled"], false);
        assert_eq!(inv["lines"][0]["provider"], "tavily");
    }

    #[tokio::test]
    async fn cost_reconcile_accepts_micros_and_updates_the_invoice() {
        use searchos_domain::cost::{CostEvent, CostOperation};
        let (st, _d) = state().await;
        let task = searchos_domain::ids::TaskId::generate();
        let ws = searchos_domain::ids::WorkspaceId::new("default");
        searchos_storage_sqlite::repos::TaskRepo::new(&st.runtime.sqlite)
            .insert(
                &task,
                &ws,
                None,
                "q",
                "q",
                "fp",
                "general",
                "1.0.0",
                "local_first",
                &searchos_domain::ids::TraceId::generate(),
            )
            .unwrap();
        let event = CostEvent {
            id: searchos_domain::ids::CostEventId::generate(),
            task_id: task,
            workspace_id: ws,
            agent_id: None,
            operation: CostOperation::WebSearch,
            provider_id: Some("tavily".into()),
            input_units: 1,
            output_units: 0,
            estimated_cost: searchos_domain::cost::Money::parse("0.005", "USD").unwrap(),
            actual_cost: None,
            duration_ms: 5,
            occurred_at: chrono::Utc::now(),
            trace_id: None,
            shared: false,
            pricing_model_version: "1".into(),
            status: Default::default(),
            error_code: None,
        };
        searchos_storage_sqlite::repos::CostRepo::new(&st.runtime.sqlite)
            .append(&event)
            .unwrap();

        let app = router(AppState::new(Arc::clone(&st.runtime)));
        let (status, body) = call_post(
            app,
            "/cost/reconcile",
            serde_json::json!({
                "cost_event_id": event.id.as_str(),
                "actual_cost_micros": 6_000,
                "source": "provider_billing",
                "currency": "USD"
            }),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["actual_cost_micros"], 6_000);

        let body = call(
            router(AppState::new(Arc::clone(&st.runtime))),
            "/cost/invoices",
        )
        .await;
        assert_eq!(body["invoices"][0]["total_actual_micros"], 6_000);
        assert_eq!(body["invoices"][0]["fully_reconciled"], true);
    }

    #[tokio::test]
    async fn fetch_batch_rejects_empty_urls_and_reports_per_item_outcomes() {
        let (st, _d) = state().await;
        let app = router(st);
        let (status, body) = call_post(app, "/fetch/batch", serde_json::json!({"urls": []})).await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"]["code"], "VALIDATION_FAILED");

        let (st, _d) = state().await;
        let app = router(st);
        let (status, body) = call_post(
            app,
            "/fetch/batch",
            serde_json::json!({
                "urls": ["notaurl", "https://example.com/", "https://example.com/"],
                "persist": false,
                "max_urls": 5,
                "default_interval_ms": 1,
            }),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let items = body["items"].as_array().unwrap();
        assert_eq!(items.len(), 3, "every input reported: {body}");
        assert_eq!(items[0]["input_index"], 0);
        assert_eq!(items[0]["outcome"], "invalid_url");
        assert_eq!(items[1]["outcome"], "fetched");
        assert_eq!(items[2]["outcome"], "duplicate");
        assert_eq!(body["queued_remaining"], 0);
    }

    #[tokio::test]
    async fn background_jobs_enqueue_list_get_and_terminal_failure() {
        let (st, _d) = state().await;
        let runtime = Arc::clone(&st.runtime);
        let (status, created) = call_post(
            router(st),
            "/jobs",
            serde_json::json!({
                "kind": "unsupported_kind",
                "workspace_id": "default",
                "payload": {"x": 1},
                "max_attempts": 4
            }),
        )
        .await;
        assert_eq!(status, 201, "{created}");
        let id = created["id"].as_str().unwrap();
        assert_eq!(created["state"], "pending");
        assert_eq!(created["attempts"], 0);

        let listed = call(
            router(AppState::new(Arc::clone(&runtime))),
            "/jobs?state=pending",
        )
        .await;
        assert!(listed["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["id"] == id));

        let (done, failed) = runtime.process_jobs(1).await.unwrap();
        assert_eq!((done, failed), (0, 1));
        let fetched = call(router(AppState::new(runtime)), &format!("/jobs/{id}")).await;
        assert_eq!(fetched["state"], "failed");
        assert_eq!(fetched["attempts"], 1);
        assert!(fetched["last_error"]
            .as_str()
            .unwrap()
            .contains("unsupported"));
    }

    #[tokio::test]
    async fn source_discovery_scans_documents_and_promotes_a_publisher() {
        let (st, _d) = state().await;
        // Seed a URL document from a publisher host.
        let doc = st
            .runtime
            .application
            .fetch_service
            .fetch_and_persist(
                "https://publisher.example.com/post",
                &WorkspaceId::new("default"),
            )
            .await;
        // Network may be unavailable in CI: fall back to a stored document row
        // so the discovery path itself is still exercised.
        if doc.is_err() {
            let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&st.runtime.sqlite);
            let ws = WorkspaceId::new("default");
            searchos_storage_sqlite::repos::ensure_workspace(&st.runtime.sqlite, &ws, "default")
                .unwrap();
            repo.save_versioned(
                &searchos_domain::document::Document {
                    id: searchos_domain::ids::DocumentId::generate(),
                    source_id: searchos_domain::ids::SourceId::new("src_discovery"),
                    canonical_url: "https://publisher.example.com/post".into(),
                    current_version_id: None,
                    content_type: "text/html".into(),
                    language: None,
                    source_type: "url".into(),
                    title: None,
                    workspace_id: "default".into(),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                },
                &searchos_domain::document::DocumentVersion {
                    id: searchos_domain::ids::DocumentVersionId::generate(),
                    document_id: searchos_domain::ids::DocumentId::generate(),
                    content_hash: "hash_discovery".into(),
                    raw_content_ref: "ref".into(),
                    markdown_ref: None,
                    published_at: None,
                    fetched_at: chrono::Utc::now(),
                    extractor_version: "1".into(),
                    crawler_version: None,
                    summary: None,
                    keywords: Vec::new(),
                },
                &[],
                &[],
                &ws,
            )
            .unwrap();
        }

        let (status, report) = call_post(
            router(AppState::new(Arc::clone(&st.runtime))),
            "/sources/discover",
            serde_json::json!({"workspace_id": "default"}),
        )
        .await;
        assert_eq!(status, 200, "{report}");
        assert!(
            report["hosts"].as_u64().unwrap() >= 1,
            "publisher host discovered: {report}"
        );

        let listed = call(
            router(AppState::new(Arc::clone(&st.runtime))),
            "/sources/discoveries?status=discovered",
        )
        .await;
        let host = listed["discoveries"][0]["host"].as_str().unwrap();
        assert_eq!(host, "publisher.example.com");

        let (status, registered) = call_post(
            router(AppState::new(Arc::clone(&st.runtime))),
            &format!("/sources/discoveries/{host}/register"),
            serde_json::json!({"workspace_id": "default"}),
        )
        .await;
        assert_eq!(status, 200, "{registered}");
        assert_eq!(registered["registered"], true);
        assert_eq!(registered["source_id"], "publisher_publisher_example_com");

        // The promoted source is live in the federated registry AND persisted
        // in the catalog.
        let catalog = call(
            router(AppState::new(Arc::clone(&st.runtime))),
            "/sources?persisted=true",
        )
        .await;
        assert!(
            catalog["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["source_id"] == "publisher_publisher_example_com"),
            "promoted source in the persisted catalog: {catalog}"
        );
        let live = call(router(AppState::new(st.runtime)), "/sources").await;
        assert!(
            live["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["source_id"] == "publisher_publisher_example_com"),
            "promoted source live in the registry: {live}"
        );
    }

    #[tokio::test]
    async fn search_stream_emits_per_result_events_and_a_final_payload() {
        use tower::ServiceExt;
        let (st, _d) = state().await;
        // Seed one local document so there is at least one result.
        let mut req = searchos_application::ingest_service::IngestRequest::new(
            searchos_domain::ids::WorkspaceId::new("default"),
        );
        req.source_key = Some("notes/stream".into());
        st.runtime
            .application
            .ingest_service
            .ingest_text(req, "streaming evidence about tokio runtime scheduling")
            .await
            .unwrap();
        st.runtime.process_outbox(16).await.unwrap();

        let resp = router(st)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/search/stream")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "query": "tokio runtime scheduling",
                            "mode": "local_only",
                            "workspace_id": "default",
                            "max_results": 5
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
            "text/event-stream"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 4 << 20)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("event: start"), "{text}");
        assert!(text.contains("event: quality"), "{text}");
        assert!(text.contains("event: final"), "{text}");
        assert!(text.contains("event: done"), "{text}");
        // The final event carries the same task id the header event announced.
        let final_data = text
            .lines()
            .skip_while(|l| !l.starts_with("event: final"))
            .nth(1)
            .and_then(|l| l.strip_prefix("data: "))
            .unwrap();
        let final_json: serde_json::Value = serde_json::from_str(final_data).unwrap();
        assert!(final_json["task_id"].as_str().is_some());
        assert!(final_json.get("results").is_some());
        if final_json["results"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            assert!(text.contains("event: result"), "{text}");
        }
    }

    #[tokio::test]
    async fn entity_endpoints_register_resolve_and_list() {
        let (st, _d) = state().await;
        let app = router(st);
        use tower::ServiceExt;
        let post = |app: axum::Router, uri: &str, body: serde_json::Value| {
            let uri = uri.to_string();
            async move {
                let resp = app
                    .oneshot(
                        axum::http::Request::builder()
                            .method("POST")
                            .uri(uri)
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status().as_u16();
                let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                    .await
                    .unwrap();
                (
                    status,
                    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                )
            }
        };

        let (status, registered) = post(
            app.clone(),
            "/entities",
            serde_json::json!({
                "canonical_name": "Tokio",
                "aliases": ["tokio async runtime"]
            }),
        )
        .await;
        assert_eq!(status, 200, "{registered}");
        assert!(registered["conflicts"].as_array().unwrap().is_empty());

        let (status, matched) = post(
            app.clone(),
            "/entities/resolve",
            serde_json::json!({ "text": "the tokio async runtime is fast" }),
        )
        .await;
        assert_eq!(status, 200, "{matched}");
        assert_eq!(matched["canonical_name"], "Tokio");

        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/entities")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let list: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, 200, "{list}");
        assert_eq!(list["alias_count"], 2);
    }

    #[tokio::test]
    async fn as_of_rejects_malformed_rfc3339() {
        let (st, _d) = state().await;
        let app = router(st);
        use tower::ServiceExt;
        for uri in ["/search", "/search/plan"] {
            let resp = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            serde_json::json!({"query": "q", "as_of": "not-a-date"}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 400, "{uri} must reject a bad as_of");
        }
    }

    #[tokio::test]
    async fn research_endpoints_run_and_inspect_the_workspace() {
        let (st, _d) = state().await;
        // Seed a document so the run can serve local evidence.
        st.runtime
            .application
            .ingest_service
            .ingest_text(
                searchos_application::ingest_service::IngestRequest::new(
                    searchos_domain::ids::WorkspaceId::new("default"),
                ),
                "Research workspace endpoint evidence about bounded deep search rounds.",
            )
            .await
            .unwrap();
        st.runtime.process_outbox(256).await.unwrap();
        let app = router(st);

        use tower::ServiceExt;
        let post = |app: axum::Router, uri: &str, body: serde_json::Value| {
            let uri = uri.to_string();
            async move {
                let resp = app
                    .oneshot(
                        axum::http::Request::builder()
                            .method("POST")
                            .uri(uri)
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status().as_u16();
                let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                    .await
                    .unwrap();
                (
                    status,
                    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                )
            }
        };

        let (status, run) = post(
            app.clone(),
            "/research/run",
            serde_json::json!({
                "query": "bounded deep search rounds",
                "session_id": "research_http_gate",
                "max_rounds": 2
            }),
        )
        .await;
        assert_eq!(status, 200, "{run}");
        assert_eq!(run["session_id"], "research_http_gate");
        assert!(run["rounds"].as_array().unwrap().len() <= 2);

        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/research/research_http_gate")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let workspace: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, 200, "{workspace}");
        assert!(!workspace["turns"].as_array().unwrap().is_empty());
        assert!(!workspace["evidence"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn memory_lifecycle_endpoint_dry_runs_and_applies() {
        let (st, _d) = state().await;
        // Seed one entry outside the default 365-day window.
        let now = chrono::Utc::now();
        let repo = searchos_storage_sqlite::repos::MemoryRepo::new(&st.runtime.sqlite);
        repo.insert(&searchos_domain::memory::MemoryEntry {
            id: searchos_domain::ids::MemoryEntryId::generate(),
            namespace: searchos_domain::memory::MemoryNamespace {
                workspace: WorkspaceId::new("default"),
                project: None,
                agent: None,
            },
            sharing: searchos_domain::memory::SharingPolicy::WorkspaceShared,
            title: "ancient note".into(),
            content: "ancient memory content for the lifecycle gate".into(),
            url: None,
            tags: Vec::new(),
            created_at: now - chrono::Duration::days(400),
            updated_at: now - chrono::Duration::days(400),
            embedding_status: "none".into(),
        })
        .unwrap();
        let app = router(st);

        use tower::ServiceExt;
        let post = |app: axum::Router, body: serde_json::Value| async move {
            let resp = app
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri("/memory/lifecycle")
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (status, value)
        };

        let (status, dry) = post(app.clone(), serde_json::json!({})).await;
        assert_eq!(status, 200, "{dry}");
        assert_eq!(dry["expired"], 1);
        assert_eq!(dry["applied"], false);

        let (status, applied) = post(app, serde_json::json!({ "apply": true })).await;
        assert_eq!(status, 200, "{applied}");
        assert_eq!(applied["applied"], true);
        assert_eq!(applied["expired"], 1);
    }

    #[tokio::test]
    async fn policy_endpoints_submit_audit_and_rollback() {
        let (st, _d) = state().await;
        let app = router(st);

        use tower::ServiceExt;
        let call = |app: axum::Router, method: &str, uri: &str, body: serde_json::Value| {
            let (method, uri) = (method.to_string(), uri.to_string());
            async move {
                let resp = app
                    .oneshot(
                        axum::http::Request::builder()
                            .method(method.as_str())
                            .uri(uri)
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status().as_u16();
                let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                    .await
                    .unwrap();
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
                (status, value)
            }
        };

        let stricter = searchos_policy::QualityPolicy {
            minimum_quality: 0.91,
            version: "test-v2".into(),
            ..Default::default()
        };
        let (status, submitted) = call(
            app.clone(),
            "PUT",
            "/policies/quality",
            serde_json::json!({ "policy": stricter, "notes": "HTTP submit" }),
        )
        .await;
        assert_eq!(status, 200, "{submitted}");
        assert_eq!(submitted["version"], 2);
        assert_eq!(submitted["status"], "active");

        let (status, active) = call(app.clone(), "GET", "/policies", serde_json::json!({})).await;
        assert_eq!(status, 200);
        let rows = active["policies"].as_array().unwrap();
        assert!(rows
            .iter()
            .any(|r| r["kind"] == "quality" && r["version"] == 2));

        let (status, history) = call(
            app.clone(),
            "GET",
            "/policies/quality/versions",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(history["versions"].as_array().unwrap().len(), 2);

        let (status, rolled) = call(
            app.clone(),
            "POST",
            "/policies/quality/rollback",
            serde_json::json!({ "version": 1 }),
        )
        .await;
        assert_eq!(status, 200, "{rolled}");
        assert_eq!(rolled["version"], 1);

        // Invalid payload / kind / version are explicit client errors.
        assert_eq!(
            call(
                app.clone(),
                "PUT",
                "/policies/quality",
                serde_json::json!({})
            )
            .await
            .0,
            400
        );
        assert_eq!(
            call(
                app.clone(),
                "PUT",
                "/policies/unknown",
                serde_json::json!({ "policy": {} })
            )
            .await
            .0,
            400
        );
        assert_eq!(
            call(
                app,
                "POST",
                "/policies/quality/rollback",
                serde_json::json!({ "version": 999 })
            )
            .await
            .0,
            400
        );
    }

    #[tokio::test]
    async fn replay_and_evaluation_endpoints_round_trip() {
        let (st, _d) = state().await;
        // Record a real trace through the runtime's own search service.
        let req = searchos_domain::request::SearchRequest::new(
            "replay endpoint round trip",
            searchos_domain::ids::ProfileId::general(),
            searchos_domain::request::SearchMode::LocalOnly,
            searchos_domain::ids::WorkspaceId::new("default"),
        )
        .unwrap();
        let trace_id = st
            .runtime
            .application
            .search_service
            .search(req)
            .await
            .unwrap()
            .trace_id;
        let app = router(st);

        use tower::ServiceExt;
        let post = |app: axum::Router, uri: String, body: serde_json::Value| async move {
            let resp = app
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (status, value)
        };

        // RFC-040: offline replay of a recorded trace.
        let (status, replay) = post(
            app.clone(),
            format!("/replay/{}", trace_id.as_str()),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, 200, "{replay}");
        assert_eq!(replay["trace_id"], trace_id.as_str());
        assert_eq!(replay["offline"], true);

        // RFC-075: the same endpoint accepts a policy override.
        let (status, simulated) = post(
            app.clone(),
            format!("/replay/{}", trace_id.as_str()),
            serde_json::json!({ "policy": {"minimum_quality": 1.0} }),
        )
        .await;
        assert_eq!(status, 200, "{simulated}");
        assert_eq!(simulated["policy_applied"], true);
        assert!(simulated["baseline"].is_object());

        // RFC-086: batch evaluation over recent traces.
        let (status, evaluation) = post(
            app,
            "/evaluation/replay".into(),
            serde_json::json!({ "limit": 5, "workspace": "default" }),
        )
        .await;
        assert_eq!(status, 200, "{evaluation}");
        assert!(evaluation["evaluated"].as_u64().unwrap() >= 1);
        assert!(evaluation["decision_agreement_rate"].is_number());

        // An invalid policy override is a client error, not a silent ignore.
        let (status, bad) = post(
            router(state().await.0),
            format!("/replay/{}", trace_id.as_str()),
            serde_json::json!({ "policy": {"minimum_quality": "not-a-number"} }),
        )
        .await;
        assert_eq!(status, 400, "{bad}");
    }

    #[tokio::test]
    async fn deletions_endpoint_serves_the_propagation_audit() {
        let (st, _d) = state().await;
        let repo = searchos_storage_sqlite::repos::DeletionAuditRepo::new(&st.runtime.sqlite);
        repo.record(&searchos_storage_sqlite::repos::DeletionAuditEntry {
            id: "del_test".into(),
            document_id: "doc_gone".into(),
            workspace_id: Some("default".into()),
            content_hash: Some("hash_abc".into()),
            chunk_count: 3,
            outbox_events: 3,
            index_verified: true,
            cache_invalidated: true,
            deleted_at: "2026-11-14T00:00:00Z".into(),
        })
        .unwrap();
        repo.record(&searchos_storage_sqlite::repos::DeletionAuditEntry {
            id: "del_other".into(),
            document_id: "doc_other".into(),
            workspace_id: Some("ws_other".into()),
            content_hash: None,
            chunk_count: 0,
            outbox_events: 0,
            index_verified: true,
            cache_invalidated: false,
            deleted_at: "2026-11-13T00:00:00Z".into(),
        })
        .unwrap();
        let app = router(st);

        use tower::ServiceExt;
        let app = std::sync::Arc::new(app);
        let request = |uri: &'static str| {
            let app = app.clone();
            async move {
                let resp = (*app)
                    .clone()
                    .oneshot(
                        axum::http::Request::builder()
                            .method("GET")
                            .uri(uri)
                            .body(axum::body::Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status().as_u16();
                let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                    .await
                    .unwrap();
                let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                (status, body)
            }
        };

        let (status, body) = request("/deletions").await;
        assert_eq!(status, 200, "{body}");
        let rows = body["deletions"].as_array().unwrap();
        assert!(rows.iter().any(|r| r["document_id"] == "doc_gone"));

        let (status, filtered) = request("/deletions?document_id=doc_gone").await;
        assert_eq!(status, 200);
        let rows = filtered["deletions"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["index_verified"], true);
        assert_eq!(rows[0]["chunk_count"], 3);
    }

    #[tokio::test]
    async fn system_status_reports_the_federated_catalog() {
        let (st, _d) = state().await;
        let app = router(st);
        let (status, body) = {
            use tower::ServiceExt;
            let resp = app
                .oneshot(
                    axum::http::Request::builder()
                        .method("GET")
                        .uri("/system/status")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (status, body)
        };
        assert_eq!(status, 200, "{body}");
        let federation = &body["federation"];
        assert_eq!(federation["strategy"], "legacy");
        let sources = federation["sources"].as_array().unwrap();
        assert!(
            sources.iter().any(|s| s["source_id"] == "local_knowledge"),
            "the local index source is registered even in legacy mode: {sources:?}"
        );
        assert!(sources.iter().all(|s| s["billable"] == false));
    }

    #[tokio::test]
    async fn federated_plan_is_visible_on_the_plan_surface() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = searchos_core::SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        cfg.federation.strategy = "plan_only".into();
        let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        let app = router(AppState::new(rt));
        let (status, body) = call_post(
            app,
            "/search/plan",
            serde_json::json!({"query": "rust edition 2024"}),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["federation_strategy"], "auto");
        let sources = body["federated_sources"].as_array().unwrap();
        assert!(
            sources.iter().any(|s| s == "local_knowledge"),
            "the local source must appear in the federated plan: {sources:?}"
        );
        // Planning never executed anything: the plan stays cost-estimated.
        assert!(body.get("trace_id").is_none());
    }

    #[tokio::test]
    async fn legacy_mode_plan_has_no_federation_fields() {
        let (st, _d) = state().await;
        let app = router(st);
        let (status, body) = call_post(
            app,
            "/search/plan",
            serde_json::json!({"query": "rust edition 2024"}),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert!(body["federation_strategy"].is_null());
        assert_eq!(body["federated_sources"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn context_budget_round_trips_through_http_search() {
        let (st, _d) = state().await;
        let app = router(st);

        let (status, body) = call_post(
            app.clone(),
            "/search",
            serde_json::json!({
                "query": "rust release notes",
                "max_results": 3,
                "context_budget": {
                    "max_tokens": 600,
                    "reserved_tokens": 50,
                    "max_sources": 4,
                    "max_chunks_per_source": 2,
                    "deduplicate": true,
                    "preserve_citations": true,
                    "include_conflicts": true
                }
            }),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let context = &body["context"];
        assert!(
            context.is_object(),
            "a declared budget must yield a context block"
        );
        assert_eq!(
            context["status"].as_str(),
            Some("complete"),
            "the fixture corpus fits easily; anything else would be a budget bug"
        );
        assert!(
            context["used_tokens"].as_u64().unwrap() <= context["usable_tokens"].as_u64().unwrap()
        );
        let citations = context["citations"].as_array().unwrap();
        assert_eq!(
            citations.len(),
            context["evidence"].as_array().unwrap().len(),
            "compression never deletes a citation"
        );
        for evidence in context["evidence"].as_array().unwrap() {
            assert!(
                !evidence["citation"].as_str().unwrap_or("").is_empty(),
                "every entry keeps its citation"
            );
        }

        // An invalid budget is a client error, not a silently ignored field.
        let (status, body) = call_post(
            app,
            "/search",
            serde_json::json!({
                "query": "rust release notes",
                "context_budget": { "max_tokens": 10, "reserved_tokens": 20 }
            }),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"]["code"], "VALIDATION_FAILED");
    }

    #[tokio::test]
    async fn search_accepts_native_and_tavily_shapes() {
        let (st, _d) = state().await;
        let app = router(st);

        // Native SearchOS contract: the response keeps the full envelope.
        let (status, native) = call_post(
            app.clone(),
            "/search",
            serde_json::json!({"query": "q", "max_results": 1}),
        )
        .await;
        assert_eq!(status, 200, "{native}");
        assert!(native["trace_id"].is_string());
        assert!(native["quality"].is_object());
        assert!(
            native.get("answer").is_none(),
            "native path must stay native"
        );

        // Tavily-shaped body: routed through the compatibility layer.
        let (status, tavily) = call_post(
            app.clone(),
            "/search",
            serde_json::json!({
                "query": "q",
                "search_depth": "basic",
                "max_results": 3,
                "include_answer": true
            }),
        )
        .await;
        assert_eq!(status, 200, "{tavily}");
        assert!(tavily["results"].is_array());
        assert!(tavily["answer"].is_null());
        assert!(tavily["response_time"].is_number());
        assert!(
            tavily["unsupported_features"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f.as_str().unwrap_or("").contains("include_answer")),
            "unsupported features reported: {tavily}"
        );
        assert!(tavily.get("trace_id").is_none());
    }

    /// Seeds one local document through the real ingest path and returns the
    /// repo row, so the document-search endpoint can be tested against real
    /// SQLite state instead of a mocked list.
    async fn seed_local_document(st: &AppState, source_key: &str, title: &str) {
        seed_document_in_workspace(
            st,
            "default",
            source_key,
            title,
            "local document body for document search",
        )
        .await;
    }

    async fn seed_document_in_workspace(
        st: &AppState,
        workspace: &str,
        source_key: &str,
        title: &str,
        content: &str,
    ) {
        let mut req =
            searchos_application::ingest_service::IngestRequest::new(WorkspaceId::new(workspace));
        req.source_key = Some(source_key.into());
        req.title = Some(title.into());
        st.runtime
            .application
            .ingest_service
            .ingest_text(req, content)
            .await
            .unwrap();
        let _ = st.runtime.process_outbox(100).await;
    }

    #[tokio::test]
    async fn document_search_uses_sql_lookup_and_matches_title_or_url() {
        let (st, _d) = state().await;
        seed_local_document(&st, "notes/alpha", "Alpha notes").await;
        let app = router(st);
        let (status, body) = call_post(
            app.clone(),
            "/documents/search",
            serde_json::json!({"query": "alpha", "limit": 5}),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let docs = body["documents"].as_array().unwrap();
        assert!(
            docs.iter().any(|d| d["canonical_url"]
                .as_str()
                .unwrap_or("")
                .contains("searchos-local://default/notes/alpha")),
            "title match via SQL lookup: {docs:?}"
        );
        // URL substring search still works and respects the limit.
        let (_, body) = call_post(
            app,
            "/documents/search",
            serde_json::json!({"query": "notes/alpha", "limit": 1}),
        )
        .await;
        assert_eq!(body["documents"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_mode_aliases_are_accepted_and_unknown_modes_rejected() {
        let (st, _d) = state().await;
        let app = router(st);
        let (status, body) = call_post(
            app.clone(),
            "/search",
            serde_json::json!({"query": "q", "mode": "local-only"}),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let (status, body) = call_post(
            app,
            "/search",
            serde_json::json!({"query": "q", "mode": "LOCAL_ONLY"}),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"]["code"], "VALIDATION_FAILED");
    }

    #[tokio::test]
    async fn stats_and_cost_endpoints() {
        let (st, _d) = state().await;
        let app = router(st);
        let stats = call(app.clone(), "/stats/search").await;
        // Metrics are a process-global registry shared by every test in this
        // binary; assert the field exists and is numeric, not an exact
        // count that breaks whenever another test runs a search.
        assert!(stats["search_requests"].is_number(), "{stats}");
        let cost = call(app, "/cost/summary").await;
        assert!(cost["summary"].is_object());
        assert_eq!(cost["is_estimate"], true);
    }

    /// RFC-049: the structured-extraction endpoint returns the parsed
    /// structures and the header-keyed table records.
    #[tokio::test]
    async fn structures_endpoint_extracts_blocks_and_records() {
        let (st, _d) = state().await;
        let app = router(st);
        let doc = "## Parameters\n\n| Name | Type |\n|---|---|\n| top_k | int |\n| timeout | int |\n\n```rust\nlet x = 1;\n```";
        let (status, body) =
            call_post(app, "/structures", serde_json::json!({ "text": doc })).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["counts"]["tables"], 1, "{body}");
        let table = &body["tables"][0];
        assert_eq!(table["headers"][0], "Name");
        assert_eq!(table["rows"][0]["Name"], "top_k");
        assert_eq!(table["rows"][0]["Type"], "int");
        let blocks = body["blocks"].as_array().unwrap();
        assert!(blocks
            .iter()
            .any(|b| b["kind"] == "code_block" && b["language"] == "rust"));
        assert!(blocks.iter().any(|b| b["kind"] == "heading"));
    }

    /// RFC-049: a request without text or url is a client error, not an
    /// empty success.
    #[tokio::test]
    async fn structures_endpoint_requires_text_or_url() {
        let (st, _d) = state().await;
        let app = router(st);
        let (status, body) = call_post(app, "/structures", serde_json::json!({})).await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"]["code"], "validation");
    }

    async fn call(app: Router, path: &str) -> serde_json::Value {
        use tower::ServiceExt;
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    async fn call_post(
        app: Router,
        path: &str,
        body: serde_json::Value,
    ) -> (u16, serde_json::Value) {
        use tower::ServiceExt;
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    async fn call_delete(app: Router, path: &str) -> (u16, serde_json::Value) {
        use tower::ServiceExt;
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn versioned_routes_mirror_unversioned() {
        let (st, _d) = state().await;
        let app = router(st);
        let plain = call(app.clone(), "/health").await;
        let versioned = call(app.clone(), "/v1/health").await;
        assert_eq!(plain["status"], versioned["status"]);
        let v1_cost = call(app, "/v1/cost/summary").await;
        assert!(v1_cost["summary"].is_object());
    }

    #[tokio::test]
    async fn governance_endpoints_are_live() {
        let (st, _d) = state().await;
        let app = router(st);
        let profiles = call(app.clone(), "/profiles").await;
        assert_eq!(profiles["profiles"].as_array().unwrap().len(), 6);
        let latest = call(app.clone(), "/profiles/latest").await;
        assert_eq!(latest["id"], "latest");
        let savings = call(app.clone(), "/stats/savings").await;
        assert_eq!(savings["is_estimate"], true);
        assert!(savings["baseline_micros"].is_number());
        let retrieval = call(app.clone(), "/stats/retrieval").await;
        assert!(retrieval["cache_hit_rate"].is_number());
        let reservations = call(app.clone(), "/cost/reservations").await;
        assert!(reservations["reservations"].is_array());
        let docs = call_post(
            app,
            "/documents/search",
            serde_json::json!({"query": "example.com"}),
        )
        .await;
        assert_eq!(docs.0, 200);
        assert!(docs.1["documents"].is_array());
    }

    #[tokio::test]
    async fn workspace_binding_is_enforced() {
        use tower::ServiceExt;
        let (mut st, _d) = state().await;
        st.auth_token = "token-a".into();
        st.workspace_tokens.insert("token-a".into(), "ws-a".into());
        let app = router(st);
        let denied = call_post(
            app.clone(),
            "/search",
            serde_json::json!({"query": "q", "workspace_id": "ws-b"}),
        )
        .await;
        // Anonymous token → 401 before workspace check.
        assert_eq!(denied.0, 401);
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/search")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer token-a")
                    .body(axum::body::Body::from(
                        serde_json::json!({"query": "q", "workspace_id": "ws-b"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403);
    }

    /// RFC-029 AC1: POST /ingest returns 201 and the text is searchable local-only.
    #[tokio::test]
    async fn ingest_text_returns_201_and_is_searchable_local_only() {
        let (st, _d) = state().await;
        let app = router(st.clone());
        let (status, body) = call_post(
            app.clone(),
            "/ingest",
            serde_json::json!({"content": "quokka habitat zanzibar"}),
        )
        .await;
        assert_eq!(status, 201, "{body}");
        assert!(body["document_id"].as_str().is_some());
        st.runtime.process_outbox(10).await.unwrap();
        let (status, search) = call_post(
            app,
            "/search",
            serde_json::json!({"query": "quokka habitat", "mode": "local_only"}),
        )
        .await;
        assert_eq!(status, 200, "{search}");
        assert!(!search["results"].as_array().unwrap().is_empty());
    }

    /// RFC-029 AC7: an oversized upload is rejected with no partial document.
    #[tokio::test]
    async fn ingest_oversize_rejected_without_partial_document() {
        let (st, _d) = state_with_ingest_limits(10, 100).await;
        let app = router(st.clone());
        let (status, body) = call_post(
            app,
            "/ingest",
            serde_json::json!({"content": "this content is definitely too large"}),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"]["code"], "VALIDATION_FAILED");
        let docs = searchos_storage_sqlite::repos::DocumentRepo::new(&st.runtime.sqlite)
            .list(10)
            .unwrap();
        assert!(docs.is_empty(), "oversize content must not persist");
    }

    /// RFC-029 AC7: multipart /ingest/file enforces the same bound.
    #[tokio::test]
    async fn ingest_file_oversize_rejected_without_partial_document() {
        use tower::ServiceExt;
        let (st, _d) = state_with_ingest_limits(10, 100).await;
        let app = router(st.clone());
        let boundary = "----searchos-test-boundary";
        let payload = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.txt\"\r\nContent-Type: text/plain\r\n\r\n{}\r\n--{boundary}--\r\n",
            "x".repeat(20)
        );
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/ingest/file")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(axum::body::Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);
        let docs = searchos_storage_sqlite::repos::DocumentRepo::new(&st.runtime.sqlite)
            .list(10)
            .unwrap();
        assert!(docs.is_empty());
    }

    /// Parses `docs/openapi.yaml` into a set of `(METHOD, path)` operations.
    fn openapi_operations() -> std::collections::BTreeSet<(String, String)> {
        #[derive(Debug, serde::Deserialize)]
        struct Spec {
            paths: std::collections::BTreeMap<
                String,
                std::collections::BTreeMap<String, serde_yaml::Value>,
            >,
        }
        const SPEC: &str = include_str!("../../../docs/openapi.yaml");
        let spec: Spec =
            serde_yaml::from_str(SPEC).expect("docs/openapi.yaml must parse as OpenAPI YAML");
        const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
        let mut out = std::collections::BTreeSet::new();
        for (path, operations) in spec.paths {
            for method in operations.keys() {
                let method = method.to_uppercase();
                assert!(
                    METHODS.contains(&method.as_str()),
                    "unsupported OpenAPI method '{method}' for {path}"
                );
                out.insert((method, path.clone()));
            }
        }
        out
    }

    /// The routes the gateway actually registers, from the same table the
    /// router is built from.
    fn registered_operations() -> std::collections::BTreeSet<(String, String)> {
        api_route_table()
            .into_iter()
            .chain(dashboard_route_table())
            .map(|spec| (spec.method.to_string(), spec.spec_path().to_string()))
            .collect()
    }

    /// The OpenAPI document is the frozen external contract: every documented
    /// operation must exist, and every registered route must be documented.
    #[test]
    fn openapi_spec_matches_the_route_table() {
        let documented = openapi_operations();
        let registered = registered_operations();
        assert!(!registered.is_empty(), "route table is empty");

        let undocumented: Vec<_> = registered.difference(&documented).collect();
        let missing: Vec<_> = documented.difference(&registered).collect();
        assert!(
            undocumented.is_empty(),
            "routes registered but NOT documented in docs/openapi.yaml: {undocumented:?}"
        );
        assert!(
            missing.is_empty(),
            "paths documented in docs/openapi.yaml but NOT registered: {missing:?}"
        );
    }

    /// The route table is also the thing that must respond. A path that does
    /// not exist at all returns 404; a path registered under another method
    /// returns 405. Handlers legitimately return 404 for unknown ids, so the
    /// dynamic paths are checked against 405 only.
    #[tokio::test]
    async fn every_registered_route_resolves() {
        use tower::ServiceExt;
        let (st, _d) = state().await;
        let app = router(st);
        for spec in api_route_table() {
            let dynamic = spec.path.contains('{');
            let resp = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(spec.method)
                        .uri(spec.path.replace('{', "probe").replace('}', ""))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_ne!(
                resp.status(), // axum::http::StatusCode::METHOD_NOT_ALLOWED
                axum::http::StatusCode::METHOD_NOT_ALLOWED,
                "{} {} is not registered under that method",
                spec.method,
                spec.path
            );
            if !dynamic {
                assert_ne!(
                    resp.status(),
                    axum::http::StatusCode::NOT_FOUND,
                    "{} {} does not resolve",
                    spec.method,
                    spec.path
                );
            }
        }
    }

    /// RFC-029 document lifecycle as the console drives it: ingest → list →
    /// versions → reindex → delete → verify it is no longer retrievable.
    #[tokio::test]
    async fn document_lifecycle_ingest_list_versions_reindex_delete() {
        let (st, _d) = state().await;
        let app = router(st.clone());

        // 1. Ingest one local text document.
        let (status, body) = call_post(
            app.clone(),
            "/ingest",
            serde_json::json!({
                "content": "lifecycle smoke content about hybrid retrieval",
                "source_key": "notes/lifecycle",
                "title": "Lifecycle note",
                "workspace_id": "default"
            }),
        )
        .await;
        assert_eq!(status, 201, "{body}");
        let doc_id = body["document_id"].as_str().unwrap_or_default().to_string();
        assert!(doc_id.starts_with("doc_"), "{body}");
        assert_eq!(body["source_type"], "local_text");
        assert!(body["chunk_count"].as_u64().unwrap_or(0) >= 1, "{body}");

        // 2. The console list carries the lifecycle status and counts.
        let list = call(app.clone(), "/documents?workspace_id=default&limit=10").await;
        let rows = list["documents"].as_array().unwrap();
        let row = rows
            .iter()
            .find(|row| row["id"] == doc_id.as_str())
            .unwrap_or_else(|| panic!("ingested document missing from list: {list}"));
        assert_eq!(row["status"], "normalized");
        assert_eq!(row["source_type"], "local_text");
        assert_eq!(row["version_count"], 1);
        assert!(row["chunk_count"].as_i64().unwrap_or(0) >= 1);

        // 3. Detail + version history.
        let detail = call(app.clone(), &format!("/documents/{doc_id}")).await;
        assert_eq!(detail["summary"]["id"], doc_id.as_str());
        assert!(detail["current_version"].is_object(), "{detail}");
        assert_eq!(detail["versions"].as_array().unwrap().len(), 1);
        let versions = call(app.clone(), &format!("/documents/{doc_id}/versions")).await;
        assert_eq!(versions["versions"].as_array().unwrap().len(), 1);
        let hash = versions["versions"][0]["content_hash"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(hash.len(), 64, "sha256 hex content hash");

        // 4. Reindex rewrites the index entries without touching metadata.
        let (status, reindexed) = call_post(
            app.clone(),
            &format!("/documents/{doc_id}/reindex"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, 200, "{reindexed}");
        assert!(
            reindexed["reindexed"].as_u64().unwrap_or(0) >= 1,
            "{reindexed}"
        );
        let after = call(app.clone(), &format!("/documents/{doc_id}")).await;
        assert_eq!(
            after["versions"].as_array().unwrap().len(),
            1,
            "reindex is not a new version"
        );

        // 5. The content is searchable before the delete.
        let (status, found) = call_post(
            app.clone(),
            "/documents/search",
            serde_json::json!({"query": "lifecycle", "limit": 10, "workspace_id": "default"}),
        )
        .await;
        assert_eq!(status, 200, "{found}");
        assert_eq!(found["documents"].as_array().unwrap().len(), 1);

        // 6. Delete is a soft delete + index removal, visible immediately.
        let (status, deleted) = call_delete(app.clone(), &format!("/documents/{doc_id}")).await;
        assert_eq!(status, 200, "{deleted}");
        assert_eq!(deleted["deleted"], true);
        let gone = call(app.clone(), &format!("/documents/{doc_id}")).await;
        assert_eq!(gone["error"]["code"], "NOT_FOUND", "{gone}");

        // 7. …and it is no longer retrievable, in either listing.
        let (_, after_delete) = call_post(
            app.clone(),
            "/documents/search",
            serde_json::json!({"query": "lifecycle", "limit": 10, "workspace_id": "default"}),
        )
        .await;
        assert!(
            after_delete["documents"].as_array().unwrap().is_empty(),
            "deleted document must not be searchable: {after_delete}"
        );
        let live = call(app.clone(), "/documents?workspace_id=default&limit=10").await;
        assert!(
            !live["documents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == doc_id.as_str()),
            "deleted document must not appear in the live list: {live}"
        );
        // The audit trail keeps the soft-deleted row when explicitly asked for.
        let deleted_rows = call(
            app,
            "/documents?workspace_id=default&status=deleted&limit=10",
        )
        .await;
        assert!(
            deleted_rows["documents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == doc_id.as_str()),
            "soft-deleted row stays visible under status=deleted: {deleted_rows}"
        );
    }

    /// RFC-029 AC9: an in-memory directory batch reports per-item outcomes.
    #[tokio::test]
    async fn ingest_batch_directory_reports_items() {
        let (st, _d) = state().await;
        let app = router(st.clone());
        let (status, body) = call_post(
            app,
            "/ingest/batch",
            serde_json::json!({
                "directory": {
                    "files": [
                        {"file_name": "a.txt", "content": "alpha batch body", "source_key": "batch/a"},
                        {"file_name": "b.txt", "content": "beta batch body", "source_key": "batch/b"}
                    ]
                }
            }),
        )
        .await;
        assert_eq!(status, 201, "{body}");
        assert_eq!(body["state"], "completed");
        assert_eq!(body["total_items"], 2);
        assert_eq!(body["ok_items"], 2);
        assert_eq!(body["failed_items"], 0);
    }

    /// Part 30 §30.11: a token bound to ws-a cannot read ws-b document rows.
    #[tokio::test]
    async fn workspace_token_cannot_search_other_workspace_documents() {
        use tower::ServiceExt;
        let (mut st, _d) = state().await;
        seed_document_in_workspace(
            &st,
            "ws_a",
            "notes/alpha",
            "Alpha zanzibar",
            "zanzibar knowledge alpha",
        )
        .await;
        seed_document_in_workspace(
            &st,
            "ws_b",
            "notes/beta",
            "Beta zanzibar",
            "zanzibar knowledge beta",
        )
        .await;
        st.auth_token = "token-a".into();
        st.workspace_tokens.insert("token-a".into(), "ws_a".into());
        let app = router(st);
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/documents/search")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer token-a")
                    .body(axum::body::Body::from(
                        serde_json::json!({"query": "zanzibar"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let docs = body["documents"].as_array().unwrap();
        assert!(!docs.is_empty());
        assert!(docs
            .iter()
            .all(|d| d["workspace_id"].as_str().unwrap() == "ws_a"));
        assert!(!docs
            .iter()
            .any(|d| d["canonical_url"].as_str().unwrap().contains("ws_b")));
    }

    #[tokio::test]
    async fn console_read_endpoints_are_live() {
        let (st, _d) = state().await;
        let app = router(st);
        let traces = call(app.clone(), "/traces?limit=5").await;
        assert!(traces["traces"].is_array());
        let status = call(app.clone(), "/system/status").await;
        assert_eq!(status["database_ok"], true);
        assert!(status["schema_version"].as_i64().unwrap() >= 2);
        assert_eq!(status["api_version"], "1");
        let v1 = call(app, "/v1/traces").await;
        assert!(v1["traces"].is_array());
    }

    #[tokio::test]
    async fn cors_preflight_and_headers() {
        use tower::ServiceExt;
        let (st, _d) = state().await;
        let app = router(st);
        let resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("OPTIONS")
                    .uri("/search")
                    .header("origin", "http://localhost:3100")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 204);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .unwrap()
                .to_str()
                .unwrap(),
            "http://localhost:3100"
        );
        let get_resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .header("origin", "http://localhost:3100")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(get_resp
            .headers()
            .contains_key("access-control-allow-origin"));
    }

    #[tokio::test]
    async fn dashboard_sse_releases_connection_slot_after_drop() {
        use tower::ServiceExt;
        let (st, _d) = state().await;
        let app = router(st);
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/dashboard/events")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(
            ACTIVE_SSE_CONNECTIONS.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "SSE slot held while the response is alive"
        );
        drop(resp);
        assert_eq!(
            ACTIVE_SSE_CONNECTIONS.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "SSE slot released after drop"
        );
    }

    #[tokio::test]
    async fn tavily_search_is_scoped_to_its_workspace() {
        let (st, _d) = state().await;
        let app = router(st);
        let (status, body) = call_post(
            app,
            "/search",
            serde_json::json!({
                "query": "q",
                "search_depth": "basic",
                "workspace_id": "ws_tavily",
                "max_results": 2
            }),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert!(body["results"].is_array());
    }

    #[tokio::test]
    async fn cors_allowlist_accepts_multiple_configured_origins() {
        use tower::ServiceExt;
        let (st, _d) = state().await;
        // `AppState` owns the runtime inside an Arc; rebuild the state with
        // the same runtime and a multi-origin allowlist for this test only.
        let runtime = Arc::clone(&st.runtime);
        let mut cfg = runtime.config.clone();
        cfg.server.cors_origin = "http://localhost:3100,http://127.0.0.1:3100".into();
        let st = AppState::new_with_config(runtime, cfg);
        let app = router(st);
        for origin in ["http://localhost:3100", "http://127.0.0.1:3100"] {
            let resp = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("OPTIONS")
                        .uri("/search")
                        .header("origin", origin)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 204);
            assert_eq!(
                resp.headers()
                    .get("access-control-allow-origin")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                origin
            );
        }
        // An origin outside the allowlist gets no ACAO header.
        let denied = app
            .oneshot(
                axum::http::Request::builder()
                    .method("OPTIONS")
                    .uri("/search")
                    .header("origin", "http://evil.example")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!denied.headers().contains_key("access-control-allow-origin"));
    }

    #[tokio::test]
    async fn dashboard_api_serves_read_model_envelopes() {
        use tower::ServiceExt;
        let (st, _d) = state().await;
        let app = router(st);
        // The route table and the Read Model map share one source of truth;
        // every registered dataset path must resolve.
        for (path, _) in DASHBOARD_DATASETS {
            let body = call(app.clone(), &format!("/api/v1/dashboard/{path}")).await;
            assert!(
                body.get("error").is_none(),
                "dashboard dataset {path} failed: {body}"
            );
        }
        let overview = call(app.clone(), "/api/v1/dashboard/overview").await;
        assert!(overview["data"]["searches"].is_object());
        assert_eq!(overview["meta"]["aggregation_version"], "1");
        assert_eq!(overview["meta"]["time_range"]["timezone"], "UTC");
        let series = call(app.clone(), "/api/v1/dashboard/search/timeseries").await;
        assert!(series["data"]["items"].is_array());
        let quality = call(app.clone(), "/api/v1/dashboard/quality/dimensions").await;
        assert!(quality["data"]["freshness"].is_array());
        let csv = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/dashboard/export?format=csv")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(csv.status().as_u16(), 200);
        assert_eq!(overview["data"]["cost"]["is_estimate"], true);
        let bad = call(app, "/api/v1/dashboard/search/timeseries?interval=century").await;
        assert_eq!(bad["error"]["code"], "VALIDATION_FAILED");
    }

    /// Seeds one search task + decision trace directly into the runtime store.
    fn seed_task(
        sqlite: &searchos_storage_sqlite::SqliteStore,
        id: &str,
        ws: &str,
        mode: &str,
        started_at: &str,
        path: &str,
    ) {
        sqlite
            .with_conn(|c| {
                c.execute(
                    "INSERT OR IGNORE INTO workspaces (id, name, config_version, created_at, updated_at)
                     VALUES (?1, ?1, 1, '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
                    rusqlite::params![ws],
                )
                .map_err(|e| searchos_domain::SearchOsError::Storage(e.to_string()))?;
                c.execute(
                    "INSERT INTO search_tasks (id, workspace_id, agent_id, query, normalized_query,
                                         query_fingerprint, profile_id, profile_version, mode, status,
                                         started_at, trace_id)
                     VALUES (?1, ?2, NULL, 'q', 'q', ?3, 'p', '1', ?4, 'completed', ?5, ?1)",
                    rusqlite::params![id, ws, id, mode, started_at],
                )
                .and_then(|_| {
                    c.execute(
                        "INSERT INTO decision_traces (id, task_id, intents_json, profile_version,
                                                     local_candidates, accepted_candidates, freshness_status,
                                                     coverage_status, source_status, selected_path,
                                                     rejection_reasons_json, estimated_cost_micros,
                                                     estimated_currency, actual_cost_micros, decision_at)
                         VALUES (?1, ?2, '[]', '1', 0, 0, 'verified', 'sufficient', 'verified', ?3,
                                 '[]', 0, 'USD', NULL, ?4)",
                        rusqlite::params![format!("dt_{id}"), id, path, started_at],
                    )
                })
                .map(|_| ())
                .map_err(|e| searchos_domain::SearchOsError::Storage(e.to_string()))
            })
            .unwrap();
    }

    #[tokio::test]
    async fn dashboard_filters_are_pushed_into_sql_and_reported() {
        let (st, _d) = state().await;
        seed_task(
            &st.runtime.sqlite,
            "t1",
            "ws_a",
            "local-only",
            "2026-11-01T10:00:00+00:00",
            "local",
        );
        seed_task(
            &st.runtime.sqlite,
            "t2",
            "ws_b",
            "external",
            "2026-11-02T10:00:00+00:00",
            "hybrid",
        );
        let app = router(st);

        // No predicate and day granularity: the precomputed Read Model is used.
        let plain = call(app.clone(), "/api/v1/dashboard/overview").await;
        assert_eq!(plain["meta"]["query_mode"], "precomputed");
        assert_eq!(plain["meta"]["applied_filters"], serde_json::json!([]));
        assert_eq!(plain["data"]["searches"]["total"], 2);

        // A workspace predicate must reach SQL and be declared in meta.
        let filtered = call(
            app.clone(),
            "/api/v1/dashboard/overview?workspace_id=ws_a&interval=day",
        )
        .await;
        assert_eq!(filtered["meta"]["query_mode"], "sql");
        assert_eq!(
            filtered["meta"]["applied_filters"],
            serde_json::json!(["workspace_id"])
        );
        assert_eq!(filtered["meta"]["data_freshness"], "live");
        assert_eq!(filtered["data"]["searches"]["total"], 1);

        // Interval alone forces a live query at the requested granularity:
        // the stored series is day-granular and cannot answer a week bucket.
        let weekly = call(
            app.clone(),
            "/api/v1/dashboard/search/timeseries?interval=week",
        )
        .await;
        assert_eq!(weekly["meta"]["query_mode"], "sql");
        assert_eq!(weekly["meta"]["interval"], "week");

        // A filter the dataset cannot apply is reported, never silently dropped.
        let ignored = call(
            app.clone(),
            "/api/v1/dashboard/crawlers/summary?workspace_id=ws_a&project_id=p1",
        )
        .await;
        assert_eq!(ignored["meta"]["query_mode"], "sql");
        let ignored_filters = ignored["meta"]["ignored_filters"].as_array().unwrap();
        assert!(ignored_filters
            .iter()
            .any(|f| f["filter"] == "workspace_id"));
        assert!(ignored_filters.iter().any(|f| f["filter"] == "project_id"));

        let bad = call(app, "/api/v1/dashboard/search/timeseries?interval=century").await;
        assert_eq!(bad["error"]["code"], "VALIDATION_FAILED");
    }

    #[test]
    fn rate_limiter_is_bounded_and_prunes_expired_buckets() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check("client"));
        assert!(limiter.check("client"));
        assert!(!limiter.check("client"));
        assert!(limiter.check("other-client"));

        // Flood the map with distinct keys; once the cap is hit the map is
        // trimmed back to the current window, so a spoofed-token flood
        // cannot grow gateway memory without bound (P1-2). The exact size
        // is not part of the contract — only the bound is.
        let mut first_at_cap = None;
        for i in 0..(MAX_TRACKED_KEYS * 4) {
            limiter.check(&format!("flood-{i}"));
            let tracked = limiter.tracked();
            if tracked >= MAX_TRACKED_KEYS && first_at_cap.is_none() {
                first_at_cap = Some(tracked);
            }
            assert!(
                tracked <= MAX_TRACKED_KEYS,
                "rate limiter map grew to {tracked} entries at iteration {i}"
            );
        }
        assert!(first_at_cap.is_some(), "flood never reached the cap");
    }
}
