# Story 004: DB Ingest — Wire Memory to PostgreSQL

**Status:** Ready  
**Owner:** Forge  
**Epic:** Animus Core  
**Priority:** High  

## 🎯 Goal
Replace the `ingest_payload` stub with real PostgreSQL writes. When the TypeScript hook fires, the message must land in `session_events` and `memory_vectors` tables. This is where Ethos starts *actually remembering*.

## 📦 Scope
- **File:** `ethos-server/src/subsystems/ingest.rs` — replace stub with real logic
- **Tables written to:**
  - `session_events` — raw event log (every message, always)
  - `memory_vectors` — embedding placeholder row (vector populated by embedder in a future story)
- **NOT in scope:** Gemini embedding API calls, consolidation, retrieval, semantic promotion

## 🗃️ Schema Reference (Story 001)

```sql
-- session_events
CREATE TABLE session_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('user','assistant','system','tool')),
    content TEXT NOT NULL,
    tokens INTEGER,
    metadata JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- memory_vectors
CREATE TABLE memory_vectors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    content TEXT NOT NULL,
    source TEXT NOT NULL,
    vector vector(768),          -- NULL until embedder runs
    metadata JSONB DEFAULT '{}',
    importance FLOAT DEFAULT 0.5,
    access_count INTEGER DEFAULT 0,
    last_accessed TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    expires_at TIMESTAMPTZ
);
```

## 🧪 Acceptance Criteria (TDD Mandatory — Red first!)

1. **Test: session_events write**
   - Send an `Ingest` request with `{content: "hello", source: "user", metadata: {session_id: "test-session"}}`
   - Verify a row exists in `session_events` with matching `content`, `role = 'user'`, `session_id`
2. **Test: memory_vectors write**
   - Same request → verify a row in `memory_vectors` with matching `content`, `source`, `vector = NULL`
3. **Test: assistant role mapping**
   - Send `source: "assistant"` → verify `role = 'assistant'` in `session_events`
4. **Test: malformed payload gracefully rejected**
   - Send `payload: {}` (missing content) → server returns `{status: "error"}`, no DB write
5. **Coverage:** >90% via `cargo tarpaulin`
6. **Runbook:** `docs/runbooks/ingest-subsystem.md` created

## 🔌 IPC Payload Contract

The TypeScript hook sends:
```json
{
  "action": "ingest",
  "payload": {
    "content": "the message text",
    "source": "user" | "assistant" | "system",
    "metadata": {
      "channel": "telegram",
      "session_id": "optional",
      "author": "optional"
    }
  }
}
```

The `ingest_payload(payload: serde_json::Value, pool: &PgPool)` function needs the DB pool passed in. **Update the router to pass `pool` to `ingest_payload`.**

## 🛠️ Implementation Notes

- `source` → `role` mapping: `"user"` → `'user'`, `"assistant"` → `'assistant'`, `"system"` → `'system'`
- `session_id`: extract from `payload.metadata.session_id`, fallback to `"default"`
- `agent_id`: extract from `payload.metadata.agent_id`, fallback to `"ethos"`
- `tokens`: leave NULL (no tokenizer yet)
- Use `sqlx::query!()` macros — no raw string interpolation
- Both inserts should be in a **single transaction** (atomic — both succeed or neither does)
- On DB error: log with `tracing::error!`, return `EthosResponse::err(...)`

## ✅ Done Checklist
Complete `/home/revenantpulse/Projects/DONE_CHECKLIST.md` before logging shipped.

---
*Spec by Neko — Story 004*
