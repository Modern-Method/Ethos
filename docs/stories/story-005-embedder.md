# Story 005: Embedder — Fill the Vectors

**Status:** Ready  
**Owner:** Forge  
**Epic:** Animus Core  
**Priority:** High  

## 🎯 Goal
Call the Gemini Embeddings API for every row in `memory_vectors` where `vector IS NULL` and write the resulting 768-dimensional vector back. This transforms raw stored text into semantic memory — the prerequisite for Story 006 retrieval.

## 📦 Scope
- **New file:** `ethos-server/src/subsystems/embedder.rs`
- **New file:** `ethos-core/src/embeddings.rs` — Gemini API client (async, retryable)
- **Updated:** `ethos-server/src/subsystems/ingest.rs` — trigger embedding after DB write
- **Updated:** `ethos-server/src/router.rs` — add `EthosRequest::Embed` for manual trigger
- **NOT in scope:** Batch re-embedding, scheduled sweeps, ONNX fallback, consolidation

## 🌐 Gemini Embeddings API

**Endpoint:** `https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-001:embedContent`  
**Auth:** `x-goog-api-key: <GOOGLE_API_KEY>` header  
**Dimensions:** 768 (matches `vector(768)` column)  
**Task type:** `RETRIEVAL_DOCUMENT` for storing, `RETRIEVAL_QUERY` for searching  

**Request:**
```json
{
  "model": "models/gemini-embedding-001",
  "content": { "parts": [{ "text": "the content to embed" }] },
  "taskType": "RETRIEVAL_DOCUMENT"
}
```

**Response:**
```json
{
  "embedding": {
    "values": [0.123, -0.456, ...]  // 768 floats
  }
}
```

**API Key source:** `ethos.toml` → `[embeddings] google_api_key = "..."` OR env var `GOOGLE_API_KEY`

## 🗃️ DB Operation

```sql
UPDATE memory_vectors
SET vector = $1
WHERE id = $2
```

`$1` is a `pgvector::Vector` built from the 768 floats returned by Gemini.

## 🧪 Acceptance Criteria (TDD — Red first!)

1. **Test: embed_content calls API and returns 768-dim vector**
   - Mock the HTTP client (use `mockito` or `wiremock`)
   - Assert returned vector has exactly 768 dimensions
   - Assert no panic on valid response
2. **Test: vector written to DB after ingest**
   - Send an `Ingest` request → verify `memory_vectors.vector IS NOT NULL` after embedding runs
   - Allow up to 3s for async embedding to complete
3. **Test: API error handled gracefully**
   - Mock API returning 429/500 → assert embedding retries (up to 3x) then logs error, does NOT crash
   - Row stays with `vector IS NULL` on failure (embedder will retry next cycle)
4. **Test: manual Embed trigger via IPC**
   - Send `EthosRequest::Embed { id: <uuid> }` → verify vector populated
5. **Coverage:** >90% via `cargo tarpaulin`
6. **Runbook:** `docs/runbooks/embedder.md` created

## ⚙️ Config (ethos.toml additions)

```toml
[embeddings]
provider = "gemini"
model = "gemini-embedding-001"
dimensions = 768
google_api_key = ""          # populated from env GOOGLE_API_KEY if empty
max_retries = 3
retry_delay_ms = 1000
```

## 🛠️ Implementation Notes

- **Async, non-blocking:** embedding runs in a `tokio::spawn` after the DB write in `ingest.rs` — never blocks the IPC response
- **HTTP client:** use `reqwest` (already in workspace deps or add it)
- **Retry logic:** exponential backoff, max 3 attempts, log errors with `tracing::error!`
- **API key:** read from config first, fall back to `std::env::var("GOOGLE_API_KEY")`
- **pgvector insert:** use `pgvector::Vector::from(vec![f32])` — pgvector crate already in deps
- **IPC type to add in `ethos-core/src/ipc.rs`:**
```rust
EthosRequest::Embed { id: uuid::Uuid }
```

## ✅ Done Checklist
Complete `/home/revenantpulse/Projects/DONE_CHECKLIST.md` before logging shipped.

---
*Spec by Neko — Story 005. The one where Ethos gets a brain.*
