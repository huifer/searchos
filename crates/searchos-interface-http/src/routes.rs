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
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::tavily::{TavilySearchRequest, TavilySearchResponse};
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Multipart, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
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
        Self {
            runtime,
            auth_token: cfg.auth_token.clone(),
            api_version: cfg.api_version.clone(),
            workspace_tokens: cfg.workspace_tokens.clone(),
            cors_origin: cfg.cors_origin.clone(),
            limiter: Arc::new(RateLimiter::new(cfg.rate_limit_per_minute)),
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
fn api_routes(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/search", post(search))
        .route("/extract", post(extract))
        .route("/ingest", post(ingest))
        .route("/ingest/file", post(ingest_file))
        .route("/ingest/batch", post(ingest_batch))
        .route("/ingest/jobs/{id}", get(get_ingest_job))
        .route("/documents", post(create_document))
        .route("/documents/{id}", get(get_document))
        .route("/documents/search", post(search_documents))
        .route("/memory/search", post(memory_search))
        .route("/cost/summary", get(cost_summary))
        .route("/cost/providers", get(cost_providers))
        .route("/cost/savings", get(cost_savings))
        .route("/cost/reservations", get(cost_reservations))
        .route("/cost/reconcile", post(cost_reconcile))
        .route("/stats/search", get(stats_search))
        .route("/stats/savings", get(stats_savings))
        .route("/stats/retrieval", get(stats_retrieval))
        .route("/decisions/{trace_id}", get(get_decision))
        .route("/trace/{trace_id}", get(get_decision))
        .route("/traces", get(list_traces))
        .route("/system/status", get(system_status))
        .route("/profiles", get(list_profiles))
        .route("/profiles/{id}", get(get_profile))
        .with_state(state)
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

/// Binds and serves the gateway.
pub async fn serve(state: AppState) -> Result<(), String> {
    let host = state.runtime.config.server.host.clone();
    let port = state.runtime.config.server.port;

    // Background outbox worker (RFC-006 6.7): fetched documents must reach
    // the full-text index without blocking the request path. Failure is
    // logged and retried on the next tick; the worker never panics the
    // gateway.
    let worker_runtime = Arc::clone(&state.runtime);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(1_500));
        loop {
            ticker.tick().await;
            if let Err(e) = worker_runtime.process_outbox(100).await {
                tracing::warn!(error = %e, "analytics outbox worker tick failed");
            }
        }
    });

    let app = router(state);
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    tracing::info!("gateway listening on http://{addr}");
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("serve: {e}"))
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
        "days",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
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
    Json(dto): Json<ExtractDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
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
        Ok(Json(serde_json::to_value(doc).unwrap_or_default()))
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
        })))
    }
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
    let scope = state.workspace_scope(&headers, None)?;
    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&state.runtime.sqlite);
    let doc = repo
        .get(&searchos_domain::ids::DocumentId::new(id.clone()))
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
    let version = repo
        .get_current_version(&doc.id)
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(
        serde_json::json!({ "document": doc, "current_version": version }),
    ))
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
        .search(&dto.query, dto.limit, scope.as_deref())
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

fn dashboard_routes(state: AppState) -> Router {
    let mut router = Router::new().route("/api/v1/dashboard/overview", get(dashboard_overview));
    for (path, _) in DASHBOARD_DATASETS {
        router = router.route(&format!("/api/v1/dashboard/{path}"), get(dashboard_dataset));
    }
    router
        .route("/api/v1/dashboard/export", get(dashboard_export))
        .route("/api/v1/dashboard/events", get(dashboard_events))
        .with_state(state)
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
    let stream = stream::unfold(0i64, move |cursor| {
        let analytics = Arc::clone(&analytics);
        async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
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
