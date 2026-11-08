//! Axum routes (RFC-012 12.4, RFC-021 21.2 API surface).
//!
//! ```text
//! POST /search          POST /extract          POST /documents
//! GET  /documents/:id   POST /memory/search    GET  /cost/summary
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
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream;
use searchos_application::analytics::AnalyticsQuery;
use searchos_core::SearchOsRuntime;
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::memory::MemoryNamespace;
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::SearchOsError;
use std::convert::Infallible;
use std::time::Duration;

/// Simple in-process fixed-window rate limiter (RFC-013 13.3).
/// A Gateway cluster would use a shared store; Local Mode stays dependency-free.
#[derive(Debug)]
pub struct RateLimiter {
    per_minute: u32,
    windows: Mutex<HashMap<String, (u64, u32)>>,
}

impl RateLimiter {
    /// New limiter; `0` disables limiting.
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Returns whether this client key may proceed.
    pub fn check(&self, key: &str) -> bool {
        if self.per_minute == 0 {
            return true;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let window = now / 60;
        let mut map = self.windows.lock().unwrap();
        let entry = map.entry(key.to_string()).or_insert((window, 0));
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
    pub limiter: Arc<RateLimiter>,
}

impl AppState {
    /// State over an assembled runtime.
    pub fn new(runtime: Arc<SearchOsRuntime>) -> Self {
        let cfg = &runtime.config.server;
        Self {
            runtime: runtime.clone(),
            auth_token: cfg.auth_token.clone(),
            api_version: cfg.api_version.clone(),
            workspace_tokens: cfg.workspace_tokens.clone(),
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

    fn authorize(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        if !self.limiter.check(&self.client_key(headers)) {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "gateway rate limit exceeded",
            ));
        }
        if self.auth_token.is_empty() {
            return Ok(());
        }
        let provided = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if provided == self.auth_token {
            Ok(())
        } else {
            Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "invalid bearer token",
            ))
        }
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
    let cors_origin = state.runtime.config.server.cors_origin.clone();
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
        .layer(from_fn(move |req: Request, next: Next| {
            let version = versioned.clone();
            let origin = cors_origin.clone();
            async move {
                let allowed_origin = if origin.is_empty() {
                    None
                } else {
                    req.headers()
                        .get("origin")
                        .and_then(|v| v.to_str().ok())
                        .filter(|o| origin == "*" || *o == origin)
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
    let (tantivy, sqlite) = state.runtime.index_consistency().await.unwrap_or((0, 0));
    Json(serde_json::json!({
        "status": "ok",
        "version": searchos_core::VERSION,
        "index": { "tantivy_chunks": tantivy, "sqlite_chunks": sqlite, "consistent": tantivy == sqlite },
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
}

async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(dto): Json<SearchDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
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
        Some(m) => match m {
            "local_only" => SearchMode::LocalOnly,
            "local_first" => SearchMode::LocalFirst,
            "external_first" => SearchMode::ExternalFirst,
            "external_only" => SearchMode::ExternalOnly,
            "hybrid" => SearchMode::Hybrid,
            other => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "VALIDATION_FAILED",
                    format!("unknown mode {other}"),
                ))
            }
        },
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
    let resp = state
        .runtime
        .application
        .search_service
        .search(req)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(Json(serde_json::to_value(resp).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            e.to_string(),
        )
    })?))
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
    let summary = state
        .runtime
        .application
        .cost_service
        .summary(&Default::default())
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
    let providers = state
        .runtime
        .application
        .cost_service
        .summary(&Default::default())
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    let mut list = Vec::new();
    for id in state.runtime.provider_registry.ids() {
        let health = state.runtime.provider_registry.health(&id);
        list.push(serde_json::json!({
            "provider_id": id,
            "health": health.map(|(s, _rl)| format!("{s:?}")).unwrap_or_else(|| "unknown".into()),
            "rate_limited": health.map(|(_, rl)| rl).unwrap_or(false),
            "cost_micros": providers
                .by_provider
                .iter()
                .find(|(p, _)| p == &id)
                .map(|(_, m)| m.amount_micros)
                .unwrap_or(0),
        }));
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
    let repo = searchos_storage_sqlite::repos::DecisionRepo::new(&state.runtime.sqlite);
    let trace = repo
        .get_trace(&trace_id)
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
    let traces = searchos_storage_sqlite::repos::DecisionRepo::new(&state.runtime.sqlite)
        .list_traces(q.limit.min(100))
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
            "tantivy_chunks": tantivy_chunks,
            "sqlite_chunks": sqlite_chunks,
            "consistent": tantivy_chunks == sqlite_chunks,
        },
        "providers": state.runtime.provider_registry.ids(),
        "crawlers": state.runtime.crawlers.list().len(),
        "extensions": state.runtime.extensions.list().len(),
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
    state.authorize_workspace(&headers, dto.workspace_id.as_deref().unwrap_or("default"))?;
    let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&state.runtime.sqlite);
    let docs = repo.list(500).map_err(|e| ApiError::from_domain(&e))?;
    let q = dto.query.to_lowercase();
    let matches: Vec<_> = docs
        .into_iter()
        .filter(|d| d.canonical_url.to_lowercase().contains(&q))
        .take(dto.limit.min(100) as usize)
        .collect();
    Ok(Json(serde_json::json!({ "documents": matches })))
}

/// Savings / CPQPA report against an equivalent-quality external-only
/// baseline (RFC-006 6.10, Appendix C).
async fn savings_payload(state: &AppState) -> Result<serde_json::Value, ApiError> {
    let baseline = state.runtime.config.budgets.baseline_per_query_micros;
    let report = state
        .runtime
        .application
        .cost_service
        .savings(baseline)
        .await
        .map_err(|e| ApiError::from_domain(&e))?;
    Ok(serde_json::to_value(report).unwrap_or_default())
}

/// `GET /stats/savings` and `GET /cost/savings`.
async fn stats_savings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    Ok(Json(savings_payload(&state).await?))
}

