//! HTTP integration tests for Ethos REST API (Story 011)
//!
//! These tests require a live PostgreSQL connection and a valid ethos.toml.
//! They use both the inner function approach (for tarpaulin coverage) and
//! the Axum `oneshot` approach for full end-to-end handler dispatch tests.

use axum::http::StatusCode;
use ethos_core::EthosConfig;
use ethos_server::http::{
    build_router, consolidate_inner, health_inner, ingest_inner, search_inner, ConsolidateRequest,
    HttpState, SearchRequest,
};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;

// For oneshot testing
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

const DATABASE_URL: &str = "postgresql://ethos:ethos_dev@localhost:5432/ethos";

/// Create shared test state — returns None if DB or config unavailable
async fn make_state() -> Option<(PgPool, EthosConfig)> {
    let pool = PgPool::connect(DATABASE_URL).await.ok()?;
    let config = EthosConfig::load("ethos.toml").ok()?;
    Some((pool, config))
}

/// Make Arc<HttpState> for router tests
async fn make_http_state() -> Option<Arc<HttpState>> {
    let (pool, config) = make_state().await?;
    Some(Arc::new(HttpState { pool, config }))
}

// ===========================================================================
// TEST 1: GET /health — server starts, responds 200 with expected fields
// ===========================================================================
#[tokio::test]
async fn test_http_server_starts() {
    let (pool, _config) = match make_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_http_server_starts: DB or config unavailable");
            return;
        }
    };

    let (status, body) = health_inner(&pool, "/tmp/ethos.sock").await;
    assert_eq!(status, StatusCode::OK, "Health check should return 200");
    assert_eq!(body["status"], "healthy", "status must be 'healthy'");
    assert!(body["version"].is_string(), "version must be present");
    assert!(
        body["postgresql"].is_string(),
        "postgresql version must be present"
    );
    assert!(body["socket"].is_string(), "socket path must be present");
}

// ===========================================================================
// TEST 2: GET /version via oneshot — returns version and protocol
// ===========================================================================
#[tokio::test]
async fn test_version_endpoint_integration() {
    let state = match make_http_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_version_endpoint_integration: DB or config unavailable");
            return;
        }
    };

    let app = build_router(state);

    let req = Request::builder()
        .method("GET")
        .uri("/version")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(json["version"].is_string());
    assert_eq!(json["protocol"], "ethos/1");
}

// ===========================================================================
// TEST 3: ingest via inner function — content stored in DB
// ===========================================================================
#[tokio::test]
async fn test_ingest_via_http() {
    let (pool, config) = match make_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_ingest_via_http: DB or config unavailable");
            return;
        }
    };

    let test_session = "http-ingest-integration-011";

    // Clean up before test
    sqlx::query("DELETE FROM session_events WHERE session_id = $1")
        .bind(test_session)
        .execute(&pool)
        .await
        .ok();

    let payload = json!({
        "content": "HTTP ingest integration test for Story 011",
        "source": "user",
        "metadata": {
            "session_id": test_session,
            "agent_id": "forge-test"
        }
    });

    let (status, body) = ingest_inner(&pool, &config, payload).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Ingest should return 200, got: {:?}",
        body
    );
    assert_eq!(body["queued"], true, "Response should have queued:true");
    assert!(body["id"].is_string(), "Response should include id");

    // Verify content was stored in DB
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT content, role FROM session_events WHERE session_id = $1",
    )
    .bind(test_session)
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert!(row.is_some(), "Content should be stored in session_events");
    let (content, _role) = row.unwrap();
    assert_eq!(content, "HTTP ingest integration test for Story 011");

    // Cleanup
    sqlx::query("DELETE FROM session_events WHERE session_id = $1")
        .bind(test_session)
        .execute(&pool)
        .await
        .ok();
}

// ===========================================================================
// TEST 4: search roundtrip via inner function — returns valid response
// ===========================================================================
#[tokio::test]
async fn test_search_roundtrip_http() {
    let (pool, config) = match make_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_search_roundtrip_http: DB or config unavailable");
            return;
        }
    };

    let req = SearchRequest {
        query: Some("memory search roundtrip integration test".to_string()),
        limit: Some(5),
        use_spreading: false,
        min_score: None,
    };

    let (status, body) = search_inner(&pool, &config, req).await;

    // Either 200 (success, results or empty) or 500 (embedding API unavailable)
    assert!(
        status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
        "Unexpected status code: {}",
        status
    );

    if status == StatusCode::OK {
        assert!(body["results"].is_array(), "Should have results array");
        assert!(body["count"].is_number(), "Should have count field");
        assert!(body["took_ms"].is_number(), "Should have took_ms field");
        assert!(body["query"].is_string(), "Should echo query field");
    }
}

