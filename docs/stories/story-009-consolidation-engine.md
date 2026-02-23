# Story 009 — Consolidation Engine (Episodic → Semantic)

**Status:** Ready for Implementation  
**Assigned:** Forge  
**Reviewer:** Sage  
**Priority:** P0 — Closes Ethos v1

---

## Overview

Replace the stub `consolidate.rs` with a real consolidation engine. This is the Animus DMN (Default Mode Network) simplified for standalone deployment: a background Tokio task that periodically promotes high-salience episodic memories into structured semantic facts.

**What gets built:**
1. Background consolidation task (runs every 15 minutes, gates on idle detection)
2. Episode scanner (finds unconsolidated episodes that meet promotion criteria)
3. Fact extractor (keyword/pattern-based extraction from episode content — **no LLM call**, rule-based for v1)
4. Conflict resolver (tiered resolution: refinement → update → supersession → flag)
5. Wire the background task into `main.rs`

---

## Files to Create / Modify

| File | Action | Description |
|------|--------|-------------|
| `ethos-server/src/subsystems/consolidate.rs` | **Replace** | Full implementation (replace stub) |
| `ethos-server/src/main.rs` | **Modify** | Spawn background consolidation task |
| `ethos-core/src/config.rs` | **Modify** | Add `ConflictResolutionConfig` and `DecayConfig` structs |
| `ethos.toml` | **No change** | Already has `[consolidation]` and `[conflict_resolution]` sections |
| `ethos-server/tests/consolidation_integration.rs` | **Create** | Integration tests |

---

## Implementation

### 1. `consolidate.rs` — Replace Stub

```rust
// ethos-server/src/subsystems/consolidate.rs
//
// Consolidation Engine — episodic → semantic promotion
//
// Implements the Animus DMN (Default Mode Network) for standalone Ethos.
// Runs every 15 minutes when system is idle.

use anyhow::Result;
use ethos_core::config::{ConsolidationConfig, ConflictResolutionConfig};
use sqlx::PgPool;
use uuid::Uuid;
use chrono::Utc;
```

**Public API (functions to implement):**

```rust
/// Called from router.rs on EthosRequest::Consolidate (manual trigger)
pub async fn trigger_consolidation(
    session: Option<String>,
    reason: Option<String>,
) -> Result<ConsolidationReport>

/// Called from main.rs to start the background 15-min consolidation loop
pub async fn run_consolidation_loop(
    pool: PgPool,
    config: ConsolidationConfig,
    conflict_config: ConflictResolutionConfig,
    mut shutdown: broadcast::Receiver<()>,
)
```

**Internal helpers to implement:**

```rust
/// Check if system is idle (no recent messages + CPU < threshold)
async fn is_system_idle(pool: &PgPool, config: &ConsolidationConfig) -> bool

/// Fetch unconsolidated episodic_traces that meet promotion criteria
async fn fetch_promotion_candidates(
    pool: &PgPool,
    config: &ConsolidationConfig,
    session_id: Option<Uuid>,
) -> Result<Vec<EpisodicTrace>>

/// Extract a SemanticFact from an episode using rule-based patterns (no LLM)
fn extract_fact_from_episode(episode: &EpisodicTrace) -> Option<ExtractedFact>

/// Apply conflict resolution and upsert the fact into semantic_facts
async fn upsert_fact(
    pool: &PgPool,
    fact: ExtractedFact,
    conflict_config: &ConflictResolutionConfig,
) -> Result<FactUpsertResult>

/// Mark episodes as consolidated
async fn mark_consolidated(pool: &PgPool, episode_ids: &[Uuid]) -> Result<()>
```

### 2. Structs

```rust
pub struct ConsolidationReport {
    pub episodes_scanned: usize,
    pub episodes_promoted: usize,
    pub facts_created: usize,
    pub facts_updated: usize,
    pub facts_superseded: usize,
    pub facts_flagged: usize,
    pub skipped_idle: bool,
}

pub struct ExtractedFact {
    pub kind: String,          // "preference", "decision", "fact", "entity"
    pub statement: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub topics: Vec<String>,
    pub confidence: f64,
    pub source_episode: Uuid,
    pub source_agent: Option<String>,
}

pub enum FactUpsertResult {
    Created(Uuid),
    Updated(Uuid),
    Superseded { old: Uuid, new: Uuid },
    Flagged { existing: Uuid, new_statement: String },
    Skipped,
}
```

### 3. Promotion Criteria

