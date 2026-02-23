//! Ethos HTTP REST API
//!
//! Axum-based HTTP server that exposes Ethos search and ingest over HTTP.
//! Runs alongside the Unix socket IPC server on port 8766 (configurable).
//!
//! Architecture: each endpoint has a thin axum handler that delegates to a pure
//! inner function. The inner functions are directly testable without axum dispatch
//! machinery, which improves coverage accuracy under tarpaulin.
//!
//! Endpoints:
//! - GET  /health      — health check with DB status
//! - GET  /version     — server version info
//! - POST /search      — semantic memory search
//! - POST /ingest      — ingest content into memory
//! - POST /consolidate — trigger consolidation cycle

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use ethos_core::ipc::{EthosRequest, EthosResponse};
use ethos_core::EthosConfig;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// Shared state for all HTTP handlers
#[derive(Clone)]
pub struct HttpState {
    pub pool: PgPool,
    pub config: EthosConfig,
}

/// Build the Axum router with all endpoints
pub fn build_router(state: Arc<HttpState>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/version", get(version_handler))
        .route("/search", post(search_handler))
        .route("/ingest", post(ingest_handler))
        .route("/consolidate", post(consolidate_handler))
        .with_state(state)
}

/// Start the HTTP server on the configured address.
/// Gracefully shuts down when the broadcast shutdown signal fires.
pub async fn start_http_server(
    pool: PgPool,
    config: EthosConfig,
    mut shutdown: broadcast::Receiver<()>,
) -> Result<()> {
    let addr = format!("{}:{}", config.http.host, config.http.port);
    let state = Arc::new(HttpState { pool, config });

    let app = build_router(state);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Ethos HTTP API listening on http://{}", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.recv().await;
            tracing::info!("HTTP server shutting down...");
        })
        .await?;

    Ok(())
}

// ============================================================================
// Request / Response DTOs
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub limit: Option<u32>,
    #[serde(default)]
    pub use_spreading: bool,
    /// Minimum score threshold (informational; filtering happens in retrieval)
    pub min_score: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ConsolidateRequest {
    pub session: Option<String>,
    pub reason: Option<String>,
}

/// Standard HTTP error response
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub status: String,
}

impl ErrorResponse {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            error: msg.into(),
            status: "error".to_string(),
        }
    }
}

// ============================================================================
// Inner (directly testable) business logic functions
// ============================================================================

/// Inner health check — queries DB and returns (status_code, json_body).
pub async fn health_inner(
    pool: &PgPool,
    socket_path: &str,
) -> (StatusCode, serde_json::Value) {
    let pg_ver = match ethos_core::db::health_check(pool).await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({
                    "status": "unhealthy",
                    "error": e.to_string(),
                }),
            );
        }
    };

    let pgvector_ver = match ethos_core::db::check_pgvector(pool).await {
        Ok(v) => v,
        Err(e) => format!("unavailable: {}", e),
    };

    (
        StatusCode::OK,
        serde_json::json!({
            "status": "healthy",
            "version": env!("CARGO_PKG_VERSION"),
            "postgresql": pg_ver,
            "pgvector": pgvector_ver,
            "socket": socket_path,
        }),
    )
}

/// Inner version — returns version info (pure, no IO).
pub fn version_inner() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "ethos/1",
    })
}

/// Inner search — validates query and calls the IPC router.
pub async fn search_inner(
    pool: &PgPool,
    config: &EthosConfig,
    req: SearchRequest,
) -> (StatusCode, serde_json::Value) {
    let query = match req.query {
        Some(q) if !q.trim().is_empty() => q,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "error": "query field is required",
                    "status": "error",
                }),
            );
        }
    };

    let start = Instant::now();

    let ipc_request = EthosRequest::Search {
        query: query.clone(),
        limit: req.limit,
        use_spreading: req.use_spreading,
    };

    let response = crate::router::handle_request_with_config(
        ipc_request,
        pool,
        Some(config.clone()),
    )
    .await;

    let took_ms = start.elapsed().as_millis() as u64;

    match response_to_http(response) {
        Ok(mut data) => {
            if let Some(obj) = data.as_object_mut() {
                obj.insert("took_ms".to_string(), serde_json::json!(took_ms));
            }
            (StatusCode::OK, data)
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({
                "error": e,
                "status": "error",
            }),
        ),
    }
}

