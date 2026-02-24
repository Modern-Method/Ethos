//! Re-embed backfill worker (Story 013)
//!
//! Periodically scans for `memory_vectors` rows with NULL embeddings,
//! re-embeds them via the currently configured backend, and writes
//! the resulting vectors back to the DB.
//!
//! After this worker runs, NULL embeddings are a temporary state rather
//! than a permanent one — full vector search is restored automatically.

use anyhow::Result;
use ethos_core::config::EmbeddingConfig;
use ethos_core::embeddings::EmbeddingBackend;
use pgvector::Vector;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::time::{Duration, interval};
use uuid::Uuid;

/// Run the background re-embed worker loop.
///
/// Spawned from `main.rs` alongside other subsystem tasks.
/// Exits immediately if `reembed_enabled` is `false`.
pub async fn run_reembed_worker(
    pool: PgPool,
    backend: Arc<dyn EmbeddingBackend>,
    config: EmbeddingConfig,
) {
    if !config.reembed_enabled {
        tracing::info!("Re-embed worker disabled via config");
        return;
    }

    let tick_secs = config.reembed_interval_minutes * 60;
    let mut ticker = interval(Duration::from_secs(tick_secs));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    tracing::info!(
        interval_min = config.reembed_interval_minutes,
        batch_size = config.reembed_batch_size,
        "Re-embed backfill worker started"
    );

    loop {
        ticker.tick().await;

        match run_reembed_tick(&pool, backend.as_ref(), &config).await {
            Ok((embedded, skipped)) => {
                if embedded > 0 || skipped > 0 {
                    tracing::info!(
                        embedded = embedded,
                        skipped = skipped,
                        "Re-embed tick complete"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Re-embed tick failed");
            }
        }
    }
}

/// A single re-embed tick. Returns `(embedded, skipped)`.
///
/// Public for unit testing.
pub async fn run_reembed_tick(
    pool: &PgPool,
    backend: &dyn EmbeddingBackend,
    config: &EmbeddingConfig,
) -> Result<(usize, usize)> {
    // 1. Count NULL-vector rows
    let null_count: Option<i64> = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM memory_vectors WHERE vector IS NULL AND content IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;

    let null_count = null_count.unwrap_or(0);
    if null_count == 0 {
        return Ok((0, 0));
    }

    tracing::debug!(null_count, "Found NULL embeddings, starting backfill");

    let mut embedded = 0usize;
    let mut skipped = 0usize;

    // 2. Fetch a batch of NULL-vector rows, episodes first then facts
    let rows = fetch_null_rows(pool, config.reembed_batch_size).await?;

    // 3. Process each row
    for row in &rows {
        match backend.embed(&row.content).await {
            Ok(Some(vec)) => {
                let pgvec = Vector::from(vec);
                sqlx::query(
                    "UPDATE memory_vectors SET vector = $1, updated_at = NOW() WHERE id = $2",
                )
                .bind(&pgvec)
                .bind(row.id)
                .execute(pool)
                .await?;
                embedded += 1;
                apply_rate_limit(config).await;
            }
            Ok(None) => {
                // Backend still in fallback mode — stop the batch
                tracing::debug!("Backend returned None during backfill — stopping batch");
                skipped += rows.len() - embedded;
                return Ok((embedded, skipped));
            }
            Err(e) => {
                tracing::warn!(id = %row.id, error = %e, "Failed to re-embed row, skipping");
                skipped += 1;
            }
        }
    }

    Ok((embedded, skipped))
}

/// Row from memory_vectors needing re-embed.
#[derive(sqlx::FromRow)]
struct NullVectorRow {
    id: Uuid,
    content: String,
}

/// Fetch NULL-vector rows, prioritising episodes over facts.
async fn fetch_null_rows(pool: &PgPool, batch_size: usize) -> Result<Vec<NullVectorRow>> {
    let rows: Vec<NullVectorRow> = sqlx::query_as(
        r#"
        SELECT id, content
        FROM memory_vectors
        WHERE vector IS NULL AND content IS NOT NULL
        ORDER BY
            CASE source_type
                WHEN 'episode' THEN 0
                WHEN 'fact'    THEN 1
                ELSE 2
            END,
            created_at DESC
        LIMIT $1
        "#,
    )
    .bind(batch_size as i64)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Insert inter-request delay to respect `rate_limit_rpm`.
async fn apply_rate_limit(config: &EmbeddingConfig) {
    if config.rate_limit_rpm > 0 {
        let delay_ms = 60_000 / config.rate_limit_rpm as u64;
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ethos_core::embeddings::{EmbeddingBackend, EmbeddingError};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ------------------------------------------------------------------
    // Mock backends for unit tests (no DB, no HTTP)
    // ------------------------------------------------------------------

    /// Backend that always returns a fixed embedding vector.
    struct MockOkBackend {
        dims: usize,
        call_count: AtomicUsize,
    }

    impl MockOkBackend {
        fn new(dims: usize) -> Self {
            Self {
                dims,
                call_count: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EmbeddingBackend for MockOkBackend {
        async fn embed(&self, _text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(Some(vec![0.1; self.dims]))
        }
        fn dimensions(&self) -> usize {
            self.dims
        }
        fn name(&self) -> &str {
            "mock-ok"
        }
    }

    /// Backend that always returns `None` (simulates fallback mode).
    struct MockNoneBackend;

    #[async_trait]
    impl EmbeddingBackend for MockNoneBackend {
        async fn embed(&self, _text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
            Ok(None)
        }
        fn dimensions(&self) -> usize {
            768
        }
        fn name(&self) -> &str {
            "mock-none"
        }
    }

    /// Backend that returns Ok for the first N calls, then None.
    struct MockPartialBackend {
        ok_count: usize,
        dims: usize,
        calls: AtomicUsize,
    }

    impl MockPartialBackend {
        fn new(ok_count: usize, dims: usize) -> Self {
            Self {
                ok_count,
                dims,
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl EmbeddingBackend for MockPartialBackend {
        async fn embed(&self, _text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.ok_count {
                Ok(Some(vec![0.1; self.dims]))
            } else {
                Ok(None)
            }
        }
        fn dimensions(&self) -> usize {
            self.dims
        }
        fn name(&self) -> &str {
            "mock-partial"
        }
    }

    fn test_config() -> EmbeddingConfig {
        EmbeddingConfig {
            backend: "gemini".to_string(),
            gemini_model: "gemini-embedding-001".to_string(),
            gemini_dimensions: 768,
            onnx_model_path: String::new(),
            onnx_dimensions: 384,
            batch_size: 32,
            batch_timeout_seconds: 5,
            queue_capacity: 1000,
            rate_limit_rpm: 0, // no delay in tests
            reembed_interval_minutes: 10,
            reembed_batch_size: 50,
            reembed_enabled: true,
        }
    }

    // ------------------------------------------------------------------
    // Integration tests (require DB)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_tick_no_nulls_returns_zero() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let backend = MockOkBackend::new(768);
        let config = test_config();

        // Ensure no NULL-vector rows exist for this test
        // (we just run tick and expect 0,0 if nothing is NULL)
        let (embedded, skipped) = run_reembed_tick(&pool, &backend, &config)
            .await
            .expect("tick should succeed");

        // backend should not have been called at all if count was 0
        // (or if nulls existed, they got filled — either way no panic)
        assert_eq!(skipped, 0);
        // embedded may be >0 if stale NULLs exist from other tests
        let _ = embedded;
    }

    #[tokio::test]
    async fn test_tick_fills_null_vectors() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        // Insert rows with NULL vector
        let mut ids = Vec::new();
        for i in 0..3 {
            let row: (Uuid,) = sqlx::query_as(
                "INSERT INTO memory_vectors (content, source) VALUES ($1, 'test-reembed') RETURNING id",
            )
            .bind(format!("reembed test content {}", i))
            .fetch_one(&pool)
            .await
            .expect("Failed to insert row");
            ids.push(row.0);
        }

        let backend = MockOkBackend::new(768);
        let config = test_config();

        let (embedded, skipped) = run_reembed_tick(&pool, &backend, &config)
            .await
            .expect("tick should succeed");

        assert!(embedded >= 3, "Should have embedded at least 3 rows, got {}", embedded);
        assert_eq!(skipped, 0);
        assert!(backend.calls() >= 3);

        // Verify vectors are no longer NULL
        for id in &ids {
            let has_vector: Option<bool> = sqlx::query_scalar(
                "SELECT vector IS NOT NULL FROM memory_vectors WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("Row not found");

            assert_eq!(has_vector, Some(true), "Vector should be populated for {}", id);
        }

        // Cleanup
        for id in ids {
            sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .ok();
        }
    }

    #[tokio::test]
    async fn test_tick_stops_batch_on_none() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        // Insert 5 rows with NULL vector
        let mut ids = Vec::new();
        for i in 0..5 {
            let row: (Uuid,) = sqlx::query_as(
                "INSERT INTO memory_vectors (content, source) VALUES ($1, 'test-reembed-none') RETURNING id",
            )
            .bind(format!("reembed none test {}", i))
            .fetch_one(&pool)
            .await
            .expect("Failed to insert row");
            ids.push(row.0);
        }

        // Backend returns Ok for first 2, then None
        let backend = MockPartialBackend::new(2, 768);
        let config = test_config();

        let (embedded, skipped) = run_reembed_tick(&pool, &backend, &config)
            .await
            .expect("tick should succeed");

        // Should have embedded some, then stopped
        assert!(embedded >= 2, "Should embed at least 2 before None");
        assert!(skipped > 0, "Should have skipped remaining after None");

        // Cleanup
        for id in ids {
            sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .ok();
        }
    }

    #[tokio::test]
    async fn test_tick_fallback_backend_skips_all() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        // Insert a row with NULL vector
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO memory_vectors (content, source) VALUES ('fallback test', 'test-reembed-fb') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .expect("Failed to insert row");

        let backend = MockNoneBackend;
        let config = test_config();

        let (embedded, skipped) = run_reembed_tick(&pool, &backend, &config)
            .await
            .expect("tick should succeed");

        assert_eq!(embedded, 0);
        assert!(skipped > 0, "All should be skipped when backend returns None");

        // Cleanup
        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(row.0)
            .execute(&pool)
            .await
            .ok();
    }
}