Fetch unconsolidated episodes (`consolidated_at IS NULL`) where **any** of:
- `importance >= config.importance_threshold` (default 0.8)
- `retrieval_count >= config.retrieval_threshold` (default 5)
- Content contains decision keywords: "decided", "let's go with", "the plan is", "we'll use", "going with"
- Content contains preference keywords: "prefer", "love", "hate", "always", "never", "favorite"
- Content contains explicit markers: "remember this", "note that", "important:"

Limit to 100 episodes per consolidation cycle to avoid overloading.

### 4. Rule-Based Fact Extraction

**No LLM call for v1.** Use pattern matching and heuristics:

```
Decision pattern:
  Input: "We decided to use BMAD Method for all Modern Method projects"
  → kind: "decision"
  → subject: "Modern Method" (or extract first proper noun)
  → predicate: "uses_dev_methodology"  
  → object: extracted noun after "use/go with/switch to"
  → confidence: 0.90

Preference pattern:
  Input: "Michael prefers Telegram over WhatsApp"
  → kind: "preference"
  → subject: extract name before "prefer/love/hate"
  → predicate: "prefers"
  → object: extract comparison target
  → confidence: 0.80

Entity/fact pattern (high importance, no keyword):
  Input: "The company is Modern Method Inc."
  → kind: "fact"
  → subject: extract noun phrase
  → predicate: "is"
  → object: extract predicate complement
  → confidence: 0.75

Fallback (importance >= 0.8, no pattern match):
  → kind: "fact"
  → statement: episode.content (truncated to 200 chars)
  → subject: "context"
  → predicate: "contains"
  → object: episode.content[..50] + "..."
  → confidence: 0.70
```

Use Rust regex or simple string matching — keep it fast and deterministic.

### 5. Conflict Resolution

Before inserting a new fact, query for existing active facts with same `subject` + `predicate`:

```sql
SELECT * FROM semantic_facts
WHERE subject = $1 AND predicate = $2
AND pruned = false AND superseded_by IS NULL
```

Apply tiered resolution:

| Case | Condition | Action |
|------|-----------|--------|
| **No conflict** | No existing fact | INSERT new fact |
| **Refinement** | Same subject+predicate, objects are compatible (one contains the other) | UPDATE existing: append detail to object, bump confidence +0.05, append source_episodes |
| **Update (temporal)** | Same subject+predicate, objects clearly differ, new confidence >= existing confidence | Supersede: set `existing.superseded_by = new.id`, INSERT new |
| **Supersession (explicit decision)** | Kind = "decision" | Always supersede: INSERT new, set `existing.superseded_by = new.id` |
| **Contradiction (ambiguous)** | confidence delta < `conflict_config.auto_supersede_confidence_delta` (default 0.15) | Flag both: `existing.flagged_for_review = true`, INSERT new with `flagged_for_review = true`, write to review inbox |
| **Auto-supersede** | New confidence > existing + 0.15 | Supersede without flagging |

**Review inbox write** (when flagging):
```
Append to ~/.openclaw/shared/inbox/michael-memory-review.md:

### [TIMESTAMP] Memory Conflict
**Subject:** {subject} / **Predicate:** {predicate}
**Existing:** "{object}" (confidence: {old_confidence:.2})
**New:** "{new_object}" (confidence: {new_confidence:.2})
**Source episodes:** {episode_ids}
Actions: `keep-old` | `keep-new` | `keep-both`
```

### 6. Idle Detection

```rust
async fn is_system_idle(pool: &PgPool, config: &ConsolidationConfig) -> bool {
    // Check: any session_events in the last idle_threshold_seconds (60s)?
    let cutoff = Utc::now() - chrono::Duration::seconds(config.idle_threshold_seconds as i64);
    let recent_count: i64 = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM session_events WHERE created_at > $1",
        cutoff
    )
    .fetch_one(pool)
    .await
    .unwrap_or(Some(0))
    .unwrap_or(0);
    
    if recent_count > 0 { return false; }
    
    // Check: CPU load (Linux /proc/loadavg)
    if let Ok(load) = std::fs::read_to_string("/proc/loadavg") {
        if let Some(load_1m) = load.split_whitespace().next() {
            if let Ok(load_val) = load_1m.parse::<f32>() {
                let cpu_percent = (load_val / num_cpus::get() as f32) * 100.0;
                if cpu_percent > config.cpu_threshold_percent as f32 {
                    return false;
                }
            }
        }
    }
    
    true
}
```

### 7. Background Loop

```rust
pub async fn run_consolidation_loop(
    pool: PgPool,
    config: ConsolidationConfig,
    conflict_config: ConflictResolutionConfig,
    mut shutdown: broadcast::Receiver<()>,
) {
    let interval = tokio::time::Duration::from_secs(config.interval_minutes * 60);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if is_system_idle(&pool, &config).await {
                    match run_consolidation_cycle(&pool, &config, &conflict_config, None).await {
                        Ok(report) => tracing::info!(
                            "Consolidation cycle complete: {} episodes → {} facts",
                            report.episodes_promoted,
                            report.facts_created
                        ),
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
```

