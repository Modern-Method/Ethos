# Runbook: Embedder Subsystem

## Overview
The Embedder subsystem transforms raw text content into semantic vectors for similarity search. It supports multiple backends:

| Backend | Dimensions | Description |
|---------|-----------|-------------|
| `gemini` | 768 | Cloud embeddings via Gemini API (default) |
| `onnx` | 384 | Local embeddings via `all-MiniLM-L6-v2` (offline) |
| `gemini-fallback-onnx` | 768 | Gemini primary; stores NULL on failure (keyword search still works) |

## Configuration

### `ethos.toml`

```toml
[embedding]
backend = "gemini"              # or "onnx" or "gemini-fallback-onnx"
gemini_model = "gemini-embedding-001"
gemini_dimensions = 768
onnx_model_path = ""            # empty = default (~/.local/share/ethos/models/)
onnx_dimensions = 384
```

### Environment Variables

| Variable | Used by | Description |
|----------|---------|-------------|
| `GOOGLE_API_KEY` | `gemini`, `gemini-fallback-onnx` | Gemini API key |

## Backend Setup

### Gemini (default)

1. Set `GOOGLE_API_KEY` in your environment.
2. Set `backend = "gemini"` in `ethos.toml`.
3. No other setup needed.

### ONNX (offline)

1. Download the model and tokenizer:
   ```bash
   ./scripts/download-onnx-model.sh
   ```
2. Set `backend = "onnx"` in `ethos.toml`.
3. If the DB was previously used with Gemini (768-dim vectors), you must resize the columns:
   ```bash
   sqlx migrate run --target-version 20260224000000
   ```
   **WARNING:** This destroys existing embeddings. Re-embed all rows after migrating.
4. Install the ONNX Runtime shared library (`libonnxruntime.so`) on the host. The `ort` crate loads it dynamically.

### Gemini-fallback-ONNX (resilient cloud)

1. Set `GOOGLE_API_KEY` in your environment.
2. Set `backend = "gemini-fallback-onnx"` in `ethos.toml`.
3. When Gemini is unavailable, memories are stored with `NULL` embedding. Keyword/BM25 search still works. When Gemini recovers, future ingests get embeddings normally.

**Note:** This mode does NOT produce ONNX embeddings. The "fallback" means graceful degradation to NULL, not switching to ONNX vectors. This prevents dimension mismatches in the DB.

## Operational Flows

### 1. Ingest Trigger
When content is ingested via the IPC `Ingest` request:
1. Data is written to `session_events` and `memory_vectors` (vector is NULL).
2. A `tokio::spawn` task is created to fetch the embedding.
3. The configured backend is called (with retries for Gemini).
4. On success, the `vector` column is updated.
5. In `gemini-fallback-onnx` mode, failure leaves vector NULL (no crash).

### 2. Manual Trigger
A manual embedding can be triggered for any row by its UUID:
```bash
# Example IPC Request
{ "action": "embed", "id": "uuid-here" }
```

## Troubleshooting

### Vector Stays NULL
- **Gemini backend:** Check logs for `Gemini API error` or `All embedding retry attempts failed`. Ensure `GOOGLE_API_KEY` is set.
- **ONNX backend:** Check logs for `ModelNotFound`. Run `scripts/download-onnx-model.sh`. Ensure `libonnxruntime.so` is installed.
- **Fallback backend:** NULL embedding is expected behavior when Gemini is unavailable. Check logs for the `Gemini embedding failed — storing memory without embedding` warning.

### ONNX Model Not Found
If you see `ONNX model not found at <path>`:
```bash
./scripts/download-onnx-model.sh
```
Or set `onnx_model_path` in `ethos.toml` to the absolute path of your model file.

### Dimension Mismatch After Backend Switch
If switching from `gemini` (768-dim) to `onnx` (384-dim):
1. Apply the manual migration: `sqlx migrate run --target-version 20260224000000`
2. Re-embed all rows.

Do **not** mix backends against the same DB column — vector similarity scores will be meaningless.

### Performance Issues
- The embedder runs asynchronously and does not block the IPC handler.
- If the Gemini API is rate-limiting (429), the subsystem will automatically back off.
- ONNX inference runs on the Tokio blocking thread pool and does not block async tasks.
