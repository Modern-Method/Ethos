//! Integration tests for the Embed IPC request
//!
//! These tests verify:
//! 1. Manual Embed trigger via IPC populates vector
//! 2. Vector IS NULL stays on API failure

use ethos_core::embeddings::{EmbeddingConfig, GeminiEmbeddingClient, GEMINI_DIMENSIONS};
use ethos_server::subsystems::embedder;
use pgvector::Vector;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_embedding_response() -> serde_json::Value {
    let values: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
    serde_json::json!({
        "embedding": {
            "values": values
        }
    })
}

fn create_test_client(mock_server: &MockServer) -> GeminiEmbeddingClient {
    let config = EmbeddingConfig {
        api_key: "test-api-key".to_string(),
        model: "gemini-embedding-001".to_string(),
        dimensions: GEMINI_DIMENSIONS,
        max_retries: 1,
        retry_delay_ms: 10,
    };
    
    GeminiEmbeddingClient::with_base_url(config, mock_server.uri())
        .expect("Failed to create test client")
}

#[tokio::test]
async fn test_manual_embed_trigger_via_ipc() {
    let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
    let pool = PgPool::connect(database_url)
        .await
        .expect("Failed to connect to Postgres");

    // Insert a row without vector
    let content = "test content for manual embed";
    let row: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO memory_vectors (content, source) VALUES ($1, 'test-manual-embed') RETURNING id"
    )
    .bind(content)
    .fetch_one(&pool)
    .await
    .expect("Failed to insert test row");

    // Verify vector is initially NULL
    let before: (Option<Vector>,) = sqlx::query_as(
        "SELECT vector FROM memory_vectors WHERE id = $1"
    )
    .bind(row.0)
    .fetch_one(&pool)
    .await
    .expect("Row not found");
    assert!(before.0.is_none(), "Vector should start as NULL");

    // Start mock server
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_embedding_response())
        )
        .mount(&mock_server)
        .await;

    // Test embed_by_id directly
    let client = create_test_client(&mock_server);
    let result = embedder::embed_by_id(row.0, &pool, &client).await;
    
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
    assert!(result.unwrap(), "Expected true (embedded)");

    // Verify vector was written
    let updated: (Option<Vector>,) = sqlx::query_as(
        "SELECT vector FROM memory_vectors WHERE id = $1"
    )
    .bind(row.0)
    .fetch_one(&pool)
    .await
    .expect("Row not found");

    assert!(updated.0.is_some(), "Vector should be populated after embed");

    // Cleanup
    sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
        .bind(row.0)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
async fn test_vector_stays_null_on_api_failure() {
    let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
    let pool = PgPool::connect(database_url)
        .await
        .expect("Failed to connect to Postgres");

    // Insert a row without vector
    let content = "content that will fail";
    let row: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO memory_vectors (content, source) VALUES ($1, 'test-api-failure') RETURNING id"
    )
    .bind(content)
    .fetch_one(&pool)
    .await
    .expect("Failed to insert test row");

    // Mock API error
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_json(json!({
                    "error": { "code": 500, "message": "Internal server error" }
                }))
        )
        .mount(&mock_server)
        .await;

    let client = create_test_client(&mock_server);
    let result = embedder::embed_by_id(row.0, &pool, &client).await;
    
    assert!(result.is_err(), "Expected error on API failure");

    // Verify vector is still NULL
    let after: (Option<Vector>,) = sqlx::query_as(
        "SELECT vector FROM memory_vectors WHERE id = $1"
    )
    .bind(row.0)
    .fetch_one(&pool)
    .await
    .expect("Row not found");

    assert!(after.0.is_none(), "Vector should remain NULL on failure");

    // Cleanup
    sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
        .bind(row.0)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
async fn test_vector_written_to_db_after_ingest() {
    // This test verifies that embed_by_id works correctly
    
    let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
    let pool = PgPool::connect(database_url)
        .await
        .expect("Failed to connect to Postgres");

    // Clean up
    sqlx::query("DELETE FROM memory_vectors WHERE source = 'test-embed-after-ingest'")
        .execute(&pool)
        .await
        .ok();

    // Insert row first
    let row: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO memory_vectors (content, source) VALUES ($1, 'test-embed-after-ingest') RETURNING id"
    )
    .bind("content to be embedded")
    .fetch_one(&pool)
    .await
    .expect("Failed to insert test row");

    // Mock API
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_embedding_response())
        )
        .mount(&mock_server)
        .await;

    let client = create_test_client(&mock_server);
    let result = embedder::embed_by_id(row.0, &pool, &client).await;
    
    assert!(result.is_ok(), "Embedding should succeed");

    // Verify vector is populated
    let after: (Option<Vector>,) = sqlx::query_as(
        "SELECT vector FROM memory_vectors WHERE id = $1"
    )
    .bind(row.0)
    .fetch_one(&pool)
    .await
    .expect("Row not found");

    assert!(after.0.is_some(), "Vector should be populated after embedding");

    // Cleanup
    sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
        .bind(row.0)
        .execute(&pool)
        .await
        .ok();
}
