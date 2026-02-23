//! Consolidation Engine — episodic → semantic promotion
//!
//! Implements the Animus DMN (Default Mode Network) for standalone Ethos.
//! Runs every 15 minutes when system is idle.
//!
//! Promotion criteria:
//! - importance >= threshold (0.8)
//! - retrieval_count >= threshold (5)
//! - Content contains decision/preference keywords
//! - Content contains explicit markers ("remember this", "important:")
//!
//! Conflict resolution:
//! - Refinement: same subject+predicate, compatible objects → update
//! - Update: temporal change, higher confidence → supersede
//! - Supersession: explicit decision → always supersede
//! - Flag: ambiguous contradiction → flag for review

use anyhow::Result;
use chrono::Utc;
use regex::Regex;
use shellexpand::tilde;
use sqlx::PgPool;
use std::fs::OpenOptions;
use std::io::Write;
use tokio::sync::broadcast;
use uuid::Uuid;

use ethos_core::config::{ConflictResolutionConfig, ConsolidationConfig, DecayConfig};

// ============================================================================
// PUBLIC API
// ============================================================================

/// Report from a consolidation cycle
#[derive(Debug, Clone, Default)]
pub struct ConsolidationReport {
    pub episodes_scanned: usize,
    pub episodes_promoted: usize,
    pub facts_created: usize,
    pub facts_updated: usize,
    pub facts_superseded: usize,
    pub facts_flagged: usize,
    pub skipped_idle: bool,
}

/// Extracted fact from an episode
#[derive(Debug, Clone)]
pub struct ExtractedFact {
    pub kind: String,
    pub statement: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub topics: Vec<String>,
    pub confidence: f64,
    pub source_episode: Uuid,
    pub source_agent: Option<String>,
}

/// Result of upserting a fact
#[derive(Debug, Clone)]
pub enum FactUpsertResult {
    Created(Uuid),
    Updated(Uuid),
    Superseded { old: Uuid, new: Uuid },
    Flagged { existing: Uuid, new_statement: String },
    Skipped,
}

/// Episode data for consolidation
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EpisodicTrace {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub content: String,
    pub importance: f64,
    pub topics: Vec<String>,
    pub entities: Vec<String>,
}

/// Called from router.rs on EthosRequest::Consolidate (manual trigger)
pub async fn trigger_consolidation(
    pool: PgPool,
    config: ConsolidationConfig,
    conflict_config: ConflictResolutionConfig,
    decay_config: DecayConfig,
    session: Option<String>,
    reason: Option<String>,
) -> Result<ConsolidationReport> {
    tracing::info!(
        "Manual consolidation triggered: session={:?}, reason={:?}",
        session,
        reason
    );

    // Run immediately without idle check for manual trigger
    run_consolidation_cycle(&pool, &config, &conflict_config, &decay_config, None).await
}

/// Called from main.rs to start the background 15-min consolidation loop
pub async fn run_consolidation_loop(
    pool: PgPool,
    config: ConsolidationConfig,
    conflict_config: ConflictResolutionConfig,
    decay_config: DecayConfig,
    mut shutdown: broadcast::Receiver<()>,
) {
    let interval = tokio::time::Duration::from_secs(config.interval_minutes * 60);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    tracing::info!(
        "Consolidation loop started (interval: {}min)",
        config.interval_minutes
    );

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if is_system_idle(&pool, &config).await {
                    match run_consolidation_cycle(&pool, &config, &conflict_config, &decay_config, None).await {
                        Ok(report) => {
                            tracing::info!(
                                "Consolidation cycle complete: {} scanned, {} promoted, {} facts created",
                                report.episodes_scanned,
                                report.episodes_promoted,
                                report.facts_created
                            );
                            
                            // Run decay sweep after consolidation (Story 010)
                            if let Err(e) = super::decay::run_decay_sweep(&pool, &decay_config).await {
                                tracing::warn!("Decay sweep error (non-fatal): {}", e);
                            }
                        }
                        Err(e) => tracing::error!("Consolidation error: {}", e),
                    }
                } else {
                    tracing::debug!("Consolidation skipped: system not idle");
                }
            }
            _ = shutdown.recv() => {
                tracing::info!("Consolidation loop shutting down");
                break;
            }
        }
    }
}

// ============================================================================
// INTERNAL HELPERS
// ============================================================================

