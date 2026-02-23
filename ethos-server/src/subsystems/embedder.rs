//! Embedder subsystem — populates vector column in memory_vectors
//!
//! This subsystem is responsible for:
//! - Polling memory_vectors rows where vector IS NULL
//! - Calling the Gemini API to generate embeddings
//! - Writing the resulting vectors back to the database
//!
//! Embedding runs in tokio::spawn AFTER the IPC response is sent — never blocks the caller.

use ethos_core::{
    embeddings::{EmbeddingConfig, EmbeddingError, GeminiEmbeddingClient},
    EthosConfig,
};
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

/// Embedding subsystem configuration derived from EthosConfig
pub struct EmbedderConfig {
    pub api_key: String,
    pub model: String,
    pub dimensions: usize,
    pub max_retries: usize,
    pub retry_delay_ms: u64,
}

impl From<&EthosConfig> for EmbedderConfig {
    fn from(config: &EthosConfig) -> Self {
        // Try config first, then environment variable
        let api_key = std::env::var("GOOGLE_API_KEY").unwrap_or_default();
        
        Self {
            api_key,
            model: config.embedding.gemini_model.clone(),
            dimensions: config.embedding.gemini_dimensions as usize,
            max_retries: 3,
            retry_delay_ms: 1000,
        }
    }
}

/// Create an embedding client from config
pub fn create_client(config: &EmbedderConfig) -> Result<GeminiEmbeddingClient, EmbeddingError> {
    let embedding_config = EmbeddingConfig {
        api_key: config.api_key.clone(),
        model: config.model.clone(),
        dimensions: config.dimensions,
        max_retries: config.max_retries,
        retry_delay_ms: config.retry_delay_ms,
    };
    
    GeminiEmbeddingClient::new(embedding_config)
}

/// Embed a single memory vector by ID
///
/// This function:
/// 1. Reads the content from memory_vectors
/// 2. Calls the embedding API
/// 3. Updates the vector column
///
/// Returns Ok(true) if successful, Ok(false) if row not found or already embedded
pub async fn embed_by_id(
    id: Uuid,
    pool: &PgPool,
    client: &GeminiEmbeddingClient,
) -> anyhow::Result<bool> {
    // 1. Fetch the row using query_as to handle Option<Vector>
    #[derive(sqlx::FromRow)]
    struct MemoryRow {
        content: Option<String>,
        vector: Option<Vector>,
    }
    
    let row: MemoryRow = sqlx::query_as(
        "SELECT content, vector FROM memory_vectors WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Memory vector {} not found", id))?;

    // Skip if already embedded
    if row.vector.is_some() {
        tracing::debug!(id = %id, "Vector already populated, skipping");
        return Ok(false);
    }

    let content = row.content.ok_or_else(|| {
        anyhow::anyhow!("Memory vector {} has no content", id)
    })?;

    // 2. Generate embedding
    let embedding = match client.embed(&content).await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(id = %id, error = %e, "Failed to generate embedding");
            // Row stays vector IS NULL on failure — do NOT panic
            return Err(e.into());
        }
    };

    // 3. Write vector to DB using raw query (pgvector needs special handling)
    let vector = Vector::from(embedding);
    sqlx::query(
        "UPDATE memory_vectors SET vector = $1 WHERE id = $2"
    )
    .bind(&vector)
    .bind(id)
    .execute(pool)
    .await?;

    tracing::info!(id = %id, "Successfully embedded memory vector");
    Ok(true)
}

/// Spawn an async task to embed a memory vector
///
/// This returns immediately and runs the embedding in the background.
/// The row stays with vector IS NULL on failure.
pub fn spawn_embed_task(
    id: Uuid,
    pool: PgPool,
    config: EmbedderConfig,
) {
    tokio::spawn(async move {
        let client = match create_client(&config) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(id = %id, error = %e, "Failed to create embedding client");
                return;
            }
        };

        match embed_by_id(id, &pool, &client).await {
            Ok(true) => tracing::info!(id = %id, "Background embedding completed"),
            Ok(false) => tracing::debug!(id = %id, "Background embedding skipped"),
            Err(e) => tracing::error!(id = %id, error = %e, "Background embedding failed"),
        }
    });
}

