//! Ebbinghaus Decay + LTP (Long-Term Potentiation)
//!
//! Salience formula: S(t) = S_0 × e^(-t/τ_eff) × (1 + α×f) × (1 + β×E)
//!
//! Where:
//!   S_0    = current salience (we update in place)
//!   t      = time since last access (in days)
//!   τ_eff  = effective decay time constant (days)
//!            τ_eff = base_tau_days × ltp_multiplier^retrieval_count
//!   α      = frequency_weight (default 0.3)
//!   f      = normalized access frequency (retrieval_count / days_since_created)
//!   β      = emotional_weight (default 0.2)
//!   E      = emotional_tone (0.0 to 1.0)
//!
//! LTP effect: Each retrieval extends the effective time constant.
//! Pruning: if salience < prune_threshold (default 0.05) → set pruned = true

use anyhow::Result;
use chrono::{DateTime, Utc};
use ethos_core::config::DecayConfig;
use sqlx::PgPool;
use uuid::Uuid;

// ============================================================================
// PUBLIC API
// ============================================================================

/// Report from a decay sweep
#[derive(Debug, Clone, Default)]
pub struct DecaySweepReport {
    pub vectors_updated: usize,
    pub vectors_pruned: usize,
    pub episodes_updated: usize,
    pub episodes_pruned: usize,
    pub facts_updated: usize,
    pub facts_pruned: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default)]
struct DecayStats {
    updated: usize,
    pruned: usize,
}

/// Run a full decay sweep over all memory tables.
/// Called by the consolidation loop after each cycle.
pub async fn run_decay_sweep(pool: &PgPool, config: &DecayConfig) -> Result<DecaySweepReport> {
    let start = std::time::Instant::now();
    let mut report = DecaySweepReport::default();

    // Decay each table
    let vectors_stats = decay_memory_vectors(pool, config).await?;
    report.vectors_updated = vectors_stats.updated;
    report.vectors_pruned = vectors_stats.pruned;

    let episodes_stats = decay_episodic_traces(pool, config).await?;
    report.episodes_updated = episodes_stats.updated;
    report.episodes_pruned = episodes_stats.pruned;

    let facts_stats = decay_semantic_facts(pool, config).await?;
    report.facts_updated = facts_stats.updated;
    report.facts_pruned = facts_stats.pruned;

    report.elapsed_ms = start.elapsed().as_millis() as u64;

    tracing::info!(
        "Decay sweep complete: {} vectors ({} pruned), {} episodes ({} pruned), {} facts ({} pruned) in {}ms",
        report.vectors_updated,
        report.vectors_pruned,
        report.episodes_updated,
        report.episodes_pruned,
        report.facts_updated,
        report.facts_pruned,
        report.elapsed_ms
    );

    Ok(report)
}