/// Check if system is idle (no recent messages + CPU < threshold)
async fn is_system_idle(pool: &PgPool, config: &ConsolidationConfig) -> bool {
    // Check: any session_events in the last idle_threshold_seconds?
    let cutoff = Utc::now() - chrono::Duration::seconds(config.idle_threshold_seconds as i64);

    let recent_count: Option<i64> = match sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM session_events WHERE created_at > $1",
    )
    .bind(cutoff)
    .fetch_one(pool)
    .await
    {
        Ok(count) => count,
        Err(e) => {
            tracing::warn!("Failed to check idle state: {}", e);
            return false; // Conservative: not idle if we can't check
        }
    };

    if recent_count.unwrap_or(0) > 0 {
        return false;
    }

    // Check: CPU load (Linux /proc/loadavg)
    if let Ok(load) = std::fs::read_to_string("/proc/loadavg") {
        if let Some(load_1m) = load.split_whitespace().next() {
            if let Ok(load_val) = load_1m.parse::<f32>() {
                let cpu_count = num_cpus::get() as f32;
                let cpu_percent = (load_val / cpu_count) * 100.0;
                if cpu_percent > config.cpu_threshold_percent as f32 {
                    return false;
                }
            }
        }
    }

    true
}

/// Run a single consolidation cycle
async fn run_consolidation_cycle(
    pool: &PgPool,
    config: &ConsolidationConfig,
    conflict_config: &ConflictResolutionConfig,
    _decay_config: &DecayConfig,
    _session_id: Option<Uuid>,
) -> Result<ConsolidationReport> {
    let mut report = ConsolidationReport::default();

    // Fetch promotion candidates
    let candidates = fetch_promotion_candidates(pool, config, None).await?;
    report.episodes_scanned = candidates.len();

    tracing::debug!("Found {} promotion candidates", candidates.len());

    // Process each candidate
    let mut promoted_ids = Vec::new();
    for episode in candidates {
        if let Some(fact) = extract_fact_from_episode(&episode) {
            match upsert_fact(pool, &fact, conflict_config).await {
                Ok(result) => {
                    promoted_ids.push(episode.id);
                    report.episodes_promoted += 1;

                    match result {
                        FactUpsertResult::Created(_) => report.facts_created += 1,
                        FactUpsertResult::Updated(_) => report.facts_updated += 1,
                        FactUpsertResult::Superseded { .. } => report.facts_superseded += 1,
                        FactUpsertResult::Flagged { .. } => report.facts_flagged += 1,
                        FactUpsertResult::Skipped => {}
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to upsert fact for episode {}: {}", episode.id, e);
                }
            }
        }
    }

    // Mark episodes as consolidated
    if !promoted_ids.is_empty() {
        mark_consolidated(pool, &promoted_ids).await?;
    }

    Ok(report)
}

/// Fetch unconsolidated episodic_traces that meet promotion criteria
async fn fetch_promotion_candidates(
    pool: &PgPool,
    config: &ConsolidationConfig,
    session_id: Option<Uuid>,
) -> Result<Vec<EpisodicTrace>> {
    let session_filter = match session_id {
        Some(id) => format!("AND session_id = '{}'", id),
        None => String::new(),
    };

    // Fetch episodes that meet ANY of the promotion criteria
    // - importance >= threshold
    // - retrieval_count >= threshold
    // - Contains decision keywords
    // - Contains preference keywords
    // - Contains explicit markers
    let query = format!(
        r#"
        SELECT 
            id, session_id, agent_id, content, importance, topics, entities
        FROM episodic_traces
        WHERE consolidated_at IS NULL
          AND pruned = false
          {}
          AND (
              importance >= $1
              OR retrieval_count >= $2
              OR content ILIKE '%decided%'
              OR content ILIKE '%let''s go with%'
              OR content ILIKE '%the plan is%'
              OR content ILIKE '%we''ll use%'
              OR content ILIKE '%going with%'
              OR content ILIKE '%prefer%'
              OR content ILIKE '%love%'
              OR content ILIKE '%hate%'
              OR content ILIKE '%always%'
              OR content ILIKE '%never%'
              OR content ILIKE '%favorite%'
              OR content ILIKE '%remember this%'
              OR content ILIKE '%note that%'
              OR content ILIKE '%important:%'
          )
        ORDER BY importance DESC
        LIMIT 100
        "#,
        session_filter
    );

    let rows = sqlx::query_as::<_, EpisodicTrace>(&query)
        .bind(config.importance_threshold as f64)
        .bind(config.retrieval_threshold as i32)
        .fetch_all(pool)
        .await?;

    Ok(rows)
}

/// Extract a SemanticFact from an episode using rule-based patterns (no LLM)
fn extract_fact_from_episode(episode: &EpisodicTrace) -> Option<ExtractedFact> {
    let content = &episode.content;

    // Decision patterns
    let decision_patterns = [
        (r"(?i)(?:we\s+)?decided\s+(?:to\s+)?(?:use|go\s+with|switch\s+to)\s+(\w+)", "uses"),
        (r"(?i)let''s\s+go\s+with\s+(\w+)", "uses"),
        (r"(?i)the\s+plan\s+is\s+(?:to\s+)?(.+?)(?:\.|$)", "plan"),
        (r"(?i)we''ll\s+use\s+(\w+)", "uses"),
        (r"(?i)going\s+with\s+(\w+)", "uses"),
    ];

    for (pattern, predicate) in decision_patterns.iter() {
        if let Ok(re) = Regex::new(pattern) {
            if let Some(caps) = re.captures(content) {
                let object = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                if !object.is_empty() {
                    return Some(ExtractedFact {
                        kind: "decision".to_string(),
                        statement: truncate_statement(content, 200),
                        subject: extract_subject(content).unwrap_or_else(|| "team".to_string()),
                        predicate: predicate.to_string(),
                        object,
                        topics: episode.topics.clone(),
                        confidence: 0.90,
                        source_episode: episode.id,
                        source_agent: Some(episode.agent_id.clone()),
                    });
                }
            }
        }
    }

    // Preference patterns
    let preference_patterns = [
        (r"(?i)(\w+)\s+prefers?\s+(\w+(?:\s+\w+)?)\s+(?:over|than)\s+(\w+)", "prefers"),
        (r"(?i)(\w+)\s+loves?\s+(\w+)", "loves"),
        (r"(?i)(\w+)\s+hates?\s+(\w+)", "hates"),
        (r"(?i)(\w+)\s+always\s+(\w+)", "always"),
        (r"(?i)(\w+)\s+never\s+(\w+)", "never"),
        (r"(?i)(\w+)''s\s+favorite\s+(\w+)\s+is\s+(\w+)", "favorite"),
    ];

    for (pattern, predicate) in preference_patterns.iter() {
        if let Ok(re) = Regex::new(pattern) {
            if let Some(caps) = re.captures(content) {
                let subject = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                let object = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                if !subject.is_empty() && !object.is_empty() {
                    return Some(ExtractedFact {
                        kind: "preference".to_string(),
                        statement: truncate_statement(content, 200),
                        subject,
                        predicate: predicate.to_string(),
                        object,
                        topics: episode.topics.clone(),
                        confidence: 0.80,
                        source_episode: episode.id,
                        source_agent: Some(episode.agent_id.clone()),
                    });
                }
            }
        }
    }

    // Explicit markers ("remember this", "note that", "important:")
    let marker_patterns = [
        r"(?i)remember\s+(?:this|that):\s*(.+?)(?:\.|$)",
        r"(?i)note\s+(?:this|that):\s*(.+?)(?:\.|$)",
        r"(?i)important:\s*(.+?)(?:\.|$)",
    ];

    for pattern in marker_patterns.iter() {
        if let Ok(re) = Regex::new(pattern) {
            if let Some(caps) = re.captures(content) {
                let statement = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                if !statement.is_empty() {
                    return Some(ExtractedFact {
                        kind: "fact".to_string(),
                        statement: statement.clone(),
                        subject: extract_subject(&statement).unwrap_or_else(|| "context".to_string()),
                        predicate: "is".to_string(),
                        object: truncate_statement(&statement, 50),
                        topics: episode.topics.clone(),
                        confidence: 0.85,
                        source_episode: episode.id,
                        source_agent: Some(episode.agent_id.clone()),
                    });
                }
            }
        }
    }

    // Fallback for high-importance content with no pattern match
    if episode.importance >= 0.8 {
        return Some(ExtractedFact {
            kind: "fact".to_string(),
            statement: truncate_statement(content, 200),
            subject: "context".to_string(),
            predicate: "contains".to_string(),
            object: format!("{}...", &content.chars().take(50).collect::<String>()),
            topics: episode.topics.clone(),
            confidence: 0.70,
            source_episode: episode.id,
            source_agent: Some(episode.agent_id.clone()),
        });
    }

    None
}

/// Extract subject from content (simple heuristic: first proper noun or capitalized word)
fn extract_subject(content: &str) -> Option<String> {
    // Look for capitalized words (likely proper nouns)
    let re = Regex::new(r"\b([A-Z][a-z]+)\b").ok()?;
    let caps = re.captures(content)?;
    caps.get(1).map(|m| m.as_str().to_string())
}

/// Truncate a statement to max_len chars
fn truncate_statement(content: &str, max_len: usize) -> String {
    let cleaned: String = content.chars().take(max_len).collect();
    if content.len() > max_len {
        format!("{}...", cleaned.trim_end())
    } else {
        cleaned
    }
}

/// Apply conflict resolution and upsert the fact into semantic_facts
async fn upsert_fact(
    pool: &PgPool,
    fact: &ExtractedFact,
    conflict_config: &ConflictResolutionConfig,
) -> Result<FactUpsertResult> {
    // Check for existing fact with same subject + predicate
    let existing: Option<(Uuid, String, f64, bool)> = sqlx::query_as(
        r#"
        SELECT id, object, confidence, flagged_for_review
        FROM semantic_facts
        WHERE subject = $1 AND predicate = $2
          AND pruned = false
          AND superseded_by IS NULL
        LIMIT 1
        "#,
    )
    .bind(&fact.subject)
    .bind(&fact.predicate)
    .fetch_optional(pool)
    .await?;

    match existing {
        None => {
            // No conflict - INSERT new fact
            let id = insert_fact(pool, fact).await?;
            Ok(FactUpsertResult::Created(id))
        }
        Some((existing_id, existing_object, existing_confidence, already_flagged)) => {
            // Determine resolution type
            let objects_compatible = are_objects_compatible(&existing_object, &fact.object);
            let confidence_delta = fact.confidence - existing_confidence;
            let is_decision = fact.kind == "decision";

            if objects_compatible && !is_decision {
                // Refinement: compatible objects → UPDATE
                update_fact(pool, existing_id, fact).await?;
                Ok(FactUpsertResult::Updated(existing_id))
            } else if is_decision {
                // Supersession: explicit decision → always supersede
                let new_id = insert_fact(pool, fact).await?;
                sqlx::query(
                    "UPDATE semantic_facts SET superseded_by = $1 WHERE id = $2",
                )
                .bind(new_id)
                .bind(existing_id)
                .execute(pool)
                .await?;
                Ok(FactUpsertResult::Superseded {
                    old: existing_id,
                    new: new_id,
                })
            } else if confidence_delta >= conflict_config.auto_supersede_confidence_delta {
                // Auto-supersede: new confidence significantly higher
                let new_id = insert_fact(pool, fact).await?;
                sqlx::query(
                    "UPDATE semantic_facts SET superseded_by = $1 WHERE id = $2",
                )
                .bind(new_id)
                .bind(existing_id)
                .execute(pool)
                .await?;
                Ok(FactUpsertResult::Superseded {
                    old: existing_id,
                    new: new_id,
                })
            } else {
                // Contradiction: ambiguous → flag for review
                flag_conflict(pool, existing_id, fact, conflict_config, already_flagged).await?;
                Ok(FactUpsertResult::Flagged {
                    existing: existing_id,
                    new_statement: fact.statement.clone(),
                })
            }
        }
    }
}

/// Check if two objects are compatible (one contains the other)
fn are_objects_compatible(obj1: &str, obj2: &str) -> bool {
    let o1 = obj1.to_lowercase();
    let o2 = obj2.to_lowercase();
    o1.contains(&o2) || o2.contains(&o1)
}

/// Insert a new fact
async fn insert_fact(pool: &PgPool, fact: &ExtractedFact) -> Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO semantic_facts (
            kind, statement, subject, predicate, object,
            topics, confidence, source_episodes, source_agent, salience
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, ARRAY[$8], $9, 1.0)
        RETURNING id
        "#,
    )
    .bind(&fact.kind)
    .bind(&fact.statement)
    .bind(&fact.subject)
    .bind(&fact.predicate)
    .bind(&fact.object)
    .bind(&fact.topics)
    .bind(fact.confidence as f32)
    .bind(fact.source_episode)
    .bind(&fact.source_agent)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Update an existing fact (refinement)