/// Inner ingest — calls the IPC router with the ingest payload.
pub async fn ingest_inner(
    pool: &PgPool,
    config: &EthosConfig,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let ipc_request = EthosRequest::Ingest { payload };

    let response = crate::router::handle_request_with_config(
        ipc_request,
        pool,
        Some(config.clone()),
    )
    .await;

    match response_to_http(response) {
        Ok(data) => (StatusCode::OK, data),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({
                "error": e,
                "status": "error",
            }),
        ),
    }
}

/// Inner consolidate — calls the IPC router with the consolidation request.
pub async fn consolidate_inner(
    pool: &PgPool,
    config: &EthosConfig,
    req: ConsolidateRequest,
) -> (StatusCode, serde_json::Value) {
    let ipc_request = EthosRequest::Consolidate {
        session: req.session,
        reason: req.reason,
    };

    let response = crate::router::handle_request_with_config(
        ipc_request,
        pool,
        Some(config.clone()),
    )
    .await;

    match response_to_http(response) {
        Ok(data) => (StatusCode::OK, data),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({
                "error": e,
                "status": "error",
            }),
        ),
    }
}

// ============================================================================
// Axum handler wrappers (thin — delegate to inner functions)
// ============================================================================

pub async fn health_handler(
    State(state): State<Arc<HttpState>>,
) -> impl IntoResponse {
    let (status, body) = health_inner(&state.pool, &state.config.service.socket_path).await;
    (status, Json(body))
}

pub async fn version_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(version_inner()))
}

pub async fn search_handler(
    State(state): State<Arc<HttpState>>,
    Json(req): Json<SearchRequest>,
) -> impl IntoResponse {
    let (status, body) = search_inner(&state.pool, &state.config, req).await;
    (status, Json(body))
}

pub async fn ingest_handler(
    State(state): State<Arc<HttpState>>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let (status, body) = ingest_inner(&state.pool, &state.config, payload).await;
    (status, Json(body))
}

pub async fn consolidate_handler(
    State(state): State<Arc<HttpState>>,
    Json(req): Json<ConsolidateRequest>,
) -> impl IntoResponse {
    let (status, body) = consolidate_inner(&state.pool, &state.config, req).await;
    (status, Json(body))
}

// ============================================================================
// Helpers
// ============================================================================

/// Convert an IPC `EthosResponse` into an HTTP body value, or an error string.
pub fn response_to_http(response: EthosResponse) -> std::result::Result<serde_json::Value, String> {
    if response.status == "ok" {
        Ok(response.data.unwrap_or(serde_json::json!({})))
    } else {
        Err(response.error.unwrap_or_else(|| "unknown error".to_string()))
    }
}

