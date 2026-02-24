//! Embedder subsystem — populates vector column in memory_vectors
//!
//! This subsystem is responsible for:
//! - Polling memory_vectors rows where vector IS NULL
//! - Calling the configured embedding backend to generate embeddings
//! - Writing the resulting vectors back to the database
//!
//! Embedding runs in tokio::spawn AFTER the IPC response is sent — never blocks the caller.

use ethos_core::{
    embeddings::{
        BackendConfig, EmbeddingBackend, EmbeddingConfig, EmbeddingError,
        OnnxConfig,
    },
    onnx_embedder,
    EthosConfig,
};
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

/// Create an embedding backend from the application config.
///
/// Reads `[embedding] backend` to select Gemini, ONNX, or Gemini-fallback-ONNX.
pub fn create_backend_from_config(
    config: &EthosConfig,
) -> Result<Box<dyn EmbeddingBackend>, EmbeddingError> {
    let api_key = std::env::var("GOOGLE_API_KEY").unwrap_or_default();

    let backend_cfg = match config.embedding.backend.as_str() {
        "onnx" => {
            let (model_path, tokenizer_path) =
                onnx_embedder::resolve_onnx_paths(&config.embedding.onnx_model_path);
            BackendConfig::Onnx(OnnxConfig {
                model_path,
                tokenizer_path,
                dimensions: config.embedding.onnx_dimensions as usize,
            })
        }
        "gemini-fallback-onnx" => BackendConfig::GeminiFallbackOnnx(EmbeddingConfig {
            api_key,
            model: config.embedding.gemini_model.clone(),
            dimensions: config.embedding.gemini_dimensions as usize,
            max_retries: 3,
            retry_delay_ms: 1000,
        }),
        _ => {
            // Default: "gemini"
            BackendConfig::Gemini(EmbeddingConfig {
                api_key,
                model: config.embedding.gemini_model.clone(),
                dimensions: config.embedding.gemini_dimensions as usize,
                max_retries: 3,
                retry_delay_ms: 1000,
            })
        }
    };

    ethos_core::embeddings::create_backend(backend_cfg)
}

/// Embed a single memory vector by ID using the provided backend.
///
/// Returns Ok(true) if successful, Ok(false) if row not found or already embedded.
pub async fn embed_by_id(
    id: Uuid,
    pool: &PgPool,
    backend: &dyn EmbeddingBackend,
) -> anyhow::Result<bool> {
    #[derive(sqlx::FromRow)]
    struct MemoryRow {
        content: Option<String>,
        vector: Option<Vector>,
    }

    let row: MemoryRow = sqlx::query_as(
        "SELECT content, vector FROM memory_vectors WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Memory vector {} not found", id))?;

    if row.vector.is_some() {
        tracing::debug!(id = %id, "Vector already populated, skipping");
        return Ok(false);
    }

    let content = row
        .content
        .ok_or_else(|| anyhow::anyhow!("Memory vector {} has no content", id))?;

    match backend.embed(&content).await {
        Ok(Some(embedding)) => {
            let vector = Vector::from(embedding);
            sqlx::query("UPDATE memory_vectors SET vector = $1 WHERE id = $2")
                .bind(&vector)
                .bind(id)
                .execute(pool)
                .await?;
            tracing::info!(id = %id, backend = backend.name(), "Successfully embedded memory vector");
            Ok(true)
        }
        Ok(None) => {
            // Fallback mode: embedding unavailable, leave vector NULL
            tracing::info!(
                id = %id,
                backend = backend.name(),
                "Embedding unavailable — stored without vector (keyword search only)"
            );
            Ok(true)
        }
        Err(e) => {
            tracing::error!(id = %id, error = %e, "Failed to generate embedding");
            Err(e.into())
        }
    }
}

/// Spawn an async task to embed a memory vector using the configured backend.
pub fn spawn_embed_task(id: Uuid, pool: PgPool, config: &EthosConfig) {
    let config = config.clone();
    tokio::spawn(async move {
        let backend = match create_backend_from_config(&config) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(id = %id, error = %e, "Failed to create embedding backend");
                return;
            }
        };

        match embed_by_id(id, &pool, backend.as_ref()).await {
            Ok(true) => tracing::info!(id = %id, "Background embedding completed"),
            Ok(false) => tracing::debug!(id = %id, "Background embedding skipped"),
            Err(e) => tracing::error!(id = %id, error = %e, "Background embedding failed"),
        }
    });
}