async fn cost_savings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    Ok(Json(savings_payload(&state).await?))
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
    let rows = state
        .runtime
        .application
        .cost_service
        .reservations(50)
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
    fn to_query(&self) -> AnalyticsQuery {
        AnalyticsQuery {
            workspace_id: self.workspace_id.clone(),
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

fn dashboard_routes(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/dashboard/overview", get(dashboard_overview))
        .route("/api/v1/dashboard/search/summary", get(dashboard_dataset))
        .route(
            "/api/v1/dashboard/search/timeseries",
            get(dashboard_dataset),
        )
        .route(
            "/api/v1/dashboard/search/strategies",
            get(dashboard_dataset),
        )
        .route("/api/v1/dashboard/search/requests", get(dashboard_dataset))
        .route("/api/v1/dashboard/cost/summary", get(dashboard_dataset))
        .route("/api/v1/dashboard/cost/breakdown", get(dashboard_dataset))
        .route("/api/v1/dashboard/cost/savings", get(dashboard_dataset))
        .route(
            "/api/v1/dashboard/knowledge/summary",
            get(dashboard_dataset),
        )
        .route(
            "/api/v1/dashboard/knowledge/documents",
            get(dashboard_dataset),
        )
        .route(
            "/api/v1/dashboard/knowledge/freshness",
            get(dashboard_dataset),
        )
        .route("/api/v1/dashboard/quality/summary", get(dashboard_dataset))
        .route(
            "/api/v1/dashboard/quality/dimensions",
            get(dashboard_dataset),
        )
        .route("/api/v1/dashboard/quality/failures", get(dashboard_dataset))
        .route("/api/v1/dashboard/providers", get(dashboard_dataset))
        .route("/api/v1/dashboard/crawlers/summary", get(dashboard_dataset))
        .route(
            "/api/v1/dashboard/infrastructure/summary",
            get(dashboard_dataset),
        )
        .route("/api/v1/dashboard/export", get(dashboard_export))
        .route("/api/v1/dashboard/events", get(dashboard_events))
        .with_state(state)
}

/// Maps an explicit Dashboard path to a Read Model key.
fn read_model_for_path(path: &str) -> Option<&'static str> {
    let suffix = path.split("/api/v1/dashboard/").nth(1)?;
    Some(match suffix {
        "search/summary" | "search/requests" => "search_requests",
        "search/timeseries" => "search_timeseries",
        "search/strategies" => "search_strategies",
        "cost/summary" | "cost/breakdown" | "cost/savings" => "cost_breakdown",
        "knowledge/summary" | "knowledge/documents" => "knowledge_growth",
        "knowledge/freshness" => "knowledge_freshness",
        "quality/summary" | "quality/dimensions" | "quality/failures" => "quality_dimensions",
        "providers" => "providers",
        "crawlers/summary" => "crawlers_summary",
        "infrastructure/summary" => "infrastructure_summary",
        _ => return None,
    })
}

async fn dashboard_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(dto): Query<DashboardQueryDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.authorize(&headers)?;
    state.authorize_workspace(&headers, dto.workspace_id.as_deref().unwrap_or("default"))?;
    let query = dto.to_query();
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
    state.authorize_workspace(&headers, dto.workspace_id.as_deref().unwrap_or("default"))?;
    let model = read_model_for_path(req.uri().path()).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "unknown dashboard dataset",
        )
    })?;
    let query = dto.to_query();
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
    let query = dto.to_query();
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

/// One-way realtime notification channel (RFC-028 6.2). SSE is a hint to
/// refetch the Read Model, never the source of historical truth.
async fn dashboard_events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::stream::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    state.authorize(&headers)?;
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
            let data = serde_json::to_string(&events).unwrap_or_else(|_| "[]".into());
            Some((
                Ok(SseEvent::default().event("analytics.events").data(data)),
                next,
            ))
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = searchos_core::SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        (AppState::new(rt), dir)
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
    async fn stats_and_cost_endpoints() {
        let (st, _d) = state().await;
        let app = router(st);
        let stats = call(app.clone(), "/stats/search").await;
        assert_eq!(stats["search_requests"], 0);
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
    async fn dashboard_api_serves_read_model_envelopes() {
        use tower::ServiceExt;
        let (st, _d) = state().await;
        let app = router(st);
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
    fn rate_limiter_is_bounded() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check("client"));
        assert!(limiter.check("client"));
        assert!(!limiter.check("client"));
        assert!(limiter.check("other-client"));
    }
}