/// Record a retrieval event for a memory item (LTP effect).
/// Called by retrieve.rs when returning results.
/// Updates: retrieval_count++, last_retrieved_at = NOW(), salience boost.
pub async fn record_retrieval(pool: &PgPool, memory_id: Uuid, source_type: &str) -> Result<()> {
    match source_type {
        "episode" => {
            sqlx::query!(
                r#"
                UPDATE episodic_traces 
                SET retrieval_count = retrieval_count + 1, 
                    last_retrieved_at = NOW(),
                    salience = LEAST(salience * 1.1, 1.0)
                WHERE id = $1
                "#,
                memory_id
            )
            .execute(pool)
            .await?;
        }
        "fact" => {
            sqlx::query!(
                r#"
                UPDATE semantic_facts 
                SET retrieval_count = retrieval_count + 1,
                    last_retrieved_at = NOW(),
                    confidence = LEAST(confidence + 0.02, 1.0),
                    salience = LEAST(salience * 1.1, 1.0)
                WHERE id = $1
                "#,
                memory_id
            )
            .execute(pool)
            .await?;
        }
        _ => {
            // memory_vectors
            sqlx::query!(
                r#"
                UPDATE memory_vectors 
                SET access_count = COALESCE(access_count, 0) + 1,
                    last_accessed = NOW(),
                    importance = LEAST(COALESCE(importance, 0.5) * 1.05, 1.0)
                WHERE id = $1
                "#,
                memory_id
            )
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// Calculate the new salience for a memory item (pure function — no DB calls).
/// Used by tests and by the sweep.
pub fn calculate_salience(
    current_salience: f64,
    retrieval_count: i32,
    created_at: DateTime<Utc>,
    last_accessed: Option<DateTime<Utc>>,
    emotional_tone: f64,
    config: &DecayConfig,
) -> f64 {
    let now = Utc::now();

    // t: days since last access (or since creation if never accessed)
    let last = last_accessed.unwrap_or(created_at);
    let t = (now - last).num_seconds() as f64 / 86400.0;

    // τ_eff: LTP-boosted time constant
    let tau_eff = config.base_tau_days * config.ltp_multiplier.powi(retrieval_count);

    // Ebbinghaus decay
    let decay = (-t / tau_eff).exp();

    // Frequency boost: retrieval_count / days_since_created
    let days_alive = ((now - created_at).num_seconds() as f64 / 86400.0).max(1.0);
    let f = (retrieval_count as f64 / days_alive).min(1.0); // normalize to [0, 1]

    // Emotional boost
    let e = emotional_tone.clamp(0.0, 1.0);

    // Final formula
    let new_salience = current_salience
        * decay
        * (1.0 + config.frequency_weight * f)
        * (1.0 + config.emotional_weight * e);

    new_salience.clamp(0.0, 1.0)
}

// ============================================================================
// INTERNAL HELPERS
// ============================================================================

/// Sweep memory_vectors table
async fn decay_memory_vectors(pool: &PgPool, config: &DecayConfig) -> Result<DecayStats> {
    let mut stats = DecayStats::default();

    // Fetch non-pruned vectors (batch of 500)
    let rows = sqlx::query_as::<_, (Uuid, Option<f64>, Option<i32>, Option<DateTime<Utc>>, DateTime<Utc>, Option<DateTime<Utc>>)>(
        r#"
        SELECT id, importance, access_count, last_accessed, created_at, expires_at
        FROM memory_vectors
        WHERE (pruned = false OR pruned IS NULL)
        LIMIT 500
        "#
    )
    .fetch_all(pool)
    .await?;

    for (id, importance, access_count, last_accessed, created_at, expires_at) in rows {
        let current_salience = importance.unwrap_or(0.5);
        let retrieval_count = access_count.unwrap_or(0);

        // Check if expired
        if let Some(exp) = expires_at {
            if exp <= Utc::now() {
                sqlx::query!(
                    "UPDATE memory_vectors SET pruned = true WHERE id = $1",
                    id
                )
                .execute(pool)
                .await?;
                stats.pruned += 1;
                continue;
            }
        }

        // Calculate new salience (no emotional tone for vectors)
        let new_salience =
            calculate_salience(current_salience, retrieval_count, created_at, last_accessed, 0.0, config);

        if new_salience < config.prune_threshold {
            sqlx::query!(
                "UPDATE memory_vectors SET importance = $1, pruned = true WHERE id = $2",
                new_salience,
                id
            )
            .execute(pool)
            .await?;
            stats.pruned += 1;
        } else if (new_salience - current_salience).abs() > 0.001 {
            sqlx::query!(
                "UPDATE memory_vectors SET importance = $1 WHERE id = $2",
                new_salience,
                id
            )
            .execute(pool)
            .await?;
            stats.updated += 1;
        }
    }

    Ok(stats)
}

/// Sweep episodic_traces table
async fn decay_episodic_traces(pool: &PgPool, config: &DecayConfig) -> Result<DecayStats> {
    let mut stats = DecayStats::default();

    // Fetch non-pruned episodes (batch of 500)
    let rows = sqlx::query_as::<_, (Uuid, f64, i32, Option<DateTime<Utc>>, DateTime<Utc>, f64)>(
        r#"
        SELECT id, salience, retrieval_count, last_retrieved_at, created_at, COALESCE(emotional_tone, 0.0) as emotional_tone
        FROM episodic_traces
        WHERE pruned = false
        LIMIT 500
        "#
    )
    .fetch_all(pool)
    .await?;

    for (id, current_salience, retrieval_count, last_accessed, created_at, emotional_tone) in rows {
        let new_salience = calculate_salience(
            current_salience,
            retrieval_count,
            created_at,
            last_accessed,
            emotional_tone,
            config,
        );

        if new_salience < config.prune_threshold {
            sqlx::query!(
                "UPDATE episodic_traces SET salience = $1, pruned = true WHERE id = $2",
                new_salience,
                id
            )
            .execute(pool)
            .await?;
            stats.pruned += 1;
        } else if (new_salience - current_salience).abs() > 0.001 {
            sqlx::query!(
                "UPDATE episodic_traces SET salience = $1 WHERE id = $2",
                new_salience,
                id
            )
            .execute(pool)
            .await?;
            stats.updated += 1;
        }
    }

    Ok(stats)
}

/// Sweep semantic_facts table (decay confidence, not salience directly)
async fn decay_semantic_facts(pool: &PgPool, config: &DecayConfig) -> Result<DecayStats> {
    let mut stats = DecayStats::default();

    // Fetch non-pruned, non-superseded facts (batch of 500)
    let rows = sqlx::query_as::<_, (Uuid, f64, f64, i32, Option<DateTime<Utc>>, DateTime<Utc>)>(
        r#"
        SELECT id, confidence, salience, retrieval_count, last_retrieved_at, created_at
        FROM semantic_facts
        WHERE pruned = false AND superseded_by IS NULL
        LIMIT 500
        "#
    )
    .fetch_all(pool)
    .await?;

    for (id, confidence, salience, retrieval_count, last_accessed, created_at) in rows {
        // Decay confidence
        let new_confidence =
            calculate_salience(confidence, retrieval_count, created_at, last_accessed, 0.0, config);

        // Decay salience
        let new_salience =
            calculate_salience(salience, retrieval_count, created_at, last_accessed, 0.0, config);

        if new_confidence < config.prune_threshold {
            sqlx::query!(
                "UPDATE semantic_facts SET confidence = $1, salience = $2, pruned = true WHERE id = $3",
                new_confidence,
                new_salience,
                id
            )
            .execute(pool)
            .await?;
            stats.pruned += 1;
        } else if (new_confidence - confidence).abs() > 0.001
            || (new_salience - salience).abs() > 0.001
        {
            sqlx::query!(
                "UPDATE semantic_facts SET confidence = $1, salience = $2 WHERE id = $3",
                new_confidence,
                new_salience,
                id
            )
            .execute(pool)
            .await?;
            stats.updated += 1;
        }
    }

    Ok(stats)
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config() -> DecayConfig {
        DecayConfig {
            base_tau_days: 7.0,
            ltp_multiplier: 1.5,
            frequency_weight: 0.3,
            emotional_weight: 0.2,
            prune_threshold: 0.05,
        }
    }

    // ========================================================================
    // TEST 1: fresh memory (t=0) barely changes
    // ========================================================================
    #[test]
    fn test_calculate_salience_fresh() {
        let config = create_test_config();
        let now = Utc::now();
        let created_at = now - chrono::Duration::seconds(10);

        let salience = calculate_salience(1.0, 0, created_at, None, 0.0, &config);

        // Fresh memory: t≈0, decay≈1, frequency=0, emotional=0
        // salience = 1.0 * e^0 * (1 + 0) * (1 + 0) = 1.0
        assert!(
            salience > 0.95,
            "Fresh memory should have salience near 1.0, got {}",
            salience
        );
    }

    // ========================================================================
    // TEST 2: one week old, no retrievals
    // ========================================================================
    #[test]
    fn test_calculate_salience_one_week() {
        let config = create_test_config();
        let now = Utc::now();
        let created_at = now - chrono::Duration::days(7);

        let salience = calculate_salience(1.0, 0, created_at, None, 0.0, &config);

        // t=7, tau_eff=7 (no LTP), decay = e^(-7/7) = e^(-1) ≈ 0.368
        // salience = 1.0 * 0.368 * 1.0 * 1.0 ≈ 0.368
        assert!(
            (salience - 0.368).abs() < 0.05,
            "One-week-old memory should decay to ~0.368, got {}",
            salience
        );
    }

    // ========================================================================
    // TEST 3: LTP effect (5 retrievals)
    // ========================================================================
    #[test]
    fn test_calculate_salience_with_ltp() {
        let config = create_test_config();
        let now = Utc::now();
        let created_at = now - chrono::Duration::days(30);

        // With 5 retrievals: tau_eff = 7 * 1.5^5 = 53.156
        let salience = calculate_salience(1.0, 5, created_at, None, 0.0, &config);

        // t=30, tau_eff≈53, decay = e^(-30/53) ≈ e^(-0.566) ≈ 0.568
        assert!(
            salience > 0.5,
            "Memory with LTP should retain >0.5 salience, got {}",
            salience
        );

        // Compare with no retrievals
        let salience_no_ltp = calculate_salience(1.0, 0, created_at, None, 0.0, &config);
        assert!(
            salience > salience_no_ltp,
            "LTP should slow decay: {} should be > {}",
            salience,
            salience_no_ltp
        );
    }

    // ========================================================================
    // TEST 4: emotional boost
    // ========================================================================
    #[test]
    fn test_calculate_salience_emotional_boost() {
        let config = create_test_config();
        let now = Utc::now();
        let created_at = now - chrono::Duration::days(7);

        let salience_neutral = calculate_salience(1.0, 0, created_at, None, 0.0, &config);
        let salience_emotional = calculate_salience(1.0, 0, created_at, None, 1.0, &config);

        // emotional boost: (1 + 0.2 * 1.0) = 1.2
        assert!(
            salience_emotional > salience_neutral,
            "Emotional tone should boost salience: {} > {}",
            salience_emotional,
            salience_neutral
        );

        let boost_ratio = salience_emotional / salience_neutral;
        assert!(
            (boost_ratio - 1.2).abs() < 0.05,
            "Boost ratio should be ~1.2, got {}",
            boost_ratio
        );
    }

    // ========================================================================
    // TEST 5: clamp to max 1.0
    // ========================================================================
    #[test]
    fn test_calculate_salience_clamp_max() {
        let config = create_test_config();
        let now = Utc::now();
        let created_at = now - chrono::Duration::seconds(10);

        // High frequency and emotional tone could boost > 1.0
        let salience = calculate_salience(1.0, 100, created_at, Some(now), 1.0, &config);

        assert!(
            salience <= 1.0,
            "Salience should be clamped to 1.0, got {}",
            salience
        );
    }

    // ========================================================================
    // TEST 6: prune threshold
    // ========================================================================
    #[test]
    fn test_calculate_salience_prune_threshold() {
        let config = create_test_config();
        let now = Utc::now();
        let created_at = now - chrono::Duration::days(90);

        let salience = calculate_salience(0.1, 0, created_at, None, 0.0, &config);

        assert!(
            salience < config.prune_threshold,
            "Very old memory should fall below prune threshold: {} < {}",
            salience,
            config.prune_threshold
        );
    }

    // ========================================================================
    // TEST 7: frequency boost
    // ========================================================================
    #[test]
    fn test_calculate_salience_frequency_boost() {
        let config = create_test_config();
        let now = Utc::now();
        let created_at = now - chrono::Duration::days(10);

        let salience_low_freq = calculate_salience(1.0, 1, created_at, None, 0.0, &config);
        let salience_high_freq = calculate_salience(1.0, 10, created_at, None, 0.0, &config);

        assert!(
            salience_high_freq > salience_low_freq,
            "Higher frequency should boost salience: {} > {}",
            salience_high_freq,
            salience_low_freq
        );
    }

    // ========================================================================
    // INTEGRATION TESTS (require DB)
    // ========================================================================

    // ========================================================================
    // TEST 1: decay sweep marks stale memories
    // ========================================================================
    #[tokio::test]
    async fn test_decay_sweep_marks_stale_memories() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let config = create_test_config();

        // Insert memory_vector with old last_accessed and low importance
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = pgvector::Vector::from(vec_data);

        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO memory_vectors (source_type, source_id, vector, importance, last_accessed, created_at)
            VALUES ('query', gen_random_uuid(), $1, 0.1, NOW() - INTERVAL '90 days', NOW() - INTERVAL '90 days')
            RETURNING id
            "#,
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert stale memory");

        // Run decay sweep
        let _report = run_decay_sweep(&pool, &config)
            .await
            .expect("Decay sweep failed");

        // Verify memory was pruned
        let pruned: bool = sqlx::query_scalar("SELECT COALESCE(pruned, false) FROM memory_vectors WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("Failed to check pruned status");

        assert!(pruned, "Stale memory should be marked as pruned");

        // Cleanup
        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST 2: decay sweep preserves fresh memories
    // ========================================================================
    #[tokio::test]
    async fn test_decay_sweep_preserves_fresh_memories() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let config = create_test_config();

        // Insert memory_vector with recent last_accessed and high importance
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = pgvector::Vector::from(vec_data);

        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO memory_vectors (source_type, source_id, vector, importance, last_accessed, created_at)
            VALUES ('query', gen_random_uuid(), $1, 0.9, NOW(), NOW())
            RETURNING id
            "#,
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert fresh memory");

        // Run decay sweep
        let _report = run_decay_sweep(&pool, &config)
            .await
            .expect("Decay sweep failed");

        // Verify memory was NOT pruned
        let pruned: bool = sqlx::query_scalar("SELECT COALESCE(pruned, false) FROM memory_vectors WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("Failed to check pruned status");

        assert!(!pruned, "Fresh memory should NOT be pruned");

        // Verify importance is still above threshold
        let importance: f64 = sqlx::query_scalar("SELECT importance FROM memory_vectors WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("Failed to check importance");

        assert!(
            importance > config.prune_threshold,
            "Fresh memory importance should be above threshold, got {}",
            importance
        );

        // Cleanup
        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST 3: LTP prevents pruning
    // ========================================================================
    #[tokio::test]
    async fn test_ltp_prevents_pruning() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let config = create_test_config();

        // Insert episodic_trace with old access but high retrieval_count
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, session_key, agent_id) VALUES ($1, $2, 'test')",
        )
        .bind(session_id)
        .bind(format!("test-ltp-{}", session_id))
        .execute(&pool)
        .await
        .ok();

        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO episodic_traces (
                session_id, agent_id, turn_index, role, content, 
                salience, retrieval_count, last_retrieved_at, created_at
            )
            VALUES (
                $1, 'test', 0, 'user', 'test content',
                0.5, 10, NOW() - INTERVAL '30 days', NOW() - INTERVAL '30 days'
            )
            RETURNING id
            "#,
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert LTP memory");

        // Run decay sweep
        let _report = run_decay_sweep(&pool, &config)
            .await
            .expect("Decay sweep failed");

        // Verify memory was NOT pruned (LTP extends tau)
        let pruned: bool = sqlx::query_scalar("SELECT pruned FROM episodic_traces WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("Failed to check pruned status");

        assert!(!pruned, "Memory with LTP (high retrieval_count) should NOT be pruned");

        // Cleanup
        sqlx::query("DELETE FROM episodic_traces WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(session_id)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST 4: record_retrieval updates counts
    // ========================================================================
    #[tokio::test]
    async fn test_record_retrieval_updates_counts() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        // Create test session and episode
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, session_key, agent_id) VALUES ($1, $2, 'test')",
        )
        .bind(session_id)
        .bind(format!("test-retrieval-{}", session_id))
        .execute(&pool)
        .await
        .ok();

        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO episodic_traces (session_id, agent_id, turn_index, role, content, salience)
            VALUES ($1, 'test', 0, 'user', 'test', 0.5)
            RETURNING id
            "#,
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert episode");

        // Record retrieval
        record_retrieval(&pool, id, "episode")
            .await
            .expect("record_retrieval failed");

        // Verify updates
        let (retrieval_count, last_retrieved_at, salience): (i32, Option<DateTime<Utc>>, f64) =
            sqlx::query_as(
                "SELECT retrieval_count, last_retrieved_at, salience FROM episodic_traces WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("Failed to fetch episode");

        assert_eq!(retrieval_count, 1, "retrieval_count should be 1");
        assert!(
            last_retrieved_at.is_some(),
            "last_retrieved_at should be set"
        );
        assert!(
            salience > 0.5,
            "salience should be boosted (was 0.5, now {})",
            salience
        );

        // Cleanup
        sqlx::query("DELETE FROM episodic_traces WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(session_id)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST 5: decay sweep report accuracy
    // ========================================================================
    #[tokio::test]
    async fn test_decay_sweep_report() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let config = create_test_config();

        // Insert mix of memories
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = pgvector::Vector::from(vec_data);

        // Fresh memory
        let fresh_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO memory_vectors (source_type, source_id, vector, importance, last_accessed, created_at)
            VALUES ('query', gen_random_uuid(), $1, 0.9, NOW(), NOW())
            RETURNING id
            "#,
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert fresh memory");

        // Stale memory
        let stale_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO memory_vectors (source_type, source_id, vector, importance, last_accessed, created_at)
            VALUES ('query', gen_random_uuid(), $1, 0.1, NOW() - INTERVAL '90 days', NOW() - INTERVAL '90 days')
            RETURNING id
            "#,
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert stale memory");

        // Run sweep
        let report = run_decay_sweep(&pool, &config)
            .await
            .expect("Decay sweep failed");

        // Report should have non-zero counts
        assert!(report.vectors_updated > 0 || report.vectors_pruned > 0, "Report should show activity");
        assert!(report.elapsed_ms > 0, "Report should have elapsed time");

        // Cleanup
        for id in [fresh_id, stale_id] {
            sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .ok();
        }
    }

    // ========================================================================
    // TEST 6: fact confidence decay
    // ========================================================================
    #[tokio::test]
    async fn test_fact_confidence_decay() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let config = create_test_config();

        // Insert semantic_fact with old created_at and low retrieval
        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO semantic_facts (
                kind, statement, subject, predicate, object,
                confidence, salience, retrieval_count, created_at
            )
            VALUES (
                'fact', 'test statement', 'test_subject', 'test_predicate', 'test_object',
                0.8, 0.8, 0, NOW() - INTERVAL '60 days'
            )
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .expect("Failed to insert fact");

        // Run decay sweep
        let _report = run_decay_sweep(&pool, &config)
            .await
            .expect("Decay sweep failed");

        // Verify confidence was reduced
        let new_confidence: f64 =
            sqlx::query_scalar("SELECT confidence FROM semantic_facts WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("Failed to check confidence");

        assert!(
            new_confidence < 0.8,
            "Old fact confidence should decay from 0.8, got {}",
            new_confidence
        );

        // Cleanup
        sqlx::query("DELETE FROM semantic_facts WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST: record_retrieval for "fact" source type
    // ========================================================================
    #[tokio::test]
    async fn test_record_retrieval_fact_source_type() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        // Insert a semantic fact
        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO semantic_facts (
                kind, statement, subject, predicate, object,
                confidence, salience
            )
            VALUES ('fact', 'ltp test fact', 'LTPFact', 'is', 'testvalue', 0.75, 0.75)
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .expect("Failed to insert fact");

        // Record retrieval for "fact" source type
        record_retrieval(&pool, id, "fact")
            .await
            .expect("record_retrieval failed");

        // Verify updates
        let (retrieval_count, last_retrieved_at, confidence, salience): (i32, Option<DateTime<Utc>>, f64, f64) =
            sqlx::query_as(
                "SELECT retrieval_count, last_retrieved_at, confidence, salience FROM semantic_facts WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("Failed to fetch fact");

        assert_eq!(retrieval_count, 1);
        assert!(last_retrieved_at.is_some());
        assert!(confidence > 0.75, "confidence should be boosted, got {}", confidence);
        assert!(salience > 0.75, "salience should be boosted, got {}", salience);

        // Cleanup
        sqlx::query("DELETE FROM semantic_facts WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST: record_retrieval for "vector" (default arm) source type
    // ========================================================================
    #[tokio::test]
    async fn test_record_retrieval_vector_source_type() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        // Insert memory_vector
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = pgvector::Vector::from(vec_data);

        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO memory_vectors (source_type, source_id, vector, importance, access_count)
            VALUES ('query', gen_random_uuid(), $1, 0.5, 0)
            RETURNING id
            "#,
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert vector");

        // Record retrieval with default/vector source type
        record_retrieval(&pool, id, "vector")
            .await
            .expect("record_retrieval failed");

        // Verify access_count incremented
        let (access_count, last_accessed, importance): (Option<i32>, Option<DateTime<Utc>>, Option<f64>) =
            sqlx::query_as(
                "SELECT access_count, last_accessed, importance FROM memory_vectors WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("Failed to fetch vector");

        assert_eq!(access_count.unwrap_or(0), 1, "access_count should be 1");
        assert!(last_accessed.is_some(), "last_accessed should be set");
        assert!(importance.unwrap_or(0.5) >= 0.5, "importance should not decrease");

        // Cleanup
        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST: expired memory_vectors (expires_at in the past)
    // ========================================================================
    #[tokio::test]
    async fn test_decay_sweep_prunes_expired_vectors() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let config = create_test_config();

        // Insert memory_vector with expires_at in the past
        let vec_data: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        let vector = pgvector::Vector::from(vec_data);

        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO memory_vectors (source_type, source_id, vector, importance, expires_at)
            VALUES ('query', gen_random_uuid(), $1, 0.9, NOW() - INTERVAL '1 hour')
            RETURNING id
            "#,
        )
        .bind(&vector)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert expired vector");

        // Run decay sweep
        let _report = run_decay_sweep(&pool, &config)
            .await
            .expect("Decay sweep failed");

        // Verify memory was pruned due to expiration
        let pruned: bool = sqlx::query_scalar(
            "SELECT COALESCE(pruned, false) FROM memory_vectors WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("Failed to check pruned status");

        assert!(pruned, "Expired memory should be pruned");

        // Cleanup
        sqlx::query("DELETE FROM memory_vectors WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST: episodic trace gets pruned when salience falls below threshold
    // ========================================================================
    #[tokio::test]
    async fn test_decay_sweep_prunes_stale_episodes() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let config = create_test_config();

        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, session_key, agent_id) VALUES ($1, $2, 'test')",
        )
        .bind(session_id)
        .bind(format!("test-stale-ep-{}", session_id))
        .execute(&pool)
        .await
        .ok();

        // Insert episodic trace with very low salience and very old access
        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO episodic_traces (
                session_id, agent_id, turn_index, role, content,
                salience, retrieval_count, created_at, last_retrieved_at
            )
            VALUES ($1, 'test', 0, 'user', 'stale episode',
                0.06, 0, NOW() - INTERVAL '120 days', NOW() - INTERVAL '120 days')
            RETURNING id
            "#,
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert stale episode");

        // Run decay sweep
        let _report = run_decay_sweep(&pool, &config)
            .await
            .expect("Decay sweep failed");

        // Verify episode was pruned
        let pruned: bool =
            sqlx::query_scalar("SELECT pruned FROM episodic_traces WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("Failed to check pruned");

        assert!(pruned, "Very stale episode with low salience should be pruned");

        // Cleanup
        sqlx::query("DELETE FROM episodic_traces WHERE id = $1").bind(id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM sessions WHERE id = $1").bind(session_id).execute(&pool).await.ok();
    }

    // ========================================================================
    // TEST: semantic fact gets pruned when confidence falls below threshold
    // ========================================================================
    #[tokio::test]
    async fn test_decay_sweep_prunes_stale_facts() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let config = create_test_config();

        // Insert semantic_fact with very low confidence and very old access
        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO semantic_facts (
                kind, statement, subject, predicate, object,
                confidence, salience, retrieval_count, created_at
            )
            VALUES (
                'fact', 'old stale fact', 'StaleSubject', 'stale_pred', 'stale_obj',
                0.06, 0.06, 0, NOW() - INTERVAL '180 days'
            )
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .expect("Failed to insert stale fact");

        // Run decay sweep
        let _report = run_decay_sweep(&pool, &config)
            .await
            .expect("Decay sweep failed");

        // Verify fact was pruned
        let pruned: bool =
            sqlx::query_scalar("SELECT pruned FROM semantic_facts WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("Failed to check pruned");

        assert!(pruned, "Very stale fact with low confidence should be pruned");

        // Cleanup
        sqlx::query("DELETE FROM semantic_facts WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    }
}