// ============================================================================
// Unit Tests — call inner functions directly for reliable tarpaulin coverage
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const DATABASE_URL: &str = "postgresql://ethos:ethos_dev@localhost:5432/ethos";

    /// Helper to get pool + config — returns None if DB or config unavailable
    async fn make_state() -> Option<(PgPool, EthosConfig)> {
        let pool = PgPool::connect(DATABASE_URL).await.ok()?;
        let config = EthosConfig::load("ethos.toml").ok()?;
        Some((pool, config))
    }

    // ========================================================================
    // TEST 1: version_inner is pure and returns correct fields
    // ========================================================================
    #[test]
    fn test_version_inner_pure() {
        let v = version_inner();
        assert!(v["version"].is_string(), "version must be string");
        assert_eq!(v["protocol"], "ethos/1", "protocol must be ethos/1");
    }

    // ========================================================================
    // TEST 2: response_to_http — ok response extracts data
    // ========================================================================
    #[test]
    fn test_response_to_http_ok() {
        let resp = EthosResponse::ok(serde_json::json!({"results": [], "count": 0}));
        let result = response_to_http(resp);
        assert!(result.is_ok());
        let data = result.unwrap();
        assert_eq!(data["count"], 0);
    }

    // ========================================================================
    // TEST 3: response_to_http — error response returns Err
    // ========================================================================
    #[test]
    fn test_response_to_http_error() {
        let resp = EthosResponse::err("something went wrong");
        let result = response_to_http(resp);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "something went wrong");
    }

    // ========================================================================
    // TEST 4: response_to_http — ok with no data returns empty object
    // ========================================================================
    #[test]
    fn test_response_to_http_ok_no_data() {
        let mut resp = EthosResponse::ok(serde_json::json!({}));
        resp.data = None;
        let result = response_to_http(resp).unwrap();
        assert!(result.is_object());
    }

    // ========================================================================
    // TEST 5: response_to_http — error with no message returns fallback
    // ========================================================================
    #[test]
    fn test_response_to_http_error_no_message() {
        let mut resp = EthosResponse::err("x");
        resp.error = None;
        resp.status = "error".to_string();
        let result = response_to_http(resp);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "unknown error");
    }

    // ========================================================================
    // TEST 6: health_inner — returns 200 with expected fields (DB available)
    // ========================================================================
    #[tokio::test]
    async fn test_health_inner_ok() {
        let (pool, _config) = match make_state().await {
            Some(s) => s,
            None => {
                eprintln!("Skipping test_health_inner_ok: DB unavailable");
                return;
            }
        };

        let (status, body) = health_inner(&pool, "/tmp/ethos.sock").await;
        assert_eq!(status, StatusCode::OK, "Health should return 200");
        assert_eq!(body["status"], "healthy");
        assert!(body["postgresql"].is_string());
        assert_eq!(body["socket"], "/tmp/ethos.sock");
    }

    // ========================================================================
    // TEST 7: search_inner — empty query returns 400 BAD_REQUEST
    // ========================================================================
    #[tokio::test]
    async fn test_search_inner_empty_query() {
        let (pool, config) = match make_state().await {
            Some(s) => s,
            None => {
                eprintln!("Skipping test_search_inner_empty_query: DB unavailable");
                return;
            }
        };

        let req = SearchRequest {
            query: Some("".to_string()),
            limit: None,
            use_spreading: false,
            min_score: None,
        };

        let (status, body) = search_inner(&pool, &config, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["status"], "error");
        assert!(body["error"].is_string());
    }

    // ========================================================================
    // TEST 8: search_inner — None query returns 400 BAD_REQUEST
    // ========================================================================
    #[tokio::test]
    async fn test_search_inner_no_query() {
        let (pool, config) = match make_state().await {
            Some(s) => s,
            None => {
                eprintln!("Skipping test_search_inner_no_query: DB unavailable");
                return;
            }
        };

        let req = SearchRequest {
            query: None,
            limit: Some(5),
            use_spreading: false,
            min_score: None,
        };

        let (status, body) = search_inner(&pool, &config, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["status"], "error");
    }

    // ========================================================================
    // TEST 9: search_inner — whitespace-only query returns 400
    // ========================================================================
    #[tokio::test]
    async fn test_search_inner_whitespace_query() {
        let (pool, config) = match make_state().await {
            Some(s) => s,
            None => {
                eprintln!("Skipping test_search_inner_whitespace_query: DB unavailable");
                return;
            }
        };

        let req = SearchRequest {
            query: Some("   ".to_string()),
            limit: None,
            use_spreading: false,
            min_score: None,
        };

        let (status, body) = search_inner(&pool, &config, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["status"], "error");
    }

    // ========================================================================
    // TEST 10: search_inner — valid query returns 200 with results array
    // ========================================================================
    #[tokio::test]
    async fn test_search_inner_valid_query() {
        let (pool, config) = match make_state().await {
            Some(s) => s,
            None => {
                eprintln!("Skipping test_search_inner_valid_query: DB unavailable");
                return;
            }
        };

        let req = SearchRequest {
            query: Some("semantic memory search".to_string()),
            limit: Some(3),
            use_spreading: false,
            min_score: None,
        };

        let (status, body) = search_inner(&pool, &config, req).await;
        // 200 (results or empty) or 500 (embedding unavailable)
        assert!(
            status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
            "Unexpected status: {}",
            status
        );

        if status == StatusCode::OK {
            assert!(body["results"].is_array(), "Should have results array");
            assert!(body["took_ms"].is_number(), "Should have took_ms");
        }
    }

    // ========================================================================
    // TEST 11: ingest_inner — missing content field returns error response
    // ========================================================================
    #[tokio::test]
    async fn test_ingest_inner_missing_content() {
        let (pool, config) = match make_state().await {
            Some(s) => s,
            None => {
                eprintln!("Skipping test_ingest_inner_missing_content: DB unavailable");
                return;
            }
        };

        let payload = serde_json::json!({
            "source": "user"
            // no "content" field — should cause an error
        });

        let (status, body) = ingest_inner(&pool, &config, payload).await;
        // Should return 500 with error info
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body["error"].is_string(), "Should have error message");
    }

    // ========================================================================
    // TEST 12: ingest_inner — valid payload stores content
    // ========================================================================
    #[tokio::test]
    async fn test_ingest_inner_valid_payload() {
        let (pool, config) = match make_state().await {
            Some(s) => s,
            None => {
                eprintln!("Skipping test_ingest_inner_valid_payload: DB unavailable");
                return;
            }
        };

        let session_id = "http-inner-test-session-011";

        // Clean up before test
        sqlx::query("DELETE FROM session_events WHERE session_id = $1")
            .bind(session_id)
            .execute(&pool)
            .await
            .ok();

        let payload = serde_json::json!({
            "content": "HTTP inner function ingest test",
            "source": "user",
            "metadata": {
                "session_id": session_id,
                "agent_id": "forge-test"
            }
        });

        let (status, body) = ingest_inner(&pool, &config, payload).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "Ingest should return 200: {:?}",
            body
        );
        assert_eq!(body["queued"], true);
        assert!(body["id"].is_string());

        // Cleanup
        sqlx::query("DELETE FROM session_events WHERE session_id = $1")
            .bind(session_id)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST 13: consolidate_inner — runs consolidation cycle
    // ========================================================================
    #[tokio::test]
    async fn test_consolidate_inner_runs() {
        let (pool, config) = match make_state().await {
            Some(s) => s,
            None => {
                eprintln!("Skipping test_consolidate_inner_runs: DB unavailable");
                return;
            }
        };

        let req = ConsolidateRequest {
            session: None,
            reason: Some("test trigger".to_string()),
        };

        let (status, body) = consolidate_inner(&pool, &config, req).await;
        assert!(
            status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
            "Unexpected status: {}",
            status
        );

        if status == StatusCode::OK {
            assert!(body["episodes_scanned"].is_number(), "Should have episodes_scanned");
        }
    }

    // ========================================================================
    // TEST 14: health_inner returns version matching CARGO_PKG_VERSION
    // ========================================================================
    #[tokio::test]
    async fn test_health_inner_version_matches_cargo() {
        let (pool, _config) = match make_state().await {
            Some(s) => s,
            None => {
                eprintln!("Skipping test_health_inner_version_matches_cargo: DB unavailable");
                return;
            }
        };

        let (status, body) = health_inner(&pool, "/tmp/test.sock").await;
        if status == StatusCode::OK {
            let version = body["version"].as_str().unwrap_or("");
            assert!(!version.is_empty(), "Version should not be empty");
            assert_eq!(version, env!("CARGO_PKG_VERSION"));
        }
    }
}