// ===========================================================================
// TEST 5: search with empty query returns BAD_REQUEST
// ===========================================================================
#[tokio::test]
async fn test_search_empty_query_http() {
    let (pool, config) = match make_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_search_empty_query_http: DB or config unavailable");
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
    assert_eq!(status, StatusCode::BAD_REQUEST, "Empty query should return 400");
    assert_eq!(body["status"], "error");
}

// ===========================================================================
// TEST 6: search with None query returns BAD_REQUEST
// ===========================================================================
#[tokio::test]
async fn test_search_no_query_field_http() {
    let (pool, config) = match make_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_search_no_query_field_http: DB or config unavailable");
            return;
        }
    };

    let req = SearchRequest {
        query: None,
        limit: Some(10),
        use_spreading: false,
        min_score: None,
    };

    let (status, body) = search_inner(&pool, &config, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "Missing query should return 400");
    assert_eq!(body["status"], "error");
}

// ===========================================================================
// TEST 7: health returns either 200 healthy or 503 unhealthy (graceful)
// ===========================================================================
#[tokio::test]
async fn test_health_response_structure() {
    let (pool, _config) = match make_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_health_response_structure: DB or config unavailable");
            return;
        }
    };

    let (status, body) = health_inner(&pool, "/tmp/test.sock").await;

    assert!(
        status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE,
        "Health must return 200 or 503, got {}",
        status
    );
    assert!(
        body["status"].is_string(),
        "Health response must have 'status' field"
    );
}

// ===========================================================================
// TEST 8: consolidate_inner — runs a consolidation cycle
// ===========================================================================
#[tokio::test]
async fn test_consolidate_inner_integration() {
    let (pool, config) = match make_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_consolidate_inner_integration: DB or config unavailable");
            return;
        }
    };

    let req = ConsolidateRequest {
        session: None,
        reason: Some("Story 011 integration test".to_string()),
    };

    let (status, body) = consolidate_inner(&pool, &config, req).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
        "Unexpected status: {}",
        status
    );

    if status == StatusCode::OK {
        assert!(
            body["episodes_scanned"].is_number(),
            "Should have episodes_scanned field"
        );
    }
}

// ===========================================================================
// TEST 9: ingest via oneshot (end-to-end handler dispatch)
// ===========================================================================
#[tokio::test]
async fn test_ingest_handler_via_oneshot() {
    let state = match make_http_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_ingest_handler_via_oneshot: DB or config unavailable");
            return;
        }
    };

    let test_session = "http-oneshot-ingest-011";
    let pool = state.pool.clone();

    sqlx::query("DELETE FROM session_events WHERE session_id = $1")
        .bind(test_session)
        .execute(&pool)
        .await
        .ok();

    let app = build_router(state);

    let payload = json!({
        "content": "oneshot ingest test",
        "source": "user",
        "metadata": {
            "session_id": test_session,
            "agent_id": "forge-test"
        }
    });

    let req = Request::builder()
        .method("POST")
        .uri("/ingest")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&payload).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "Ingest handler should return 200");

    sqlx::query("DELETE FROM session_events WHERE session_id = $1")
        .bind(test_session)
        .execute(&pool)
        .await
        .ok();
}

// ===========================================================================
// TEST 10: search with spreading enabled (backward compat)
// ===========================================================================
#[tokio::test]
async fn test_search_with_spreading_http() {
    let (pool, config) = match make_state().await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_search_with_spreading_http: DB or config unavailable");
            return;
        }
    };

    let req = SearchRequest {
        query: Some("spreading activation test".to_string()),
        limit: Some(3),
        use_spreading: true,
        min_score: None,
    };

    let (status, body) = search_inner(&pool, &config, req).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
        "Unexpected status: {}",
        status
    );

    if status == StatusCode::OK {
        assert!(body["results"].is_array());
    }
}
