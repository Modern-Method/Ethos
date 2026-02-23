# Story 010 — Ebbinghaus Decay + LTP (Long-Term Potentiation)

**Status:** Ready for Implementation  
**Assigned:** Forge  
**Reviewer:** Sage  
**Priority:** P0 — Closes Ethos v1  
**Depends on:** Story 009 (DecayConfig added to config.rs)

---

## Overview

Implement the Ebbinghaus forgetting curve with Long-Term Potentiation (LTP) for Ethos memory management. Memories decay over time if not accessed, but strengthen with each retrieval — mimicking biological memory consolidation.

This runs as a **decay sweep** integrated into the consolidation loop (Story 009): after each consolidation cycle, run a decay pass. Also handles pruning expired/forgotten memories.

**What gets built:**
1. `decay.rs` subsystem — Ebbinghaus formula + LTP implementation
2. Decay sweep over `memory_vectors`, `episodic_traces`, `semantic_facts`
3. Pruning of memories below salience threshold
4. LTP updates on retrieval (hook into `retrieve.rs`)
5. Wire into consolidation loop

---

## The Formula

```
salience(t) = S_0 × e^(−t/τ_eff) × (1 + α×f) × (1 + β×E)

Where:
  S_0    = current salience (not the initial value — we update in place)
  t      = time since last access (in days)
  τ_eff  = effective decay time constant (days)
           τ_eff = base_tau_days × ltp_multiplier^retrieval_count
  α      = frequency_weight (default 0.3)
  f      = normalized access frequency (retrieval_count / days_since_created)
  β      = emotional_weight (default 0.2)
  E      = emotional_tone (0.0 to 1.0, from episodic_traces.emotional_tone)
           For memory_vectors and semantic_facts: use 0.0 (no emotional tone data)
  
Pruning: if salience < prune_threshold (default 0.05) → set pruned = true
```

**LTP effect**: Each retrieval extends the effective time constant. After 1 retrieval: τ_eff = 7 × 1.5¹ = 10.5 days. After 5 retrievals: τ_eff = 7 × 1.5⁵ = 53 days. Frequently accessed memories become nearly permanent.

---

## Files to Create / Modify

| File | Action | Description |
|------|--------|-------------|
| `ethos-server/src/subsystems/decay.rs` | **Create** | Decay engine |
| `ethos-server/src/subsystems/mod.rs` | **Modify** | Add `pub mod decay;` |
| `ethos-server/src/subsystems/consolidate.rs` | **Modify** | Call `decay::run_decay_sweep()` after consolidation cycle |
| `ethos-server/src/subsystems/retrieve.rs` | **Modify** | Call `decay::record_retrieval()` when results returned |
| `ethos-core/src/ipc.rs` | **Modify** | (if needed) Add `EthosRequest::Decay` for manual trigger |
| `ethos-server/tests/decay_integration.rs` | **Create** | Integration tests |

---

## Implementation

### `decay.rs` — Full Module

```rust
// ethos-server/src/subsystems/decay.rs
//
// Ebbinghaus Decay + LTP (Long-Term Potentiation)
//
// Salience formula: S(t) = S_0 × e^(-t/τ_eff) × (1 + α×f) × (1 + β×E)
// LTP: τ_eff = base_tau × ltp_multiplier^retrieval_count
// Pruning: salience < prune_threshold → pruned = true
```

**Public API:**

```rust
/// Run a full decay sweep over all memory tables.
/// Called by the consolidation loop after each cycle.
pub async fn run_decay_sweep(pool: &PgPool, config: &DecayConfig) -> Result<DecaySweepReport>

/// Record a retrieval event for a memory item (LTP effect).
/// Called by retrieve.rs when returning results.
/// Updates: retrieval_count++, last_retrieved_at = NOW(), salience boost.
pub async fn record_retrieval(pool: &PgPool, memory_id: Uuid, source_type: &str) -> Result<()>

/// Calculate the new salience for a memory item (pure function — no DB calls).
/// Used by tests and by the sweep.
pub fn calculate_salience(
    current_salience: f64,
    retrieval_count: i32,
    created_at: DateTime<Utc>,
    last_accessed: Option<DateTime<Utc>>,
    emotional_tone: f64,
    config: &DecayConfig,
) -> f64
```

**Internal helpers:**

```rust
/// Sweep memory_vectors table
async fn decay_memory_vectors(pool: &PgPool, config: &DecayConfig) -> Result<DecayStats>

/// Sweep episodic_traces table
async fn decay_episodic_traces(pool: &PgPool, config: &DecayConfig) -> Result<DecayStats>

/// Sweep semantic_facts table (decay confidence, not salience directly)
async fn decay_semantic_facts(pool: &PgPool, config: &DecayConfig) -> Result<DecayStats>
```

### Structs