/// Process all unembedded rows (for batch/scheduled processing)
///
/// Returns the number of successfully embedded rows
pub async fn embed_all_pending(
    pool: &PgPool,
    client: &GeminiEmbeddingClient,
    limit: usize,
) -> anyhow::Result<usize> {
    #[derive(sqlx::FromRow)]
    struct PendingRow {
        id: Uuid,
        content: Option<String>,
    }

    // Fetch rows where vector IS NULL
    let rows: Vec<PendingRow> = sqlx::query_as(
        "SELECT id, content FROM memory_vectors 
         WHERE vector IS NULL AND content IS NOT NULL 
         ORDER BY created_at ASC LIMIT $1"
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;

    let mut success_count = 0;

    for row in rows {
        let content = row.content.unwrap_or_default();
        
        match client.embed(&content).await {
            Ok(embedding) => {
                let vector = Vector::from(embedding);
                
                match sqlx::query(
                    "UPDATE memory_vectors SET vector = $1 WHERE id = $2"
                )
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
            Err(e) => {
                tracing::error!(id = %row.id, error = %e, "Failed to embed content");
                // Row stays vector IS NULL — continue to next
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
    use ethos_core::embeddings::{EmbeddingConfig as CoreEmbeddingConfig, GeminiEmbeddingClient, GEMINI_DIMENSIONS};
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
        let config = CoreEmbeddingConfig {
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
    async fn test_embed_by_id_writes_vector_to_db() {
        // This test requires a running PostgreSQL with pgvector
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        // Insert a test row without vector
        let content = "test content for embedding";
        let row: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source) VALUES ($1, 'test') RETURNING id"
        )
        .bind(content)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert test row");

        // Start mock server
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(mock_embedding_response())
            )
            .mount(&mock_server)
            .await;

        let client = create_test_client(&mock_server);

        // Embed the row
        let result = embed_by_id(row.0, &pool, &client).await;
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

        assert!(updated.0.is_some(), "Vector should be populated");

        // Cleanup
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
        let client = create_test_client(&mock_server);

        let fake_id = Uuid::new_v4();
        let result = embed_by_id(fake_id, &pool, &client).await;
        
        assert!(result.is_err(), "Expected error for nonexistent row");
    }

    #[tokio::test]
    async fn test_embed_by_id_returns_false_if_already_embedded() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        // Insert a row with vector already set
        let content = "already embedded content";
        let vec_data: Vec<f32> = (0..768).map(|i| i as f32 / 768.0).collect();
        let vector = Vector::from(vec_data);
        
        let row: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source, vector) VALUES ($1, 'test', $2) RETURNING id"
        )
        .bind(content)
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert test row");

        let mock_server = MockServer::start().await;
        let client = create_test_client(&mock_server);

        let result = embed_by_id(row.0, &pool, &client).await;
        assert!(result.is_ok(), "Expected Ok");
        assert!(!result.unwrap(), "Expected false (already embedded)");

        // Cleanup
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

        // Insert a test row
        let content = "content that will fail to embed";
        let row: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source) VALUES ($1, 'test') RETURNING id"
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
                    .set_body_json(serde_json::json!({
                        "error": { "code": 500, "message": "Internal server error" }
                    }))
            )
            .mount(&mock_server)
            .await;

        let client = create_test_client(&mock_server);

        // Embed should fail
        let result = embed_by_id(row.0, &pool, &client).await;
        assert!(result.is_err(), "Expected error on API failure");

        // Verify vector is still NULL
        let updated: (Option<Vector>,) = sqlx::query_as(
            "SELECT vector FROM memory_vectors WHERE id = $1"
        )
        .bind(row.0)
        .fetch_one(&pool)
        .await
        .expect("Row not found");

        assert!(
            updated.0.is_none(),
            "Vector should remain NULL on failure"
        );

        // Cleanup
        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(row.0)
            .execute(&pool)
            .await
            .ok();
    }
}