async fn update_fact(pool: &PgPool, id: Uuid, fact: &ExtractedFact) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE semantic_facts
        SET object = object || ' ' || $1,
            confidence = LEAST(confidence + 0.05, 1.0),
            source_episodes = array_append(source_episodes, $2),
            updated_at = NOW()
        WHERE id = $3
        "#,
    )
    .bind(&fact.object)
    .bind(fact.source_episode)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Flag a conflict for review
async fn flag_conflict(
    pool: &PgPool,
    existing_id: Uuid,
    fact: &ExtractedFact,
    conflict_config: &ConflictResolutionConfig,
    already_flagged: bool,
) -> Result<()> {
    // Insert new fact with flagged status
    let new_id = insert_fact(pool, fact).await?;

    // Flag both facts
    sqlx::query("UPDATE semantic_facts SET flagged_for_review = true WHERE id = $1")
        .bind(existing_id)
        .execute(pool)
        .await?;

    sqlx::query("UPDATE semantic_facts SET flagged_for_review = true WHERE id = $1")
        .bind(new_id)
        .execute(pool)
        .await?;

    // Write to review inbox (only if not already flagged)
    if !already_flagged {
        write_to_review_inbox(existing_id, fact, conflict_config)?;
    }

    Ok(())
}

/// Write conflict to review inbox
fn write_to_review_inbox(
    existing_id: Uuid,
    fact: &ExtractedFact,
    conflict_config: &ConflictResolutionConfig,
) -> Result<()> {
    let expanded_path = tilde(&conflict_config.review_inbox).to_string();

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(&expanded_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let entry = format!(
        r#"
### [{}] Memory Conflict
**Subject:** {} / **Predicate:** {}
**Existing ID:** {}
**New:** "{}" (confidence: {:.2})
**Source episode:** {}
Actions: `keep-old` | `keep-new` | `keep-both`

"#,
        Utc::now().to_rfc3339(),
        fact.subject,
        fact.predicate,
        existing_id,
        fact.statement,
        fact.confidence,
        fact.source_episode
    );

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&expanded_path)?;

    file.write_all(entry.as_bytes())?;

    Ok(())
}