```rust
#[derive(Debug, Default)]
pub struct DecaySweepReport {
    pub vectors_updated: usize,
    pub vectors_pruned: usize,
    pub episodes_updated: usize,
    pub episodes_pruned: usize,
    pub facts_updated: usize,
    pub facts_pruned: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Default)]
struct DecayStats {
    updated: usize,
    pruned: usize,
}
```

### `calculate_salience` — Pure Function

```rust
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
```

### `decay_memory_vectors`

```sql
-- Fetch non-pruned, non-expired vectors that need decay
SELECT id, importance, access_count, last_accessed, created_at, expires_at
FROM memory_vectors
WHERE pruned = false
  AND (expires_at IS NULL OR expires_at > NOW())
```

For each row:
1. Calculate new salience using `calculate_salience(importance, access_count, created_at, last_accessed, 0.0, config)`
2. If `new_salience < config.prune_threshold` → set `pruned = true`
3. If `expires_at IS NOT NULL AND expires_at <= NOW()` → set `pruned = true` regardless
4. Batch UPDATE: `UPDATE memory_vectors SET importance = $1, pruned = $2 WHERE id = $3`

Process in batches of 500 rows to avoid holding locks.

### `decay_episodic_traces`

```sql
SELECT id, salience, retrieval_count, last_retrieved_at, created_at, emotional_tone
FROM episodic_traces
WHERE pruned = false
```

For each row:
1. Calculate new salience using `calculate_salience(salience, retrieval_count, created_at, last_retrieved_at, emotional_tone, config)`
2. If `new_salience < config.prune_threshold` → `pruned = true`
3. Batch UPDATE: `UPDATE episodic_traces SET salience = $1, pruned = $2 WHERE id = $3`

### `decay_semantic_facts`

For semantic_facts, we decay **confidence** (not salience — facts have a separate quality signal):

```sql
SELECT id, confidence, salience, retrieval_count, last_retrieved_at, created_at
FROM semantic_facts
WHERE pruned = false AND superseded_by IS NULL
```

For each row:
1. Decay confidence: `new_confidence = calculate_salience(confidence, retrieval_count, created_at, last_retrieved_at, 0.0, config)`
2. Decay salience: `new_salience = calculate_salience(salience, retrieval_count, created_at, last_retrieved_at, 0.0, config)`
3. If `new_confidence < config.prune_threshold` → `pruned = true`
4. UPDATE: `SET confidence = $1, salience = $2, pruned = $3 WHERE id = $4`

Note: facts decay more slowly than episodes by nature (they have higher LTP effect from retrieval).

### `record_retrieval` — Hook into retrieve.rs

When `retrieve.rs` returns search results, it should call `record_retrieval` for each result ID:

```rust
pub async fn record_retrieval(pool: &PgPool, memory_id: Uuid, source_type: &str) -> Result<()> {
    match source_type {
        "episode" => {
            sqlx::query!(
                "UPDATE episodic_traces 
                 SET retrieval_count = retrieval_count + 1, 
                     last_retrieved_at = NOW(),
                     salience = LEAST(salience * 1.1, 1.0)
                 WHERE id = $1",
                memory_id
            )
            .execute(pool)
            .await?;
        }
        "fact" => {
            sqlx::query!(
                "UPDATE semantic_facts 
                 SET retrieval_count = retrieval_count + 1,
                     last_retrieved_at = NOW(),
                     confidence = LEAST(confidence + 0.02, 1.0),
                     salience = LEAST(salience * 1.1, 1.0)
                 WHERE id = $1",
                memory_id
            )
            .execute(pool)
            .await?;
        }
        _ => {
            // memory_vectors
            sqlx::query!(
                "UPDATE memory_vectors 
                 SET access_count = access_count + 1,
                     last_accessed = NOW(),
                     importance = LEAST(importance * 1.05, 1.0)
                 WHERE id = $1",
                memory_id
            )
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}
```

### Wire into consolidate.rs

At the end of `run_consolidation_cycle`, after promoting episodes:

```rust
// Run decay sweep after consolidation
match decay::run_decay_sweep(pool, &decay_config).await {
    Ok(report) => tracing::info!(
        "Decay sweep: pruned {} vectors, {} episodes, {} facts",
        report.vectors_pruned, report.episodes_pruned, report.facts_pruned
    ),
    Err(e) => tracing::warn!("Decay sweep error (non-fatal): {}", e),
}
```

Pass `DecayConfig` into `run_consolidation_cycle` — it's already in `EthosConfig` (added by Story 009).

### Wire retrieve.rs

After returning results from `retrieve.rs`, fire-and-forget record_retrieval for each result ID:

```rust
// After building results vector:
let pool_clone = pool.clone();
let result_ids: Vec<(Uuid, String)> = results.iter()
    .map(|r| (r.id, "vector".to_string()))
    .collect();
tokio::spawn(async move {
    for (id, source_type) in result_ids {
        if let Err(e) = decay::record_retrieval(&pool_clone, id, &source_type).await {
            tracing::warn!("LTP update failed for {}: {}", id, e);
        }
    }
});
```

---

## DB — No Migrations Needed

All columns used already exist:
- `memory_vectors`: `importance`, `access_count`, `last_accessed`, `expires_at`, `pruned` (need to add `pruned` if missing — check)
- `episodic_traces`: `salience`, `retrieval_count`, `last_retrieved_at`, `emotional_tone`, `pruned`
- `semantic_facts`: `salience`, `confidence`, `retrieval_count`, `last_retrieved_at`, `pruned`, `superseded_by`

**Check first:** Run `\d memory_vectors` to verify `pruned` column exists. If not, create migration `003_story_010_pruned_flag.sql`:
```sql
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS pruned boolean NOT NULL DEFAULT false;
```

---

## Tests (Required — Target: 90% coverage)

### Unit Tests (in `decay.rs`)

1. **`test_calculate_salience_fresh`** — New memory (t=0, no retrievals) → salience barely changes
2. **`test_calculate_salience_one_week`** — Memory accessed 7 days ago, no retrievals, tau=7 → salience ≈ S_0 × e^(-1) ≈ 0.368 × S_0
3. **`test_calculate_salience_with_ltp`** — 5 retrievals, same time → tau_eff = 7 × 1.5^5 = 53 days → much slower decay
4. **`test_calculate_salience_emotional_boost`** — emotional_tone = 1.0, β = 0.2 → (1 + 0.2 × 1.0) = 1.2x boost
5. **`test_calculate_salience_clamp_max`** — Boosted value > 1.0 → clamped to 1.0
6. **`test_calculate_salience_prune_threshold`** — Very old memory, no retrievals → salience < 0.05
7. **`test_calculate_salience_frequency_boost`** — High retrieval_count / days_alive → f boost applied

### Integration Tests (`tests/decay_integration.rs`)

1. **`test_decay_sweep_marks_stale_memories`** — Insert memory_vector with last_accessed = 90 days ago, importance = 0.1, no retrievals. Run sweep. Verify pruned = true.
2. **`test_decay_sweep_preserves_fresh_memories`** — Insert memory with last_accessed = 1 day ago, importance = 0.9. Run sweep. Verify pruned = false, importance slightly reduced but > 0.05.
3. **`test_ltp_prevents_pruning`** — Insert memory with last_accessed = 30 days ago but retrieval_count = 10. Run sweep. Verify not pruned (LTP extends tau).
4. **`test_record_retrieval_updates_counts`** — Insert episodic trace, call record_retrieval, verify retrieval_count = 1, last_retrieved_at IS NOT NULL, salience increased.
5. **`test_decay_sweep_report`** — Run sweep with mix of fresh/stale memories. Verify report counts are accurate.
6. **`test_fact_confidence_decay`** — Insert semantic_fact with low retrieval_count and old created_at. Run sweep. Verify confidence reduced.

---

## Acceptance Criteria

- [ ] `decay.rs` created with full Ebbinghaus + LTP implementation
- [ ] `calculate_salience` is a pure function (no DB calls, fully testable)
- [ ] Decay sweep runs after each consolidation cycle
- [ ] `record_retrieval` called from `retrieve.rs` (fire-and-forget, non-blocking)
- [ ] Stale memories (salience < 0.05) marked `pruned = true` (NOT deleted — recoverable)
- [ ] Expired memories (expires_at < NOW()) marked `pruned = true`
- [ ] LTP: memories retrieved frequently decay significantly slower (verify in tests)
- [ ] `cargo test` passes with >= 90% coverage
- [ ] `cargo clippy` clean
- [ ] Runbook at `docs/runbooks/runbook-010-decay.md`

---

## Definition of Done

- All acceptance criteria checked
- Sage code review: APPROVED
- `cargo build --release` succeeds
- Integration tests pass against live DB
- `run_decay_sweep` logged and observable in server output

---

## Notes for Forge

- `calculate_salience` MUST be a pure function — easy to test, no side effects
- Pruned memories are **soft-deleted** (pruned = true), never hard-deleted. This preserves audit trail and allows recovery.
- Batch DB updates in chunks of 500 to avoid long-held locks during sweeps
- The decay sweep is non-fatal — if it fails, log a warning but don't crash the consolidation cycle
- `record_retrieval` is fire-and-forget — use `tokio::spawn` in retrieve.rs, never block the search response
- Use `DateTime<Utc>` everywhere (chrono crate, already in Cargo.toml)
- `f64` not `f32` for salience calculations (precision matters for decay formula)
