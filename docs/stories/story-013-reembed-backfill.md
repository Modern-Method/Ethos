# Story 013 — Background Re-Embed Backfill Worker

**Status:** Ready for Implementation  
**Assigned:** Forge  
**Reviewer:** Sage  
**Priority:** P1 — Closes the Story 012 NULL-embedding gap

---

## Overview

Story 012 introduced `gemini-fallback-onnx` mode: when Gemini fails, memories are stored with `NULL` embeddings. Vector similarity search cannot reach them — they degrade to keyword-only.

This story adds a **background re-embed worker** that:
1. Periodically scans for memories with `NULL` embeddings
2. Batches them and re-embeds via the currently configured backend
3. Updates the DB, restoring full vector search
4. Respects rate limits and skips gracefully if the backend is still unavailable

After this story, `NULL` embeddings are a temporary state, not a permanent one.

---

## Design

### Where It Lives

The re-embed worker is a new background task inside `ethos-server/src/subsystems/`, spawned alongside the consolidation engine and decay worker in `main.rs`.

It runs on a **configurable timer** (default: every 10 minutes). Event-based triggering (e.g., detecting backend recovery) is explicitly **out of scope for v1** — timer is simpler and good enough.

### What It Does Per Tick

```
1. Count NULL-embedding rows in episodic_memories + semantic_facts
2. If count == 0: log "no nulls, skip" and sleep until next tick
3. If backend unavailable: log "backend not ready, skip" and sleep
4. Fetch a batch of NULL-embedding rows (configurable batch size, default 50)
5. For each row: call backend.embed(content)
   - On Ok(Some(vec))  → UPDATE embedding, increment success counter
   - On Ok(None)       → backend is in fallback mode, stop batch, log, sleep
   - On Err(e)         → log error, skip this row, continue batch
6. Log summary: N re-embedded, M skipped
7. Sleep until next tick
```

### Rate Limiting

Re-embed batches must respect `[embedding] rate_limit_rpm`. The worker should insert inter-request delays when processing a batch, the same way the main embedder subsystem does.

Use the existing `rate_limit_rpm` config field — don't add new config.

### Scope: Which Tables

| Table | Column to check | Column to update |
|-------|----------------|-----------------|
| `episodic_memories` | `embedding IS NULL` | `embedding` |
| `semantic_facts` | `embedding IS NULL` | `embedding` |

Process episodic first (more recent, higher retrieval value), then semantic.

---

## Config Changes

Add to `[embedding]` section in `ethos.toml`:

```toml
[embedding]
# ... existing fields ...

# Re-embed backfill worker
reembed_interval_minutes = 10   # How often to scan for NULLs
reembed_batch_size = 50         # Records per tick
reembed_enabled = true          # Set false to disable entirely
```

Add to `EmbeddingConfig` in `ethos-core/src/config.rs`.

---

## Implementation Plan

### 1. New file: `ethos-server/src/subsystems/reembed.rs`