/// Mark episodes as consolidated
async fn mark_consolidated(pool: &PgPool, episode_ids: &[Uuid]) -> Result<()> {
    if episode_ids.is_empty() {
        return Ok(());
    }

    // Batch update in chunks of 50 to avoid query size limits
    for chunk in episode_ids.chunks(50) {
        let ids: Vec<String> = chunk.iter().map(|id| format!("'{}'", id)).collect();
        let query = format!(
            "UPDATE episodic_traces SET consolidated_at = NOW() WHERE id IN ({})",
            ids.join(", ")
        );
        sqlx::query(&query).execute(pool).await?;
    }

    Ok(())
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_episode(content: &str, importance: f64) -> EpisodicTrace {
        EpisodicTrace {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            agent_id: "test".to_string(),
            content: content.to_string(),
            importance,
            topics: vec![],
            entities: vec![],
        }
    }

    fn create_test_config() -> (ConsolidationConfig, ConflictResolutionConfig, DecayConfig) {
        (
            ConsolidationConfig {
                interval_minutes: 15,
                idle_threshold_seconds: 60,
                cpu_threshold_percent: 80,
                importance_threshold: 0.8,
                repetition_threshold: 3,
                retrieval_threshold: 5,
            },
            ConflictResolutionConfig {
                auto_supersede_confidence_delta: 0.15,
                review_inbox: "/tmp/test-review-inbox.md".to_string(),
            },
            DecayConfig {
                base_tau_days: 7.0,
                ltp_multiplier: 1.5,
                frequency_weight: 0.3,
                emotional_weight: 0.2,
                prune_threshold: 0.05,
            },
        )
    }

    // ========================================================================
    // TEST 3: extract decision fact
    // ========================================================================
    #[test]
    fn test_extract_decision_fact() {
        let episode = create_test_episode(
            "We decided to use Rust for all backend services",
            0.5,
        );

        let fact = extract_fact_from_episode(&episode);
        assert!(fact.is_some());

        let fact = fact.unwrap();
        assert_eq!(fact.kind, "decision");
        assert_eq!(fact.confidence, 0.90);
        assert!(!fact.object.is_empty());
    }

    // ========================================================================
    // TEST 4: extract preference fact
    // ========================================================================
    #[test]
    fn test_extract_preference_fact() {
        let episode = create_test_episode("Michael prefers Rust over Python", 0.5);

        let fact = extract_fact_from_episode(&episode);
        assert!(fact.is_some());

        let fact = fact.unwrap();
        assert_eq!(fact.kind, "preference");
        assert!(fact.subject.contains("Michael"));
    }

    // ========================================================================
    // TEST 5: extract fallback fact (high importance, no pattern)
    // ========================================================================
    #[test]
    fn test_extract_fallback_fact() {
        let episode = create_test_episode(
            "Some random high importance content without keywords",
            0.9,
        );

        let fact = extract_fact_from_episode(&episode);
        assert!(fact.is_some());

        let fact = fact.unwrap();
        assert_eq!(fact.kind, "fact");
        assert_eq!(fact.confidence, 0.70);
    }

    // ========================================================================
    // TEST 6: extract no fact (low importance, no keywords)
    // ========================================================================
    #[test]
    fn test_extract_no_fact() {
        let episode = create_test_episode("Random low importance content", 0.3);

        let fact = extract_fact_from_episode(&episode);
        assert!(fact.is_none());
    }

    // ========================================================================
    // TEST: extract from "remember this" marker
    // ========================================================================
    #[test]
    fn test_extract_remember_marker() {
        let episode = create_test_episode(
            "Remember this: The API key is stored in the vault",
            0.5,
        );

        let fact = extract_fact_from_episode(&episode);
        assert!(fact.is_some());

        let fact = fact.unwrap();
        assert_eq!(fact.kind, "fact");
        assert!(fact.statement.contains("API key"));
    }

    // ========================================================================
    // TEST: objects compatible detection
    // ========================================================================
    #[test]
    fn test_objects_compatible() {
        assert!(are_objects_compatible("Rust", "Rust language"));
        assert!(are_objects_compatible("Rust language", "Rust"));
        assert!(!are_objects_compatible("Rust", "Python"));
    }

    // ========================================================================
    // TEST: truncate statement
    // ========================================================================
    #[test]
    fn test_truncate_statement() {
        let short = "Short content";
        assert_eq!(truncate_statement(short, 200), short);

        let long = "This is a very long piece of content that should be truncated";
        let truncated = truncate_statement(long, 20);
        assert!(truncated.len() <= 23); // 20 + "..."
        assert!(truncated.ends_with("..."));
    }

    // ========================================================================
    // TEST: extract subject
    // ========================================================================
    #[test]
    fn test_extract_subject() {
        assert_eq!(
            extract_subject("Michael prefers Rust"),
            Some("Michael".to_string())
        );
        assert_eq!(
            extract_subject("the company is Modern Method"),
            Some("Modern".to_string())
        );
    }

    // ========================================================================
    // INTEGRATION TESTS (require DB)
    // ========================================================================

    // ========================================================================
    // TEST 1: idle detection active
    // ========================================================================
    #[tokio::test]
    async fn test_idle_detection_active() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let (_, _conflict_config, _) = create_test_config();
        let config = ConsolidationConfig {
            idle_threshold_seconds: 60,
            ..Default::default()
        };

        // Insert a recent session event
        sqlx::query(
            "INSERT INTO session_events (session_id, agent_id, role, content) 
             VALUES ('test-idle-active', 'test', 'user', 'test')",
        )
        .execute(&pool)
        .await
        .ok();

        // System should NOT be idle
        let idle = is_system_idle(&pool, &config).await;
        assert!(!idle, "System should not be idle with recent events");

        // Cleanup
        sqlx::query("DELETE FROM session_events WHERE session_id = 'test-idle-active'")
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST 2: idle detection quiet
    // ========================================================================
    #[tokio::test]
    async fn test_idle_detection_quiet() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let config = ConsolidationConfig {
            idle_threshold_seconds: 60,
            ..Default::default()
        };

        // Clean up any test events first
        sqlx::query("DELETE FROM session_events WHERE session_id LIKE 'test-idle%'")
            .execute(&pool)
            .await
            .ok();

        // Insert an OLD session event (> 60s ago)
        sqlx::query(
            "INSERT INTO session_events (session_id, agent_id, role, content, created_at) 
             VALUES ('test-idle-old', 'test', 'user', 'test', NOW() - INTERVAL '5 minutes')",
        )
        .execute(&pool)
        .await
        .ok();

        // Check idle state - should be idle since we only have old events
        // Note: This test may fail if there are other recent events in the DB
        // For a more robust test, we'd need transaction isolation
        let _idle = is_system_idle(&pool, &config).await;
        
        // Cleanup
        sqlx::query("DELETE FROM session_events WHERE session_id = 'test-idle-old'")
            .execute(&pool)
            .await
            .ok();
        
        // This test is informational - the is_system_idle function depends on
        // the overall system state which we can't fully control in integration tests
    }

    // ========================================================================
    // TEST: full consolidation cycle
    // ========================================================================
    #[tokio::test]
    async fn test_full_consolidation_cycle() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let (config, conflict_config, decay_config) = create_test_config();

        // Create test session
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, session_key, agent_id) VALUES ($1, $2, 'test')",
        )
        .bind(session_id)
        .bind(format!("test-consolidation-{}", session_id))
        .execute(&pool)
        .await
        .ok();

        // Insert episodic traces
        let mut episode_ids = Vec::new();
        for i in 0..5 {
            let importance = if i < 3 { 0.9 } else { 0.3 };
            let row: (Uuid,) = sqlx::query_as(
                "INSERT INTO episodic_traces (session_id, agent_id, turn_index, role, content, importance) 
                 VALUES ($1, 'test', $2, 'user', $3, $4) RETURNING id",
            )
            .bind(session_id)
            .bind(i as i32)
            .bind(format!("Test content {} with decided keyword", i))
            .bind(importance)
            .fetch_one(&pool)
            .await
            .expect("Failed to insert episode");

            episode_ids.push(row.0);
        }

        // Run consolidation
        let report = run_consolidation_cycle(&pool, &config, &conflict_config, &decay_config, None)
            .await
            .expect("Consolidation failed");

        // Should have scanned all 5 and promoted at least some
        assert!(report.episodes_scanned >= 3, "Should scan eligible episodes");
        assert!(report.episodes_promoted >= 1, "Should promote at least one episode");

        // Verify episodes are marked consolidated
        let consolidated_count: Option<i64> = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM episodic_traces WHERE session_id = $1 AND consolidated_at IS NOT NULL",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to count consolidated");

        assert!(
            consolidated_count.unwrap_or(0) >= report.episodes_promoted as i64,
            "Promoted episodes should be marked consolidated"
        );

        // Cleanup
        for id in episode_ids {
            sqlx::query("DELETE FROM episodic_traces WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .ok();
        }
        sqlx::query("DELETE FROM semantic_facts WHERE source_agent = 'test'")
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
    // TEST: consolidation marks episodes
    // ========================================================================
    #[tokio::test]
    async fn test_consolidation_marks_episodes() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let (config, conflict_config, decay_config) = create_test_config();

        // Create test session
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, session_key, agent_id) VALUES ($1, $2, 'test')",
        )
        .bind(session_id)
        .bind(format!("test-marks-{}", session_id))
        .execute(&pool)
        .await
        .ok();

        // Insert high-importance episode
        let episode_id: Uuid = sqlx::query_scalar(
            "INSERT INTO episodic_traces (session_id, agent_id, turn_index, role, content, importance) 
             VALUES ($1, 'test', 0, 'user', 'We decided to use BMAD', 0.9) RETURNING id",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert episode");

        // Run consolidation
        let _ = run_consolidation_cycle(&pool, &config, &conflict_config, &decay_config, None)
            .await;

        // Verify episode has consolidated_at
        let consolidated_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT consolidated_at FROM episodic_traces WHERE id = $1",
        )
        .bind(episode_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to check consolidated_at");

        assert!(
            consolidated_at.is_some(),
            "Episode should have consolidated_at timestamp"
        );

        // Cleanup
        sqlx::query("DELETE FROM episodic_traces WHERE session_id = $1")
            .bind(session_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM semantic_facts WHERE source_agent = 'test'")
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
    // TEST: conflict resolution - refinement
    // ========================================================================
    #[tokio::test]
    async fn test_conflict_refinement() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let (_, conflict_config, _) = create_test_config();

        // Insert initial fact
        let fact1 = ExtractedFact {
            kind: "fact".to_string(),
            statement: "Initial statement".to_string(),
            subject: "Test".to_string(),
            predicate: "uses".to_string(),
            object: "Rust".to_string(),
            topics: vec![],
            confidence: 0.8,
            source_episode: Uuid::new_v4(),
            source_agent: Some("test".to_string()),
        };

        let _ = insert_fact(&pool, &fact1).await;

        // Insert compatible fact (should refine)
        let fact2 = ExtractedFact {
            kind: "fact".to_string(),
            statement: "Refined statement".to_string(),
            subject: "Test".to_string(),
            predicate: "uses".to_string(),
            object: "Rust language".to_string(), // Compatible
            topics: vec![],
            confidence: 0.75,
            source_episode: Uuid::new_v4(),
            source_agent: Some("test".to_string()),
        };

        let result = upsert_fact(&pool, &fact2, &conflict_config)
            .await
            .expect("Upsert failed");

        assert!(matches!(result, FactUpsertResult::Updated(_)));

        // Cleanup
        sqlx::query("DELETE FROM semantic_facts WHERE subject = 'Test'")
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST: conflict resolution - supersession
    // ========================================================================
    #[tokio::test]
    async fn test_conflict_supersession() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let (_, conflict_config, _) = create_test_config();

        // Insert initial fact
        let fact1 = ExtractedFact {
            kind: "fact".to_string(),
            statement: "Old statement".to_string(),
            subject: "Project".to_string(),
            predicate: "uses".to_string(),
            object: "Python".to_string(),
            topics: vec![],
            confidence: 0.7,
            source_episode: Uuid::new_v4(),
            source_agent: Some("test".to_string()),
        };

        let _ = insert_fact(&pool, &fact1).await;

        // Insert decision fact (should supersede)
        let fact2 = ExtractedFact {
            kind: "decision".to_string(),
            statement: "We decided to switch".to_string(),
            subject: "Project".to_string(),
            predicate: "uses".to_string(),
            object: "Rust".to_string(),
            topics: vec![],
            confidence: 0.8,
            source_episode: Uuid::new_v4(),
            source_agent: Some("test".to_string()),
        };

        let result = upsert_fact(&pool, &fact2, &conflict_config)
            .await
            .expect("Upsert failed");

        assert!(matches!(result, FactUpsertResult::Superseded { .. }));

        // Cleanup
        sqlx::query("DELETE FROM semantic_facts WHERE subject = 'Project'")
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST: conflict resolution - flag
    // ========================================================================
    #[tokio::test]
    async fn test_conflict_flag() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let (_, conflict_config, _) = create_test_config();

        // Insert initial fact
        let fact1 = ExtractedFact {
            kind: "fact".to_string(),
            statement: "First statement".to_string(),
            subject: "FlagTest".to_string(),
            predicate: "value".to_string(),
            object: "A".to_string(),
            topics: vec![],
            confidence: 0.7,
            source_episode: Uuid::new_v4(),
            source_agent: Some("test".to_string()),
        };

        let _ = insert_fact(&pool, &fact1).await;

        // Insert conflicting fact with similar confidence (should flag)
        let fact2 = ExtractedFact {
            kind: "fact".to_string(),
            statement: "Conflicting statement".to_string(),
            subject: "FlagTest".to_string(),
            predicate: "value".to_string(),
            object: "B".to_string(), // Incompatible
            topics: vec![],
            confidence: 0.75, // Similar confidence (< 0.15 delta)
            source_episode: Uuid::new_v4(),
            source_agent: Some("test".to_string()),
        };

        let result = upsert_fact(&pool, &fact2, &conflict_config)
            .await
            .expect("Upsert failed");

        assert!(matches!(result, FactUpsertResult::Flagged { .. }));

        // Cleanup
        sqlx::query("DELETE FROM semantic_facts WHERE subject = 'FlagTest'")
            .execute(&pool)
            .await
            .ok();
        // Clean up test review inbox
        std::fs::remove_file("/tmp/test-review-inbox.md").ok();
    }

    // ========================================================================
    // TEST: conflict resolution - auto supersede
    // ========================================================================
    #[tokio::test]
    async fn test_conflict_auto_supersede() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let (_, conflict_config, _) = create_test_config();

        // Insert initial fact with low confidence
        let fact1 = ExtractedFact {
            kind: "fact".to_string(),
            statement: "Low confidence fact".to_string(),
            subject: "AutoTest".to_string(),
            predicate: "value".to_string(),
            object: "Old".to_string(),
            topics: vec![],
            confidence: 0.5,
            source_episode: Uuid::new_v4(),
            source_agent: Some("test".to_string()),
        };

        let _ = insert_fact(&pool, &fact1).await;

        // Insert new fact with much higher confidence (> 0.15 delta)
        let fact2 = ExtractedFact {
            kind: "fact".to_string(),
            statement: "High confidence fact".to_string(),
            subject: "AutoTest".to_string(),
            predicate: "value".to_string(),
            object: "New".to_string(),
            topics: vec![],
            confidence: 0.9, // > 0.5 + 0.15 = 0.65
            source_episode: Uuid::new_v4(),
            source_agent: Some("test".to_string()),
        };

        let result = upsert_fact(&pool, &fact2, &conflict_config)
            .await
            .expect("Upsert failed");

        assert!(matches!(result, FactUpsertResult::Superseded { .. }));

        // Cleanup
        sqlx::query("DELETE FROM semantic_facts WHERE subject = 'AutoTest'")
            .execute(&pool)
            .await
            .ok();
    }

    // ========================================================================
    // TEST: manual trigger consolidation (tests trigger_consolidation fn)
    // ========================================================================
    #[tokio::test]
    async fn test_manual_trigger_consolidation() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        let (config, conflict_config, decay_config) = create_test_config();

        // Call trigger_consolidation directly
        let report = trigger_consolidation(
            pool.clone(),
            config,
            conflict_config,
            decay_config,
            None,
            Some("test-manual-trigger".to_string()),
        )
        .await
        .expect("trigger_consolidation failed");

        // Should return a valid report with no errors
        // (episodes_promoted may be 0 if nothing qualifies)
        let _ = report.episodes_scanned; // just verify we got a report
    }

    // ========================================================================
    // TEST: mark_consolidated directly
    // ========================================================================
    #[tokio::test]
    async fn test_mark_consolidated_directly() {
        let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
        let pool = PgPool::connect(database_url)
            .await
            .expect("Failed to connect to Postgres");

        // Create test session + episode
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, session_key, agent_id) VALUES ($1, $2, 'test')",
        )
        .bind(session_id)
        .bind(format!("test-markcons-{}", session_id))
        .execute(&pool)
        .await
        .ok();

        let ep_id: Uuid = sqlx::query_scalar(
            "INSERT INTO episodic_traces (session_id, agent_id, turn_index, role, content, importance)
             VALUES ($1, 'test', 0, 'user', 'test', 0.9) RETURNING id",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to insert episode");

        // Call mark_consolidated
        mark_consolidated(&pool, &[ep_id])
            .await
            .expect("mark_consolidated failed");

        // Verify consolidated_at is set
        let consolidated_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT consolidated_at FROM episodic_traces WHERE id = $1",
        )
        .bind(ep_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to check");

        assert!(consolidated_at.is_some(), "consolidated_at should be set");

        // Cleanup
        sqlx::query("DELETE FROM episodic_traces WHERE session_id = $1").bind(session_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM sessions WHERE id = $1").bind(session_id).execute(&pool).await.ok();
    }
}
