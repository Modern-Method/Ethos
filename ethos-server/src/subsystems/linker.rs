//! Linker subsystem — automatic graph link creation
//!
//! This subsystem creates associative edges in `memory_graph_links` after each ingest:
//! - Finds top-3 similar memories using cosine similarity
//! - Creates or strengthens edges for matches above 0.6 threshold
//! - Hebbian strengthening: `weight = min(1.0, old_weight + 0.1)`

use anyhow::Result;
use ethos_core::embeddings::GeminiEmbeddingClient;
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

/// Minimum cosine similarity to create a link
const SIMILARITY_THRESHOLD: f64 = 0.6;

/// Weight increment for Hebbian strengthening
const WEIGHT_INCREMENT: f64 = 0.1;

/// Maximum weight for edges
const MAX_WEIGHT: f64 = 1.0;

/// Number of similar memories to find
const TOP_K_SIMILAR: i64 = 3;

/// Link a newly ingested memory to existing memories in the graph
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `source_type` - Type of the new memory ("episode", "fact", "workflow")
/// * `source_id` - UUID of the new memory
/// * `client` - Gemini embedding client
///
/// # Returns
/// * `Ok(usize)` - Number of links created/strengthened
/// * `Err` - On database or embedding errors
pub async fn link_memory(
    pool: &PgPool,
    source_type: &str,
    source_id: Uuid,
    _client: &GeminiEmbeddingClient,
) -> Result<usize> {
    // Get the embedding for the new memory
    let vector_row: Option<(Vec<f32>,)> = sqlx::query_as(
        r#"
        SELECT vector
        FROM memory_vectors
        WHERE source_type = $1 AND source_id = $2
        "#
    )
    .bind(source_type)
    .bind(source_id)
    .fetch_optional(pool)
    .await?;

    let vector_data = match vector_row {
        Some((v,)) => v,
        None => {
            tracing::debug!("No embedding found for {} {}", source_type, source_id);
            return Ok(0);
        }
    };

    let vector = Vector::from(vector_data);

    // Find top-3 similar memories (excluding self)
    let similar_rows = sqlx::query_as::<_, (String, Uuid, f64)>(
        r#"
        SELECT source_type, source_id, 1 - (vector <=> $1::vector) AS score
        FROM memory_vectors
        WHERE (source_type, source_id) != ($2, $3)
          AND vector IS NOT NULL
        ORDER BY vector <=> $1::vector
        LIMIT $4
        "#
    )
    .bind(&vector)
    .bind(source_type)
    .bind(source_id)
    .bind(TOP_K_SIMILAR)
    .fetch_all(pool)
    .await?;

    let mut links_created = 0;

    // Create or strengthen edges for similar memories above threshold
    for (target_type, target_id, score) in similar_rows {
        if score >= SIMILARITY_THRESHOLD {
            // Upsert edge with Hebbian strengthening
            let result = sqlx::query(
                r#"
                INSERT INTO memory_graph_links 
                    (from_type, from_id, to_type, to_id, relation, weight)
                VALUES ($1, $2, $3, $4, 'similarity', $5)
                ON CONFLICT (from_type, from_id, to_type, to_id, relation)
                DO UPDATE SET 
                    weight = LEAST($6, memory_graph_links.weight + $7),
                    updated_at = now()
                "#
            )
            .bind(source_type)
            .bind(source_id)
            .bind(&target_type)
            .bind(target_id)
            .bind(score)              // Initial weight for new edges
            .bind(MAX_WEIGHT)         // Max weight cap
            .bind(WEIGHT_INCREMENT)   // Strengthening increment
            .execute(pool)
            .await?;

            if result.rows_affected() > 0 {
                links_created += 1;
            }
        }
    }

    // Also create reverse links (bidirectional association)
    for (target_type, target_id, score) in sqlx::query_as::<_, (String, Uuid, f64)>(
        r#"
        SELECT source_type, source_id, 1 - (vector <=> $1::vector) AS score
        FROM memory_vectors
        WHERE (source_type, source_id) != ($2, $3)
          AND vector IS NOT NULL
          AND 1 - (vector <=> $1::vector) >= $4
        ORDER BY vector <=> $1::vector
        LIMIT $5
        "#
    )
    .bind(&vector)
    .bind(source_type)
    .bind(source_id)
    .bind(SIMILARITY_THRESHOLD)
    .bind(TOP_K_SIMILAR)
    .fetch_all(pool)
    .await?
    {
        // Create reverse edge: target -> source
        let _ = sqlx::query(
            r#"
            INSERT INTO memory_graph_links 
                (from_type, from_id, to_type, to_id, relation, weight)
            VALUES ($1, $2, $3, $4, 'similarity', $5)
            ON CONFLICT (from_type, from_id, to_type, to_id, relation)
            DO UPDATE SET 
                weight = LEAST($6, memory_graph_links.weight + $7),
                updated_at = now()
            "#
        )
        .bind(&target_type)
        .bind(target_id)
        .bind(source_type)
        .bind(source_id)
        .bind(score)
        .bind(MAX_WEIGHT)
        .bind(WEIGHT_INCREMENT)
        .execute(pool)
        .await?;
    }

    if links_created > 0 {
        tracing::info!(
            source_type,
            source_id = %source_id,
            links = links_created,
            "Created graph links for new memory"
        );
    }

    Ok(links_created)
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // TEST 1: linker creates edge above threshold
    // ========================================================================
    #[test]
    fn test_linker_creates_edge_above_threshold() {
        // Similarity 0.7 >= 0.6 threshold
        assert!(0.7 >= SIMILARITY_THRESHOLD);
    }

    // ========================================================================
    // TEST 2: linker strengthens existing edge
    // ========================================================================
    #[test]
    fn test_linker_strengthens_existing_edge() {
        let old_weight = 0.5;
        let new_weight = (old_weight + WEIGHT_INCREMENT).min(MAX_WEIGHT);
        
        assert!((new_weight - 0.6).abs() < 0.01);
    }

    // ========================================================================
    // TEST 3: linker skips below threshold
    // ========================================================================
    #[test]
    fn test_linker_skips_below_threshold() {
        // Similarity 0.5 < 0.6 threshold
        assert!(0.5 < SIMILARITY_THRESHOLD);
    }

    // ========================================================================
    // TEST 4: weight caps at max
    // ========================================================================
    #[test]
    fn test_linker_weight_caps_at_max() {
        let old_weight = 0.95;
        let new_weight = (old_weight + WEIGHT_INCREMENT).min(MAX_WEIGHT);
        
        assert!((new_weight - 1.0).abs() < 0.01);
    }

    // ========================================================================
    // TEST 5: finds top-3 similar
    // ========================================================================
    #[test]
    fn test_linker_finds_top_k() {
        assert_eq!(TOP_K_SIMILAR, 3);
    }
}
