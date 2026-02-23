# Retrieval Runbook

> Story 006: Semantic Search Implementation
> Last updated: 2026-02-22

## Overview

The retrieval subsystem implements `EthosRequest::Search` — semantic search over memory vectors using pgvector cosine similarity.

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   IPC Client    │────▶│  Ethos Server   │────▶│   PostgreSQL    │
│  (search req)   │     │  (router.rs)    │     │   (pgvector)    │
└─────────────────┘     └────────┬────────┘     └─────────────────┘
                                 │
                                 ▼
                        ┌─────────────────┐
                        │ Gemini Embedding │
                        │  (RETRIEVAL_QUERY)│
                        └─────────────────┘
```

## API Contract

### Request

```json
{
  "action": "search",
  "query": "what did we discuss about Rust?",
  "limit": 5
}
```

### Response (Success)

```json
{
  "status": "ok",
  "data": {
    "results": [
      {
        "id": "uuid",
        "content": "matched memory text",
        "source": "user",
        "score": 0.923,
        "metadata": {},
        "created_at": "2026-02-22T..."
      }
    ],
    "query": "what did we discuss about Rust?",
    "count": 5
  },
  "version": "0.1.0"
}
```

### Response (Error)

```json
{
  "status": "error",
  "error": "Query cannot be empty",
  "data": null,
  "version": "0.1.0"
}
```

## Query Flow

1. **Validate query** — empty/whitespace queries return error
2. **Clamp limit** — default 5, max 20, min 1
3. **Embed query** — call Gemini with `TaskType::RetrievalQuery`
4. **pgvector search** — cosine similarity via `<=>` operator
5. **Return results** — ordered by score descending

## pgvector Query

```sql
SELECT 
    id,
    content,
    source,
    1 - (vector <=> $1::vector) AS score,
    metadata,
    created_at
FROM memory_vectors
WHERE vector IS NOT NULL
ORDER BY vector <=> $1::vector
LIMIT $2
```

- `<=>` is pgvector's cosine distance operator
- Score = `1 - distance` (range 0-1, higher is better)
- Only rows with non-NULL vectors are returned

## Constraints

| Constraint | Value |
|------------|-------|
| Default limit | 5 |
| Max limit | 20 |
| Min limit | 1 |
| Task type | `RETRIEVAL_QUERY` (NOT `RETRIEVAL_DOCUMENT`) |
| Score range | 0.0 - 1.0 |

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Empty query | Return `{status: "error", error: "Query cannot be empty"}` |
| Embedding failure | Return `{status: "error", error: "Failed to embed query: ..."}` |
| No results | Return `{status: "ok", data: {results: [], count: 0}}` |
| Database error | Propagate error to caller |

## Testing

Run unit tests:
```bash
cargo test --package ethos-server --lib subsystems::retrieve::tests
```

Run with coverage:
```bash
cargo tarpaulin --package ethos-server --lib
```

### Test Coverage

| Test | Description |
|------|-------------|
| `test_search_returns_top_k_ordered_by_similarity` | Results ordered by score desc |
| `test_search_uses_retrieval_query_task_type` | Embeds with RETRIEVAL_QUERY |
| `test_search_skips_null_vectors` | Excludes NULL vector rows |
| `test_search_empty_results_returns_ok_with_empty_array` | Empty = ok, not error |
| `test_search_respects_limit` | Limit honored |
| `test_search_empty_query_returns_error` | Empty query = error |
| `test_search_limit_clamped_to_max_20` | Max 20 enforced |
| `test_search_default_limit_is_5` | Default is 5 |
| `test_search_embedding_failure_returns_error` | Graceful on embed fail |
| `test_search_scores_in_valid_range` | Scores 0-1 |

## Monitoring

Key metrics to monitor:
- Search latency (embedding + pgvector)
- Embedding API error rate
- pgvector query time
- Result count distribution

## Troubleshooting

### No results returned

1. Check if vectors exist: `SELECT COUNT(*) FROM memory_vectors WHERE vector IS NOT NULL;`
2. Verify embedding pipeline ran: `SELECT COUNT(*) FROM memory_vectors WHERE vector IS NULL;`
3. Run embedder: `EthosRequest::Embed { id }` for pending rows

### Slow queries

1. Check HNSW index exists: `\d memory_vectors` (look for `idx_vectors_hnsw`)
2. Verify query uses index: `EXPLAIN ANALYZE SELECT ...`
3. Consider adjusting HNSW parameters (`ef_search`)

### Embedding failures

1. Check API key: `GOOGLE_API_KEY` environment variable
2. Check API quotas: Gemini rate limits
3. Check logs for specific error codes

## Related Documentation

- [Story 006 Spec](../stories/story-006-retrieval.md)
- [Embedder Runbook](./embedder.md) (if exists)
- [pgvector Documentation](https://github.com/pgvector/pgvector)