/// Process all unembedded rows (for batch/scheduled processing).
///
/// Returns the number of successfully embedded rows.
pub async fn embed_all_pending(
    pool: &PgPool,
    backend: &dyn EmbeddingBackend,
    limit: usize,
) -> anyhow::Result<usize> {
    #[derive(sqlx::FromRow)]
    struct PendingRow {
        id: Uuid,
        content: Option<String>,
    }

    let rows: Vec<PendingRow> = sqlx::query_as(
        "SELECT id, content FROM memory_vectors
         WHERE vector IS NULL AND content IS NOT NULL
         ORDER BY created_at ASC LIMIT $1",
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;

    let mut success_count = 0;

    for row in rows {
        let content = row.content.unwrap_or_default();

        match backend.embed(&content).await {
            Ok(Some(embedding)) => {
                let vector = Vector::from(embedding);
                match sqlx::query("UPDATE memory_vectors SET vector = $1 WHERE id = $2")
                    .bind(&vector)
                    .bind(row.id)
                    .execute(pool)
                    .await
                {
                    Ok(_) => {
                        success_count += 1;
                        tracing::info!(id = %row.id, "Embedded pending memory vector");
                    }
                    Err(e) => {
                        tracing::error!(id = %row.id, error = %e, "Failed to write vector to DB");
                    }
                }
            }
            Ok(None) => {
                // Fallback: no embedding produced — skip (not a success)
                tracing::info!(id = %row.id, "No embedding available, skipping");
            }
            Err(e) => {
                tracing::error!(id = %row.id, error = %e, "Failed to embed content");
            }
        }
    }

    Ok(success_count)
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ethos_core::embeddings::{
        EmbeddingConfig as CoreEmbeddingConfig, GeminiEmbeddingClient, GEMINI_DIMENSIONS,
    };
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

    fn create_test_backend(mock_server: &MockServer) -> Box<dyn EmbeddingBackend> {
        let config = CoreEmbeddingConfig {
            api_key: "test-api-key".to_string(),
            model: "gemini-embedding-001".to_string(),
            dimensions: GEMINI_DIMENSIONS,
            max_retries: 1,
            retry_delay_ms: 10,
        };

        Box::new(
            GeminiEmbeddingClient::with_base_url(config, mock_server.uri())
                .expect("Failed to create test client"),
        )
    }

    #[tokio::test]
    async fn test_embed_by_id_writes_vector_to_db() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let content = "test content for embedding";
        let row: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source) VALUES ($1, 'test') RETURNING id",
        )
        .bind(content)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert test row");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_embedding_response()),
            )
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        let result = embed_by_id(row.0, &pool, backend.as_ref()).await;
        assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
        assert!(result.unwrap(), "Expected true (embedded)");

        let updated: (Option<Vector>,) =
            sqlx::query_as("SELECT vector FROM memory_vectors WHERE id = $1")
                .bind(row.0)
                .fetch_one(&pool)
                .await
                .expect("Row not found");

        assert!(updated.0.is_some(), "Vector should be populated");

        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(row.0)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn test_embed_by_id_returns_false_for_nonexistent() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        let backend = create_test_backend(&mock_server);

        let fake_id = Uuid::new_v4();
        let result = embed_by_id(fake_id, &pool, backend.as_ref()).await;

        assert!(result.is_err(), "Expected error for nonexistent row");
    }

    #[tokio::test]
    async fn test_embed_by_id_returns_false_if_already_embedded() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let content = "already embedded content";
        let vec_data: Vec<f32> = (0..768).map(|i| i as f32 / 768.0).collect();
        let vector = Vector::from(vec_data);

        let row: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source, vector) VALUES ($1, 'test', $2) RETURNING id",
        )
        .bind(content)
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert test row");

        let mock_server = MockServer::start().await;
        let backend = create_test_backend(&mock_server);

        let result = embed_by_id(row.0, &pool, backend.as_ref()).await;
        assert!(result.is_ok(), "Expected Ok");
        assert!(!result.unwrap(), "Expected false (already embedded)");

        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(row.0)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn test_embed_by_id_stays_null_on_api_error() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let content = "content that will fail to embed";
        let row: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source) VALUES ($1, 'test') RETURNING id",
        )
        .bind(content)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert test row");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(serde_json::json!({
                    "error": { "code": 500, "message": "Internal server error" }
                })),
            )
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        let result = embed_by_id(row.0, &pool, backend.as_ref()).await;
        assert!(result.is_err(), "Expected error on API failure");

        let updated: (Option<Vector>,) =
            sqlx::query_as("SELECT vector FROM memory_vectors WHERE id = $1")
                .bind(row.0)
                .fetch_one(&pool)
                .await
                .expect("Row not found");

        assert!(
            updated.0.is_none(),
            "Vector should remain NULL on failure"
        );

        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(row.0)
            .execute(&pool)
            .await
            .ok();
    }
}
