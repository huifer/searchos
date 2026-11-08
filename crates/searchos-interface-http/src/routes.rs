//! Axum routes (RFC-012 12.4 API surface).
//!
//! ```text
//! POST /search          POST /extract          POST /documents
//! GET  /documents/:id   POST /memory/search    GET  /cost/summary
//! GET  /cost/providers  GET  /stats/search     GET  /decisions/:trace_id
//! GET  /health
//! ```
//!
//! Handlers only authenticate, validate DTOs, call the application, and map
//! responses (RFC-012 12.3).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use searchos_core::SearchOsRuntime;
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::memory::MemoryNamespace;
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::SearchOsError;

/// Shared gateway state.
#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<SearchOsRuntime>,
    /// Bearer token; empty disables auth (local dev).
    pub auth_token: String,
}

impl AppState {
    /// State over an assembled runtime.
    pub fn new(runtime: Arc<SearchOsRuntime>) -> Self {
        let token = runtime.config.server.auth_token.clone();
        Self {
            runtime,
            auth_token: token,
        }
    }

    fn authorize(&self, headers: &HeaderMap) -> Result<(), ApiError> {
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

/// Builds the router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/search", post(search))
        .route("/extract", post(extract))
        .route("/documents", post(create_document))
        .route("/documents/{id}", get(get_document))
        .route("/memory/search", post(memory_search))
        .route("/cost/summary", get(cost_summary))
        .route("/cost/providers", get(cost_providers))
        .route("/stats/search", get(stats_search))
        .route("/decisions/{trace_id}", get(get_decision))
        .with_state(state)
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
}
