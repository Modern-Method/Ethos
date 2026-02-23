# Runbook: Embedder Subsystem

## Overview
The Embedder subsystem transforms raw text content into semantic vectors using the Gemini Embeddings API (`gemini-embedding-001`). These vectors are stored in the `vector` column of the `memory_vectors` table to enable similarity search.

## Configuration
The embedder reads configuration from `ethos.toml` and environment variables:

| Key | Description | Default |
|-----|-------------|---------|
| `GOOGLE_API_KEY` | Gemini API Key (Env Var) | Required |
| `embedding.gemini_model` | Model name | `gemini-embedding-001` |
| `embedding.gemini_dimensions` | Vector dimensions | `768` |

## Operational Flows

### 1. Ingest Trigger
When content is ingested via the IPC `Ingest` request:
1. Data is written to `session_events` and `memory_vectors` (vector is NULL).
2. A `tokio::spawn` task is created to fetch the embedding.
3. The API is called with up to 3 retries (exponential backoff).
4. On success, the `vector` column is updated.

### 2. Manual Trigger
A manual embedding can be triggered for any row by its UUID:
```bash
# Example IPC Request
{ "action": "embed", "id": "uuid-here" }
```

## Troubleshooting

### Vector Stays NULL
- Check logs for `Gemini API error` or `All embedding retry attempts failed`.
- Ensure `GOOGLE_API_KEY` is set and valid.
- Verify the content in `memory_vectors` is not empty.

### Performance Issues
- The embedder runs asynchronously and does not block the IPC handler.
- If the API is rate-limiting (429), the subsystem will automatically back off.
- Gemini free tier has significant rate limits; check your GCP quota.

### Schema Mismatches
- If you change the embedding model to one with different dimensions (e.g., 384), you MUST run a migration to change the `vector(768)` column type.
