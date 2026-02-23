# Story 006: Retrieval — Query What Ethos Knows

**Status:** Ready  
**Owner:** Forge  
**Epic:** Animus Core  
**Priority:** High  

## 🎯 Goal
Implement `EthosRequest::Search` — the IPC endpoint that accepts a query string, embeds it using `RETRIEVAL_QUERY` task type, and returns the top-K semantically similar memories from `memory_vectors` using pgvector cosine similarity. This closes the memory loop: **ingest → embed → retrieve**.

## 📦 Scope
- **Updated:** `ethos-core/src/ipc.rs` — add `EthosRequest::Search` and `SearchResult` response type
- **Updated:** `ethos-server/src/subsystems/retrieve.rs` — replace stub with real pgvector query
- **Updated:** `ethos-server/src/router.rs` — wire `Search` → `retrieve::search`
- **NOT in scope:** Re-ranking, spreading activation, hybrid BM25+vector, consolidation

## 🔌 IPC Contract

**Request:**
```json
{
  "action": "search",
  "query": "what did we talk about yesterday?",
  "limit": 5,
  "task_type": "retrieval_query"
}
```

**Response:**
```json
{
  "status": "ok",
  "data": {
    "results": [
      {
        "id": "uuid",
        "content": "the matched memory text",
        "source": "user",
        "score": 0.923,
        "metadata": { "session_id": "...", "ts": "..." },
        "created_at": "2026-02-22T..."
      }
    ],
    "query": "what did we talk about yesterday?",
    "count": 5
  }
}
```

## 🗃️ pgvector Query

```sql
SELECT 
    id,
    content,
    source,
    metadata,
    created_at,
    1 - (vector <=> $1::vector) AS score
FROM memory_vectors
WHERE vector IS NOT NULL
ORDER BY vector <=> $1::vector
LIMIT $2
```

The `<=>` operator is pgvector cosine distance. `1 - distance = similarity score (0–1)`.

## 🧪 Acceptance Criteria (TDD — Red first!)

1. **Test: search returns top-K results ordered by similarity**
   - Insert 3 test rows with known vectors
   - Query with a vector close to one of them
   - Assert results are returned in cosine similarity order, closest first
2. **Test: search embeds query with RETRIEVAL_QUERY task type**
   - Mock Gemini API
   - Assert the embed call uses `taskType: RETRIEVAL_QUERY` (not `RETRIEVAL_DOCUMENT`)
3. **Test: search skips rows with NULL vectors**
   - Insert a row without a vector
   - Assert it never appears in search results
4. **Test: search with no results returns empty array (not error)**
   - Empty DB → `{results: [], count: 0}` with `status: "ok"`
5. **Test: limit is respected**
   - Insert 10 rows → search with `limit: 3` → assert exactly 3 returned
6. **Test: missing query returns error**
   - `EthosRequest::Search { query: "", limit: 5 }` → `{status: "error"}`
7. **Coverage:** >90% via `cargo tarpaulin`
8. **Runbook:** `docs/runbooks/retrieval.md` created

## 🛠️ Implementation Notes

- **Embedding the query:** use `GeminiEmbeddingClient::embed_with_task(query, TaskType::RetrievalQuery)`
- **pgvector binding:** `pgvector::Vector::from(vec_of_f32)` — same as embedder
- **Score threshold:** no minimum score filter for now (return whatever pgvector gives)
- **Default limit:** 5 if not provided, max 20
- **Graceful degradation:** if embedding fails, return error response (don't return unranked results)

## IPC type additions (ethos-core/src/ipc.rs)

```rust
// Add to EthosRequest enum:
Search {
    query: String,
    limit: Option<i64>,
}

// Add SearchResult struct:
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: uuid::Uuid,
    pub content: String,
    pub source: String,
    pub score: f64,
    pub metadata: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}
```

## ✅ Done Checklist
Complete `/home/revenantpulse/Projects/DONE_CHECKLIST.md` before logging shipped.

---
*Spec by Neko — Story 006. The one where Ethos gets a voice.*
