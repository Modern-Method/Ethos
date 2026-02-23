# Runbook 010 — Ebbinghaus Decay + LTP

**Story:** 010 — Ebbinghaus Forgetting Curve + Long-Term Potentiation  
**Component:** `ethos-server/src/subsystems/decay.rs`  
**Author:** Forge  
**Date:** 2026-02-23  

---

## Architecture Overview

```mermaid
graph TD
    A[After Consolidation Cycle - Story 009] --> B[run_decay_sweep]
    B --> C[decay_memory_vectors]
    B --> D[decay_episodic_traces]
    B --> E[decay_semantic_facts]

    C --> F{calculate_salience}
    D --> F
    E --> F

    F --> G[S0 × e^(-t/τ_eff) × freq_boost × emotional_boost]
    G --> H{salience < prune_threshold?}
    H -->|Yes| I[SET pruned = true - soft delete]
    H -->|No| J[UPDATE salience/importance]

    K[Search Request - retrieve.rs] --> L[Results Returned]
    L --> M[tokio::spawn - fire and forget]
    M --> N[record_retrieval for each result ID]
    N --> O[LTP: retrieval_count++, last_retrieved_at = NOW, salience boost]
```

---

## The Formula

```
salience(t) = S_0 × e^(−t/τ_eff) × (1 + α×f) × (1 + β×E)

Where:
  S_0    = current salience (updated in-place each sweep)
  t      = days since last access (last_retrieved_at or created_at)
  τ_eff  = base_tau_days × ltp_multiplier^retrieval_count
  α      = frequency_weight (default 0.3)
  f      = min(retrieval_count / days_alive, 1.0)
  β      = emotional_weight (default 0.2)
  E      = emotional_tone, clamped to [0.0, 1.0]

Pruning threshold: salience < prune_threshold (default 0.05) → pruned = true
```

### LTP Effect on τ_eff

| Retrievals | τ_eff (days) | Notes |
|------------|-------------|-------|
| 0 | 7.0 | Base: decays to ~37% in 1 week |
| 1 | 10.5 | 50% slower decay |
| 3 | 23.6 | ~3× slower decay |
| 5 | 53.2 | ~7.5× slower — weeks to prune |
| 10 | 402 | ~57× slower — effectively permanent |

### Numerical Examples

```
Fresh memory (t=0, no retrievals):
  salience = 1.0 × e^0 × 1.0 × 1.0 = 1.0  (unchanged)

One week old (t=7, no retrievals, tau=7):
  salience = 1.0 × e^(-1) × 1.0 = 0.368

One month old (t=30, 5 retrievals, tau=53):
  salience ≈ 1.0 × e^(-0.566) × ... ≈ > 0.5  (LTP prevents decay)

Emotional memory (t=7, emotional_tone=1.0):
  salience = 0.368 × 1.2 = 0.442  (20% boost from emotion)
```

---

## Decay Sweep

Runs automatically after every consolidation cycle (every 15 minutes when idle). Also runnable manually.

### Tables Swept

| Table | Salience Field | Emotional Tone | Prune Condition |
|-------|---------------|----------------|-----------------|
| `memory_vectors` | `importance` | 0.0 (no data) | importance < 0.05 OR expires_at <= NOW() |
| `episodic_traces` | `salience` | `emotional_tone` column | salience < 0.05 |
| `semantic_facts` | `confidence`, `salience` | 0.0 (no data) | confidence < 0.05 |

Processed in batches of 500 rows to avoid long-held DB locks.

---

## LTP Record Retrieval

When `retrieve.rs` returns search results, it fire-and-forgets a `tokio::spawn` call:

```rust
tokio::spawn(async move {
    for (id, source_type) in result_ids {
        decay::record_retrieval(&pool, id, "vector").await;
    }
});
```

This is **non-blocking** — the search response is returned to the caller before LTP updates complete. Failures are logged as warnings, not errors.

**LTP Updates Per Source Type:**

| source_type | retrieval_count | last_retrieved_at | Salience Boost |
|-------------|----------------|-------------------|----------------|
| "episode" | +1 | NOW() | salience × 1.1 |
| "fact" | +1 | NOW() | confidence +0.02, salience × 1.1 |
| (anything else / "vector") | access_count +1 | NOW() | importance × 1.05 |

---

## Soft Deletes (pruned = true)

**Pruned memories are NEVER hard-deleted.** The `pruned` flag preserves the audit trail and enables recovery.

To view pruned memories:
```sql
-- Pruned vectors
SELECT id, importance, content_snippet, created_at 
FROM memory_vectors WHERE pruned = true
ORDER BY created_at DESC LIMIT 20;

-- Pruned episodes
SELECT id, salience, content[:100], agent_id
FROM episodic_traces WHERE pruned = true
ORDER BY created_at DESC LIMIT 20;
```

To recover a pruned memory:
```sql
UPDATE memory_vectors SET pruned = false, importance = 0.1 WHERE id = '<id>';
```

---

## Configuration