### 8. Wire into `main.rs`

After connecting to DB but before starting the IPC server, spawn the consolidation loop:

```rust
// Add ConflictResolutionConfig to EthosConfig first (see config.rs changes below)
let consolidation_pool = pool.clone();
let consolidation_config = config.consolidation.clone();
let conflict_config = config.conflict_resolution.clone();
let consolidation_shutdown = tx.subscribe();

tokio::spawn(async move {
    consolidate::run_consolidation_loop(
        consolidation_pool,
        consolidation_config,
        conflict_config,
        consolidation_shutdown,
    )
    .await;
});
```

### 9. `config.rs` Additions

Add to `EthosConfig`:

```rust
pub conflict_resolution: ConflictResolutionConfig,
```

Add struct:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct ConflictResolutionConfig {
    pub auto_supersede_confidence_delta: f64,
    pub review_inbox: String,
}
```

Also, add `DecayConfig` struct here (used by Story 010):

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct DecayConfig {
    pub base_tau_days: f64,
    pub ltp_multiplier: f64,
    pub frequency_weight: f64,
    pub emotional_weight: f64,
    pub prune_threshold: f64,
}
```

And add to `EthosConfig`:
```rust
pub decay: DecayConfig,
```

---

## Tests (Required — Target: 90% coverage)

### Unit Tests (in `consolidate.rs`)

1. **`test_idle_detection_active`** — Insert a session_event < 60s ago, verify `is_system_idle` returns false
2. **`test_idle_detection_quiet`** — Insert a session_event > 60s ago, verify `is_system_idle` returns true
3. **`test_extract_decision_fact`** — Content with "we decided to use X", verify kind="decision", confidence=0.90
4. **`test_extract_preference_fact`** — Content with "Michael prefers X", verify kind="preference"
5. **`test_extract_fallback_fact`** — High importance (0.9), no pattern match, verify kind="fact" fallback
6. **`test_extract_no_fact`** — Low importance (0.3), no keywords, verify returns None
7. **`test_conflict_refinement`** — Same subject+predicate, compatible objects → UPDATE
8. **`test_conflict_supersession`** — Decision kind, existing fact → superseded_by set
9. **`test_conflict_flag`** — Ambiguous contradiction, confidence delta < 0.15 → both flagged
10. **`test_conflict_auto_supersede`** — New confidence > old + 0.15 → clean supersession

### Integration Tests (`tests/consolidation_integration.rs`)

1. **`test_full_consolidation_cycle`** — Insert 5 episodic_traces (3 with importance >= 0.8, 2 low), run cycle, verify: 3 promoted, consolidated_at set, semantic_facts created
2. **`test_consolidation_marks_episodes`** — After cycle, episodes have `consolidated_at IS NOT NULL`
3. **`test_manual_trigger_consolidation`** — Call `trigger_consolidation(None, Some("test"))`, verify runs without error, returns report

---

## Acceptance Criteria

- [ ] `consolidate.rs` stub replaced with full implementation
- [ ] Background loop spawned from `main.rs` with shutdown signal support
- [ ] High-importance episodes (>= 0.8) promoted to `semantic_facts` within one cycle
- [ ] Episodes marked `consolidated_at = NOW()` after processing
- [ ] Conflict resolution: supersession sets `superseded_by`, flagged writes to review inbox
- [ ] `EthosConfig` extended with `ConflictResolutionConfig` and `DecayConfig`
- [ ] `cargo test` passes with >= 90% coverage
- [ ] `cargo clippy` clean (no warnings)
- [ ] Runbook at `docs/runbooks/runbook-009-consolidation.md`

---

## Definition of Done

- All acceptance criteria checked
- Sage code review: APPROVED
- `cargo build --release` succeeds
- Integration test passes against live DB

---

## Notes for Forge

- Use `num_cpus` crate for CPU count in idle detection (already a common dep — check Cargo.toml first, add if missing)
- The `session_events` table has `created_at` column — use it for idle detection
- Don't call any external LLM — rule-based extraction only for v1. Fast and deterministic wins.
- The `trigger_consolidation` function signature changes from `-> Result<()>` to `-> Result<ConsolidationReport>` — update router.rs to match
- All DB queries should be inside transactions where atomicity matters (e.g., upsert + mark consolidated)
- Write to review inbox using `std::fs::OpenOptions::append` — no async needed for this
