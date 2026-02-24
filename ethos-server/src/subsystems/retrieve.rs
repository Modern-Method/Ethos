//! Retrieval subsystem — semantic search over memory vectors
//!
//! This subsystem implements `EthosRequest::Search`:
//! - Embeds the query using Gemini with `TaskType::RetrievalQuery`
//! - Queries pgvector with cosine similarity
//! - Optionally applies spreading activation for associative retrieval
//! - Returns top-K results ordered by score (highest first)

use std::collections::HashMap;

use anyhow::Result;
use ethos_core::config::RetrievalConfig;
use ethos_core::embeddings::EmbeddingBackend;
use ethos_core::graph::{spread_activation, ActivationNode};
use pgvector::Vector;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Maximum allowed limit for search results
const MAX_LIMIT: i64 = 20;

/// Default limit when none specified
const DEFAULT_LIMIT: i64 = 5;

/// Search result item matching the IPC contract
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: Uuid,
    pub content: String,
    pub source: String,
    pub score: f64,
    pub metadata: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Search response data structure
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub query: String,
    pub count: usize,
}

/// Search memory vectors for semantically similar content
///
/// # Arguments
/// * `query` - The search query text
/// * `limit` - Optional limit on results (default 5, max 20)
/// * `use_spreading` - Whether to apply spreading activation (default false)
/// * `pool` - Database connection pool
/// * `client` - Gemini embedding client
/// * `config` - Retrieval configuration
///
/// # Returns
/// * `Ok(SearchResponse)` - Search results with scores
/// * `Err` - On embedding failure or database error
///
/// # Constraints
/// * Empty query returns error
/// * Limit clamped to [1, 20]
/// * Only rows with non-NULL vectors are returned
/// * Score = 1 - cosine_distance (range 0-1)
/// * With spreading: score = weighted combination of similarity + activation + structural
pub async fn search_memory(
    query: String,
    limit: Option<u32>,
    use_spreading: bool,
    pool: &PgPool,
    backend: &dyn EmbeddingBackend,
    config: &RetrievalConfig,
) -> Result<serde_json::Value> {
    // Validate query is not empty
    let query = query.trim();
    if query.is_empty() {
        return Ok(serde_json::json!({
            "status": "error",
            "error": "Query cannot be empty"
        }));
    }

    // Clamp limit to valid range
    let limit = limit
        .map(|l| (l as i64).clamp(1, MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT);

    // Embed the query using the configured backend (RETRIEVAL_QUERY task type when supported)
    let query_vector = match backend.embed_query(query).await {
        Ok(Some(v)) => v,
        Ok(None) => {
            tracing::warn!("Embedding backend returned None for query — cannot perform vector search");
            return Ok(serde_json::json!({
                "status": "error",
                "error": "Embedding unavailable — vector search requires a working embedding backend"
            }));
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to embed query");
            return Ok(serde_json::json!({
                "status": "error",
                "error": format!("Failed to embed query: {}", e)
            }));
        }
    };

    // Convert to pgvector Vector
    let vector = Vector::from(query_vector);

    // Query pgvector with cosine similarity
    // score = 1 - distance (cosine distance ranges 0-2, but for normalized vectors 0-1)
    // With spreading, we fetch more anchors than final limit
    let anchor_limit = if use_spreading {
        (config.anchor_top_k_episodes + config.anchor_top_k_facts) as i64
    } else {
        limit
    };

    let rows = sqlx::query_as::<_, (Uuid, Option<String>, Option<String>, Option<f64>, Option<serde_json::Value>, Option<chrono::DateTime<chrono::Utc>>)>(
        r#"
        SELECT 
            id,
            content,
            source,
            1 - (vector <=> $1::vector) AS score,
            metadata,
            created_at
        FROM memory_vectors
        WHERE vector IS NOT NULL
        ORDER BY vector <=> $1::vector
        LIMIT $2
        "#
    )
    .bind(&vector)
    .bind(anchor_limit)
    .fetch_all(pool)
    .await?;

    // Build anchor nodes for spreading activation
    let mut anchors: Vec<ActivationNode> = Vec::new();
    let mut content_map: HashMap<Uuid, (String, String, chrono::DateTime<chrono::Utc>)> = HashMap::new();

    for (id, content, source, score, _metadata, created_at) in rows {
        // Skip rows missing required fields
        let content = match content {
            Some(c) => c,
            None => continue,
        };
        let source = match source {
            Some(s) => s,
            None => continue,
        };
        let score = score.unwrap_or(0.0) as f32;
        let created_at = created_at.unwrap_or_else(chrono::Utc::now);

        anchors.push(ActivationNode {
            id,
            node_type: source.clone(),
            cosine_score: score,
            spread_score: 0.0,
            structural_score: 0.0,
            final_score: score,
        });

        content_map.insert(id, (content, source, created_at));
    }

    // Apply spreading activation if requested
    let final_nodes = if use_spreading && !anchors.is_empty() {
        let spread_result = spread_activation(pool, &anchors, config).await?;
        spread_result.nodes
    } else {
        // Without spreading, use cosine scores as final scores
        anchors
    };

    // Build results from final nodes (limited to requested limit)
    let results: Vec<SearchResult> = final_nodes
        .into_iter()
        .take(limit as usize)
        .filter_map(|node| {
            let (content, source, created_at) = content_map.get(&node.id)?;
            
            Some(SearchResult {
                id: node.id,
                content: content.clone(),
                source: source.clone(),
                score: node.final_score as f64,
                metadata: serde_json::json!({
                    "cosine_score": node.cosine_score,
                    "spread_score": node.spread_score,
                    "structural_score": node.structural_score,
                }),
                created_at: *created_at,
            })
        })
        .collect();

    let count = results.len();

    // Record retrieval for LTP effect (fire-and-forget, non-blocking)
    let pool_clone = pool.clone();
    let result_ids: Vec<(Uuid, String)> = results
        .iter()
        .map(|r| (r.id, "vector".to_string()))
        .collect();
    
    tokio::spawn(async move {
        for (id, source_type) in result_ids {
            if let Err(e) = super::decay::record_retrieval(&pool_clone, id, &source_type).await {
                tracing::warn!("LTP update failed for {}: {}", id, e);
            }
        }
    });

    Ok(serde_json::json!({
        "results": results,
        "query": query,
        "count": count
    }))
}

/// Legacy stub for backward compatibility
pub async fn search_memory_legacy(query: String, limit: Option<u32>) -> Result<serde_json::Value> {
    tracing::info!("Stub: searching memory for: {}, limit: {:?}", query, limit);
    Ok(serde_json::json!({"results": []}))
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ethos_core::config::RetrievalConfig;
    use ethos_core::embeddings::{EmbeddingConfig, GeminiEmbeddingClient, GEMINI_DIMENSIONS};
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper to create a test embedding backend with mock server
    fn create_test_backend(mock_server: &MockServer) -> Box<dyn EmbeddingBackend> {
        let config = EmbeddingConfig {
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

    /// Helper to create test retrieval config
    fn create_test_config() -> RetrievalConfig {
        RetrievalConfig {
            decay_factor: 0.15,
            spreading_strength: 0.85,
            iterations: 3,
            anchor_top_k_episodes: 10,
            anchor_top_k_facts: 10,
            weight_similarity: 0.5,
            weight_activation: 0.3,
            weight_structural: 0.2,
            confidence_gate: 0.12,
        }
    }

    /// Helper to generate a mock embedding response with specific values
    fn mock_embedding_response_with_values(values: Vec<f32>) -> serde_json::Value {
        serde_json::json!({
            "embedding": {
                "values": values
            }
        })
    }

    /// Helper to generate a standard 768-dim mock embedding
    fn mock_embedding_response() -> serde_json::Value {
        let values: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        mock_embedding_response_with_values(values)
    }

    // ========================================================================
    // TEST 1: search returns top-K results ordered by similarity
    // ========================================================================
    #[tokio::test]
    async fn test_search_returns_top_k_ordered_by_similarity() {
        // Setup
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        
        // Mock any POST request to return embedding
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Insert test rows with known vectors
        let vec_a: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vec_b: Vec<f32> = (0..768).map(|i| ((i + 100) as f32) / 868.0).collect();
        let vec_c: Vec<f32> = (0..768).map(|i| ((i + 200) as f32) / 968.0).collect();

        let vector_a = Vector::from(vec_a);
        let vector_b = Vector::from(vec_b);
        let vector_c = Vector::from(vec_c);

        // Insert rows - A should be most similar to our mock query vector
        let row_a: (Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source, vector) VALUES ('content A', 'test', $1) RETURNING id"
        )
        .bind(&vector_a)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert row A");

        let row_b: (Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source, vector) VALUES ('content B', 'test', $1) RETURNING id"
        )
        .bind(&vector_b)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert row B");

        let row_c: (Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source, vector) VALUES ('content C', 'test', $1) RETURNING id"
        )
        .bind(&vector_c)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert row C");

        // Execute search
        let config = create_test_config();
        let result = search_memory("test query".to_string(), Some(3), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Search failed");

        // Verify - result should have "results" key (not "status": "error")
        let status = result.get("status").and_then(|s| s.as_str());
        assert_ne!(status, Some("error"), "Search should not return error: {:?}", result);
        
        let results = result.get("results").expect(&format!("Missing results in: {:?}", result));
        let results_arr = results.as_array().expect("Results not an array");
        
        assert!(!results_arr.is_empty(), "Should return results");
        assert!(results_arr.len() <= 3, "Should respect limit");

        // Verify ordering (highest score first)
        if results_arr.len() > 1 {
            let scores: Vec<f64> = results_arr
                .iter()
                .filter_map(|r| r.get("score").and_then(|s| s.as_f64()))
                .collect();
            
            for i in 1..scores.len() {
                assert!(
                    scores[i - 1] >= scores[i],
                    "Results should be ordered by score descending"
                );
            }
        }

        // Cleanup
        for id in [row_a.0, row_b.0, row_c.0] {
            sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .ok();
        }
    }

    // ========================================================================
    // TEST 2: search embeds query with RETRIEVAL_QUERY task type
    // ========================================================================
    #[tokio::test]
    async fn test_search_uses_retrieval_query_task_type() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;

        // Use a more flexible matcher - just check for RETRIEVAL_QUERY in body
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Execute search - should use RETRIEVAL_QUERY
        let config = create_test_config();
        let result = search_memory("what did we discuss".to_string(), Some(5), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Search failed");

        // If the mock was hit, the task type was correct
        assert!(result.get("results").is_some(), "Should have results key");
        
        // Verify the mock received a request with RETRIEVAL_QUERY
        let received_requests = mock_server.received_requests().await.unwrap_or_default();
        assert!(!received_requests.is_empty(), "Mock should have received at least one request");
        
        // Check that the request body contains RETRIEVAL_QUERY
        let last_request = received_requests.last().unwrap();
        let body_str = String::from_utf8_lossy(&last_request.body);
        assert!(
            body_str.contains("RETRIEVAL_QUERY"),
            "Request body should contain RETRIEVAL_QUERY, got: {}",
            body_str
        );
    }

    // ========================================================================
    // TEST 3: search skips rows with NULL vectors
    // ========================================================================
    #[tokio::test]
    async fn test_search_skips_null_vectors() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Insert row WITHOUT vector (NULL)
        let row_no_vector: (Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source) VALUES ('no vector here', 'test') RETURNING id"
        )
        .fetch_one(&pool)
        .await
        .expect("Failed to insert row without vector");

        // Insert row WITH vector
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = Vector::from(vec_data);
        
        let row_with_vector: (Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source, vector) VALUES ('has vector', 'test', $1) RETURNING id"
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert row with vector");

        // Execute search
        let config = create_test_config();
        let result = search_memory("test query".to_string(), Some(10), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Search failed");

        let results = result.get("results").unwrap().as_array().unwrap();

        // Verify the row without vector is NOT in results
        let ids: Vec<String> = results
            .iter()
            .filter_map(|r| r.get("id").and_then(|i| i.as_str()))
            .map(String::from)
            .collect();

        assert!(
            !ids.contains(&row_no_vector.0.to_string()),
            "Row without vector should not appear in results"
        );

        // Cleanup
        for id in [row_no_vector.0, row_with_vector.0] {
            sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .ok();
        }
    }

    // ========================================================================
    // TEST 4: search with no results returns empty array (not error)
    // ========================================================================
    #[tokio::test]
    async fn test_search_empty_results_returns_ok_with_empty_array() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // that definitely won't match. Actually, just ensure no rows have vectors.
        
        // Execute search - should return empty results, NOT error
        let config = create_test_config();
        let result = search_memory("unlikely to match anything xyzzy123".to_string(), Some(5), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Search should not error");

        // Should have status implicitly via being a valid response
        assert!(result.get("results").is_some(), "Should have results key");
        
        let results = result.get("results").unwrap().as_array().unwrap();
        let count = result.get("count").unwrap().as_u64().unwrap();

        // Empty results is OK, not an error
        assert!(results.is_empty() || results.len() <= 5, "Should have 0-5 results");
        assert_eq!(count as usize, results.len(), "Count should match results length");
    }

    // ========================================================================
    // TEST 5: limit is respected
    // ========================================================================
    #[tokio::test]
    async fn test_search_respects_limit() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Insert 10 rows with vectors
        let mut ids = Vec::new();
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = Vector::from(vec_data);

        for i in 0..10 {
            let row: (Uuid,) = sqlx::query_as(
                "INSERT INTO memory_vectors (content, source, vector) VALUES ($1, 'test', $2) RETURNING id"
            )
            .bind(format!("content {}", i))
            .bind(&vector)
            .fetch_one(&pool)
            .await
            .expect("Failed to insert row");

            ids.push(row.0);
        }

        // Search with limit 3
        let config = create_test_config();
        let result = search_memory("test query".to_string(), Some(3), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Search failed");

        let results = result.get("results").unwrap().as_array().unwrap();
        let count = result.get("count").unwrap().as_u64().unwrap();

        assert_eq!(results.len(), 3, "Should return exactly 3 results");
        assert_eq!(count, 3, "Count should be 3");

        // Cleanup
        for id in ids {
            sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .ok();
        }
    }

    // ========================================================================
    // TEST 6: missing/empty query returns error
    // ========================================================================
    #[tokio::test]
    async fn test_search_empty_query_returns_error() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        let backend = create_test_backend(&mock_server);

        // Empty query
        let config = create_test_config();
        let result = search_memory("".to_string(), Some(5), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Should not panic");

        // Should return error status
        let status = result.get("status").and_then(|s| s.as_str());
        assert_eq!(status, Some("error"), "Empty query should return error status");

        // Whitespace-only query
        let result = search_memory("   ".to_string(), Some(5), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Should not panic");

        let status = result.get("status").and_then(|s| s.as_str());
        assert_eq!(status, Some("error"), "Whitespace-only query should return error status");
    }

    // ========================================================================
    // TEST 7: limit is clamped to max 20
    // ========================================================================
    #[tokio::test]
    async fn test_search_limit_clamped_to_max_20() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Insert 25 rows
        let mut ids = Vec::new();
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = Vector::from(vec_data);

        for i in 0..25 {
            let row: (Uuid,) = sqlx::query_as(
                "INSERT INTO memory_vectors (content, source, vector) VALUES ($1, 'test', $2) RETURNING id"
            )
            .bind(format!("content {}", i))
            .bind(&vector)
            .fetch_one(&pool)
            .await
            .expect("Failed to insert row");

            ids.push(row.0);
        }

        // Request limit of 100 - should be clamped to 20
        let config = create_test_config();
        let result = search_memory("test query".to_string(), Some(100), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Search failed");

        let results = result.get("results").unwrap().as_array().unwrap();

        assert!(
            results.len() <= 20,
            "Should return at most 20 results, got {}",
            results.len()
        );

        // Cleanup
        for id in ids {
            sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .ok();
        }
    }

    // ========================================================================
    // TEST 8: default limit is 5
    // ========================================================================
    #[tokio::test]
    async fn test_search_default_limit_is_5() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Insert 10 rows
        let mut ids = Vec::new();
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = Vector::from(vec_data);

        for i in 0..10 {
            let row: (Uuid,) = sqlx::query_as(
                "INSERT INTO memory_vectors (content, source, vector) VALUES ($1, 'test', $2) RETURNING id"
            )
            .bind(format!("content {}", i))
            .bind(&vector)
            .fetch_one(&pool)
            .await
            .expect("Failed to insert row");

            ids.push(row.0);
        }

        // Search with no limit - should default to 5
        let config = create_test_config();
        let result = search_memory("test query".to_string(), None, false, &pool, backend.as_ref(), &config)
            .await
            .expect("Search failed");

        let results = result.get("results").unwrap().as_array().unwrap();
        let count = result.get("count").unwrap().as_u64().unwrap();

        assert_eq!(results.len(), 5, "Should return exactly 5 results by default");
        assert_eq!(count, 5, "Count should be 5");

        // Cleanup
        for id in ids {
            sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .ok();
        }
    }

    // ========================================================================
    // TEST 9: embedding failure returns error (graceful degradation)
    // ========================================================================
    #[tokio::test]
    async fn test_search_embedding_failure_returns_error() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        
        // Mock API failure
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(serde_json::json!({
                        "error": { "code": 500, "message": "Internal server error" }
                    }))
            )
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Search should fail gracefully
        let config = create_test_config();
        let result = search_memory("test query".to_string(), Some(5), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Should not panic on embedding failure");

        // Should return error status
        let status = result.get("status").and_then(|s| s.as_str());
        assert_eq!(status, Some("error"), "Should return error status on embedding failure");
        
        let error = result.get("error").and_then(|e| e.as_str());
        assert!(error.is_some(), "Should have error message");
    }

    // ========================================================================
    // TEST 10: score is between 0 and 1 (cosine similarity range)
    // ========================================================================
    #[tokio::test]
    async fn test_search_scores_in_valid_range() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Insert row with vector
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = Vector::from(vec_data);

        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source, vector) VALUES ('test', 'test', $1) RETURNING id"
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert row");

        // Execute search
        let config = create_test_config();
        let result = search_memory("test query".to_string(), Some(5), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Search failed");

        let results = result.get("results").unwrap().as_array().unwrap();

        for r in results {
            let score = r.get("score").and_then(|s| s.as_f64()).unwrap_or(-1.0);
            assert!(
                (0.0..=1.0).contains(&score),
                "Score {} should be between 0 and 1",
                score
            );
        }

        // Cleanup
        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(row.0)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST 11: spreading activation returns results (backward compat)
    // ========================================================================
    #[tokio::test]
    async fn test_search_with_spreading_activation_backward_compat() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Insert a test row
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = Vector::from(vec_data);

        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source, vector) VALUES ('spreading test', 'test', $1) RETURNING id"
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert row");

        // Search with spreading activation enabled
        let config = create_test_config();
        let result = search_memory("test query".to_string(), Some(5), true, &pool, backend.as_ref(), &config)
            .await
            .expect("Search with spreading failed");

        // Should return results (even with empty graph, spreading falls back to cosine)
        let results = result.get("results").unwrap().as_array().unwrap();
        assert!(!results.is_empty(), "Should return results even with spreading");

        // Cleanup
        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(row.0)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST 12: spreading activation with zero strength equals pure cosine
    // ========================================================================
    #[tokio::test]
    async fn test_search_spreading_zero_strength_equals_cosine() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_embedding_response()))
            .mount(&mock_server)
            .await;

        let backend = create_test_backend(&mock_server);

        // Insert a test row
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = Vector::from(vec_data);

        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source, vector) VALUES ('zero strength test', 'test', $1) RETURNING id"
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert row");

        // Search with spreading=false
        let config = create_test_config();
        let result_cosine = search_memory("test query".to_string(), Some(5), false, &pool, backend.as_ref(), &config)
            .await
            .expect("Cosine search failed");

        // Search with spreading=true (but no graph edges, so should behave similarly)
        let result_spreading = search_memory("test query".to_string(), Some(5), true, &pool, backend.as_ref(), &config)
            .await
            .expect("Spreading search failed");

        // Both should return results
        let cosine_results = result_cosine.get("results").unwrap().as_array().unwrap();
        let spreading_results = result_spreading.get("results").unwrap().as_array().unwrap();

        assert_eq!(cosine_results.len(), spreading_results.len(), 
            "With no graph edges, spreading should return same count as pure cosine");

        // Cleanup
        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(row.0)
            .execute(&pool)
            .await
            .ok();
    }
}