```rust
use anyhow::Result;
use ethos_core::{
    config::EmbeddingConfig,
    embeddings::EmbeddingBackend,
};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::time::{Duration, interval};

pub async fn run_reembed_worker(
    pool: PgPool,
    backend: Arc<dyn EmbeddingBackend>,
    config: EmbeddingConfig,
) -> Result<()> {
    if !config.reembed_enabled {
        tracing::info!("Re-embed worker disabled via config");
        return Ok(());
    }

    let mut ticker = interval(Duration::from_secs(
        config.reembed_interval_minutes * 60,
    ));

    loop {
        ticker.tick().await;
        
        match run_reembed_tick(&pool, backend.clone(), &config).await {
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

async fn run_reembed_tick(
    pool: &PgPool,
    backend: Arc<dyn EmbeddingBackend>,
    config: &EmbeddingConfig,
) -> Result<(usize, usize)> {
    let null_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM episodic_memories WHERE embedding IS NULL
         UNION ALL
         SELECT COUNT(*) FROM semantic_facts WHERE embedding IS NULL"
    )
    .fetch_one(pool)
    .await?;

    if null_count == 0 {
        return Ok((0, 0));
    }

    tracing::debug!(null_count = null_count, "Found NULL embeddings, starting backfill");

    let mut embedded = 0usize;
    let mut skipped = 0usize;

    // Process episodic memories first
    let episodes = sqlx::query!(
        "SELECT id, content FROM episodic_memories 
         WHERE embedding IS NULL 
         ORDER BY created_at DESC 
         LIMIT $1",
        config.reembed_batch_size as i64
    )
    .fetch_all(pool)
    .await?;

    for row in &episodes {
        match backend.embed(&row.content).await? {
            Some(vec) => {
                let pgvec = pgvector::Vector::from(vec);
                sqlx::query!(
                    "UPDATE episodic_memories SET embedding = $1 WHERE id = $2",
                    pgvec as _,
                    row.id
                )
                .execute(pool)
                .await?;
                embedded += 1;
                apply_rate_limit(config).await;
            }
            None => {
                // Backend still in fallback mode — stop the batch
                tracing::debug!("Backend returned None during backfill — stopping batch");
                skipped += episodes.len() - embedded;
                return Ok((embedded, skipped));
            }
        }
    }

    // Then semantic facts (if budget remains)
    let remaining = config.reembed_batch_size.saturating_sub(embedded);
    if remaining == 0 {
        return Ok((embedded, skipped));
    }

    let facts = sqlx::query!(
        "SELECT id, content FROM semantic_facts 
         WHERE embedding IS NULL 
         ORDER BY created_at DESC 
         LIMIT $1",
        remaining as i64
    )
    .fetch_all(pool)
    .await?;

    for row in &facts {
        match backend.embed(&row.content).await? {
            Some(vec) => {
                let pgvec = pgvector::Vector::from(vec);
                sqlx::query!(
                    "UPDATE semantic_facts SET embedding = $1 WHERE id = $2",
                    pgvec as _,
                    row.id
                )
                .execute(pool)
                .await?;
                embedded += 1;
                apply_rate_limit(config).await;
            }
            None => {
                skipped += facts.len() - (embedded - episodes.len());
                return Ok((embedded, skipped));
            }
        }
    }

    Ok((embedded, skipped))
}

/// Insert inter-request delay to respect rate_limit_rpm
async fn apply_rate_limit(config: &EmbeddingConfig) {
    if config.rate_limit_rpm > 0 {
        let delay_ms = 60_000 / config.rate_limit_rpm as u64;
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
}
```

### 2. Spawn in `ethos-server/src/main.rs`

```rust
// Alongside existing subsystem spawns:
let reembed_backend = backend.clone();
tokio::spawn(subsystems::reembed::run_reembed_worker(
    pool.clone(),
    reembed_backend,
    config.embedding.clone(),
));
```

### 3. `ethos-server/src/subsystems/mod.rs`

Add `pub mod reembed;`

### 4. Config struct update (`ethos-core/src/config.rs`)

```rust
pub struct EmbeddingConfig {
    // ... existing fields ...
    
    #[serde(default = "default_reembed_interval")]
    pub reembed_interval_minutes: u64,
    
    #[serde(default = "default_reembed_batch_size")]
    pub reembed_batch_size: usize,
    
    #[serde(default = "default_reembed_enabled")]
    pub reembed_enabled: bool,
}

fn default_reembed_interval() -> u64 { 10 }
fn default_reembed_batch_size() -> usize { 50 }
fn default_reembed_enabled() -> bool { true }
```

---

## Files to Create / Modify

| File | Action |
|------|--------|
| `ethos-server/src/subsystems/reembed.rs` | **Create** — full worker implementation |
| `ethos-server/src/subsystems/mod.rs` | **Modify** — add `pub mod reembed` |
| `ethos-server/src/main.rs` | **Modify** — spawn the worker task |
| `ethos-core/src/config.rs` | **Modify** — add 3 new config fields with defaults |
| `ethos.toml.example` | **Modify** — add `reembed_*` fields to `[embedding]` |

---

## Acceptance Criteria

- [ ] `reembed.rs` compiles cleanly with `cargo build`
- [ ] Worker starts up with the server and logs its status
- [ ] With `reembed_enabled = false`: worker exits immediately, logs disabled message
- [ ] When all embeddings are present: tick completes silently (no log spam)
- [ ] When NULLs exist and backend available: NULLs get filled, DB confirms non-NULL
- [ ] When backend returns `None` (fallback mode): batch stops cleanly, no panic
- [ ] Rate limiting: inter-request delay applied per `rate_limit_rpm`
- [ ] `cargo test` passes (existing tests unaffected; add 1-2 unit tests for tick logic)

---

## Out of Scope

- Event-based triggering on backend recovery detection (Story 014 candidate)
- Retry logic for individual failed rows (they'll be retried on next tick)
- Metrics/Prometheus instrumentation (separate story)
- Re-embed prioritization by importance score (future enhancement)

---

## References

- Story 012: `docs/stories/story-012-onnx-fallback.md`
- Consolidation worker (architectural reference): `ethos-server/src/subsystems/consolidate.rs`
- Decay worker (timer pattern reference): `ethos-server/src/subsystems/decay.rs`
- Embedding config: `ethos-core/src/config.rs`