In `ethos.toml`:
```toml
[decay]
base_tau_days = 7.0      # Base time constant (days to decay to ~37%)
ltp_multiplier = 1.5     # Each retrieval multiplies tau_eff by this
frequency_weight = 0.3   # α: frequency boost weight
emotional_weight = 0.2   # β: emotional tone boost weight
prune_threshold = 0.05   # Soft-delete when salience falls below this
```

**Tuning Advice:**
- Increase `base_tau_days` to retain memories longer overall
- Increase `ltp_multiplier` to make retrievals have more impact
- Decrease `prune_threshold` to prune more aggressively
- Lower `frequency_weight`/`emotional_weight` to make decay more uniform

---

## Database Changes

**Migration 003** (applied 2026-02-23):
```sql
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS pruned BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX IF NOT EXISTS idx_vectors_pruned ON memory_vectors(pruned) WHERE pruned = FALSE;
```

**Columns Used:**

`memory_vectors`:
- `importance` (f64) — treated as salience, decayed in place
- `access_count` (i32) — retrieval count for LTP
- `last_accessed` (TIMESTAMPTZ) — for t calculation
- `expires_at` (TIMESTAMPTZ) — hard expiration, prunes regardless of salience
- `pruned` (bool) — NEW: soft-delete flag

`episodic_traces`:
- `salience` (f64) — decayed in place
- `retrieval_count` (i32) — LTP
- `last_retrieved_at` (TIMESTAMPTZ)
- `emotional_tone` (f64) — E in formula
- `pruned` (bool) — existing column

`semantic_facts`:
- `confidence` (f64) — decayed as primary signal
- `salience` (f64) — decayed secondarily
- `retrieval_count` (i32) — LTP
- `last_retrieved_at` (TIMESTAMPTZ)
- `pruned` (bool) — existing column

---

## Observability

Log messages to watch:
```
INFO Decay sweep complete: 15 vectors (2 pruned), 8 episodes (0 pruned), 3 facts (0 pruned) in 45ms
WARN LTP update failed for <id>: ...
WARN Decay sweep error (non-fatal): ...
```

Monitoring queries:
```sql
-- How many memories are pruned?
SELECT 
  'memory_vectors' as table_name, COUNT(*) as pruned_count 
  FROM memory_vectors WHERE pruned = true
UNION ALL
SELECT 'episodic_traces', COUNT(*) FROM episodic_traces WHERE pruned = true
UNION ALL
SELECT 'semantic_facts', COUNT(*) FROM semantic_facts WHERE pruned = true;

-- Average salience by age bucket
SELECT 
  CASE 
    WHEN created_at > NOW() - INTERVAL '1 day' THEN '<1d'
    WHEN created_at > NOW() - INTERVAL '7 days' THEN '1-7d'
    WHEN created_at > NOW() - INTERVAL '30 days' THEN '7-30d'
    ELSE '>30d'
  END as age_bucket,
  AVG(salience) as avg_salience,
  COUNT(*) as count
FROM episodic_traces
WHERE pruned = false
GROUP BY 1;
```

---

## Runbook: Common Issues

### "Decay sweep not running"
1. Check: Is consolidation loop running? Decay runs after each consolidation cycle.
2. Check: Is system idle? If not idle, consolidation (and decay) is skipped.
3. Check logs: `grep "Decay sweep" /var/log/ethos/server.log`

### "Memories decaying too fast"
1. Increase `base_tau_days` (e.g., from 7 to 14)
2. Ensure `record_retrieval` is being called after searches (check retrieve.rs hook)
3. Check if `ltp_multiplier` is set correctly (1.5 = each retrieval extends tau by 50%)

### "Too many memories pruned"
1. Increase `prune_threshold` (e.g., from 0.05 to 0.02)
2. Or set `base_tau_days` higher
3. Recover specific memories with: `UPDATE memory_vectors SET pruned = false WHERE id = '<id>'`

### "LTP not working"
1. Verify retrieve.rs is calling `record_retrieval` (check spawn block in search_memory)
2. Query: `SELECT id, retrieval_count, last_retrieved_at FROM episodic_traces ORDER BY retrieval_count DESC LIMIT 10`
3. Check for WARN-level logs about LTP failures

---

## Testing

```bash
# Unit tests (pure functions, fast)
cargo test subsystems::decay::tests::test_calculate_

# Integration tests (require DB)
cargo test subsystems::decay -- --test-threads=1

# All tests
cargo test -- --test-threads=1

# Coverage
cargo tarpaulin --out Stdout -- --test-threads=1
```

---

## Files Created/Modified

| File | Change |
|------|--------|
| `ethos-server/src/subsystems/decay.rs` | NEW — Ebbinghaus decay + LTP engine |
| `ethos-server/src/subsystems/mod.rs` | Added `pub mod decay;` |
| `ethos-server/src/subsystems/consolidate.rs` | Call `decay::run_decay_sweep()` after cycle |
| `ethos-server/src/subsystems/retrieve.rs` | Call `decay::record_retrieval()` fire-and-forget |
| `migrations/003_story_010_pruned_flag.sql` | NEW — Add `pruned` column to `memory_vectors` |
