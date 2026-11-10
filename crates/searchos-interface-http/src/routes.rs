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
use axum::routing::{delete, get, post, MethodRouter};
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
        RouteSpec::new("POST", "/extract", post(extract)),
        RouteSpec::new("POST", "/ingest", post(ingest)),
        RouteSpec::new("POST", "/ingest/file", post(ingest_file)),
        RouteSpec::new("POST", "/ingest/batch", post(ingest_batch)),
        RouteSpec::new("GET", "/ingest/jobs/{id}", get(get_ingest_job)),
        RouteSpec::new("POST", "/documents", post(create_document)),
        RouteSpec::new("GET", "/documents", get(list_documents)),
        RouteSpec::new("GET", "/documents/{id}", get(get_document)),
        RouteSpec::new("DELETE", "/documents/{id}", delete(delete_document)),
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
        RouteSpec::new("POST", "/cost/reconcile", post(cost_reconcile)),
        RouteSpec::new("GET", "/stats/search", get(stats_search)),
        RouteSpec::new("GET", "/stats/savings", get(stats_savings)),
        RouteSpec::new("GET", "/stats/retrieval", get(stats_retrieval)),
        RouteSpec::new("GET", "/decisions/{trace_id}", get(get_decision)),
        RouteSpec::new("GET", "/trace/{trace_id}", get(get_decision)),
        RouteSpec::new("GET", "/traces", get(list_traces)),
        RouteSpec::new("GET", "/system/status", get(system_status)),
        RouteSpec::new("GET", "/profiles", get(list_profiles)),
        RouteSpec::new("GET", "/profiles/{id}", get(get_profile)),
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
pub(crate) async fn run_outbox_worker(
    runtime: Arc<SearchOsRuntime>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_millis(1_500));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = runtime.process_outbox(100).await {
                    tracing::warn!(error = %e, "index outbox worker tick failed");
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
    #[serde(default)]
    pub max_total_cost: Option<f64>,
    /// Client idempotency key (RFC-014 14.7).
    #[serde(default)]
    pub request_id: Option<String>,
    /// Optional output cap, applied after the quality gate.
    #[serde(default)]
    pub max_results: Option<u32>,
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
    if let Some(cost) = dto.max_total_cost {
        req.budget.max_total_cost_micros = (cost * 1_000_000.0).round() as i64;
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
    })))
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

/// `POST /cost/reconcile` — provider billing import (RFC-006 6.7).
#[derive(Debug, serde::Deserialize)]
pub struct ReconcileDto {
    pub cost_event_id: String,
    /// Actual billed amount in major units (e.g. 0.005 USD).
    pub actual_cost: f64,
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
    if dto.actual_cost < 0.0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "actual_cost must be >= 0",
        ));
    }
    let micros = (dto.actual_cost * 1_000_000.0).round() as i64;
    let currency = dto.currency.unwrap_or_else(|| "USD".into());
    let source = dto.source.unwrap_or_else(|| "provider_billing".into());
    state
        .runtime
        .application
        .cost_service
        .reconcile(&dto.cost_event_id, micros, &source, &currency)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::json!({
        "reconciled": dto.cost_event_id,
        "actual_cost_micros": micros,
        "source": source,
        "currency": currency,
    })))
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
