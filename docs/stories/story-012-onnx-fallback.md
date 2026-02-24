# Story 012 — ONNX Fallback Embedder

**Status:** Ready for Implementation  
**Assigned:** Forge  
**Reviewer:** Sage  
**Priority:** P1 — Resilience / Offline Support

---

## Overview

Ethos currently uses the Gemini API (`gemini-embedding-001`, 768-dim) as its only embedding backend. If the API is unavailable (no key, network outage, rate limit, offline deployment), Ethos fails to embed and silently drops memories.

This story adds:

1. **An `EmbeddingBackend` trait** — a clean abstraction over embedding providers
2. **A local ONNX backend** — using `ort` + `all-MiniLM-L6-v2` (384-dim, runs fully offline)
3. **Explicit `backend` config support** — `"gemini"`, `"onnx"`, or `"gemini-fallback-onnx"`
4. **Graceful degraded mode** — when Gemini fails in `gemini-fallback-onnx` mode, memories are stored without embeddings (keyword/BM25 search still works; vector similarity degrades gracefully)

---

## Design Notes

### The Dimension Mismatch Problem

Gemini outputs 768-dim vectors. `all-MiniLM-L6-v2` outputs 384-dim vectors. pgvector columns are typed to a fixed dimension.

**We do NOT mix dimension spaces in the same DB column.** Mixing would corrupt search rankings.

Instead, each backend runs in isolation:

| Backend | Dimensions | pgvector column | Use case |
|---------|-----------|-----------------|----------|
| `"gemini"` | 768 | `embedding vector(768)` | Default, cloud, best quality |
| `"onnx"` | 384 | `embedding vector(384)` | Explicit offline / no API key |
| `"gemini-fallback-onnx"` | 768 (primary) | `embedding vector(768)` | Cloud with graceful fallback — Gemini fails → store `NULL` embedding → keyword search |

> **`gemini-fallback-onnx` stores NULL on Gemini failure, not ONNX vectors.**  
> This keeps the DB consistent. Keyword (BM25/pg_trgm) search still works. When Gemini comes back, the next ingest or a background re-embed job fills the gaps.

The `onnx` backend requires a fresh DB or migration to resize the vector column. This is documented in the runbook.

---

## Implementation Plan

### 1. `EmbeddingBackend` Trait (`ethos-core/src/embeddings.rs`)

```rust
#[async_trait::async_trait]
pub trait EmbeddingBackend: Send + Sync {
    /// Embed a single text. Returns None if embedding is unavailable
    /// (used in fallback mode to signal graceful degradation).
    async fn embed(&self, text: &str) -> Result<Option<Vec<f32>>, EmbeddingError>;

    /// Returns the embedding dimension (e.g., 768 or 384)
    fn dimensions(&self) -> usize;

    /// Backend name for logging
    fn name(&self) -> &str;
}
```

Refactor existing `GeminiEmbeddingClient` to implement `EmbeddingBackend`.

---

### 2. `OnnxEmbeddingClient` (`ethos-core/src/onnx_embedder.rs`)

**Crate deps to add:**
```toml
# ethos-core/Cargo.toml
ort = { version = "2", features = ["load-dynamic"] }
tokenizers = { version = "0.20", default-features = false, features = ["http"] }
```

**Model:** `all-MiniLM-L6-v2` (22MB ONNX, 384-dim)  
**Download source:** HuggingFace — `sentence-transformers/all-MiniLM-L6-v2`

**Model loading:**
- Look for model file at `ethos.toml` `[embedding] onnx_model_path` (absolute path)
- If not set, default to `~/.local/share/ethos/models/all-MiniLM-L6-v2.onnx`
- If file doesn't exist → return `EmbeddingError::ModelNotFound` with download instructions

**Inference pipeline:**
1. Tokenize input text (`tokenizers` crate, BPE tokenizer bundled or downloaded alongside model)
2. Run ONNX session: inputs = `input_ids`, `attention_mask`, `token_type_ids`
3. Extract last hidden state → mean-pool over sequence length → normalize to unit vector
4. Return `Vec<f32>` of length 384

**Tokenizer:**
- Download `tokenizer.json` from HuggingFace alongside the ONNX model
- Path: `~/.local/share/ethos/models/all-MiniLM-L6-v2-tokenizer.json`

---

### 3. `FallbackEmbeddingClient` (`ethos-core/src/embeddings.rs`)

Wraps the Gemini client. On any `EmbeddingError`, logs a warning and returns `Ok(None)`.

```rust
pub struct FallbackEmbeddingClient {
    inner: GeminiEmbeddingClient,
}

impl FallbackEmbeddingClient {
    pub fn new(config: EmbeddingConfig) -> Result<Self, EmbeddingError> { ... }
}

#[async_trait]
impl EmbeddingBackend for FallbackEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
        match self.inner.embed(text).await {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                tracing::warn!(error = %e, "Gemini embedding failed — storing memory without embedding (keyword search only)");
                Ok(None)
            }
        }
    }
    fn dimensions(&self) -> usize { 768 }
    fn name(&self) -> &str { "gemini-fallback-onnx" }
}
```

---

### 4. Backend Factory (`ethos-core/src/embeddings.rs`)

```rust
pub enum BackendConfig {
    Gemini(EmbeddingConfig),
    Onnx(OnnxConfig),
    GeminiFallbackOnnx(EmbeddingConfig),
}

pub fn create_backend(config: BackendConfig) -> Result<Box<dyn EmbeddingBackend>, EmbeddingError> {
    match config {
        BackendConfig::Gemini(c) => Ok(Box::new(GeminiEmbeddingClient::new(c)?)),
        BackendConfig::Onnx(c) => Ok(Box::new(OnnxEmbeddingClient::new(c)?)),
        BackendConfig::GeminiFallbackOnnx(c) => Ok(Box::new(FallbackEmbeddingClient::new(c)?)),
    }
}
```

---

### 5. Config Changes (`ethos.toml`)

```toml
[embedding]
# Options: "gemini" | "onnx" | "gemini-fallback-onnx"
backend = "gemini-fallback-onnx"

# Gemini settings (used when backend includes gemini)
gemini_model = "gemini-embedding-001"
gemini_dimensions = 768

# ONNX settings (used when backend = "onnx")
onnx_model_path = ""  # defaults to ~/.local/share/ethos/models/all-MiniLM-L6-v2.onnx
onnx_dimensions = 384

# Shared
batch_size = 32
batch_timeout_seconds = 5
queue_capacity = 1000
rate_limit_rpm = 15
```

Update `EthosConfig` struct in `ethos-core/src/config.rs` to parse these fields.

---

### 6. DB Migration for ONNX Mode

Create migration `migrations/20260224000000_onnx_dimension_option.sql`:

```sql
-- This migration is only needed if switching backend from "gemini" to "onnx"
-- It resizes the embedding column from 768 to 384 dimensions.
-- WARNING: Destroys existing embeddings. Re-embed after applying.
-- Only apply if [embedding] backend = "onnx" in ethos.toml.

-- NOT applied automatically. Run manually: sqlx migrate run --target-version 20260224000000
ALTER TABLE episodic_memories 
  ALTER COLUMN embedding TYPE vector(384);
ALTER TABLE semantic_facts
  ALTER COLUMN embedding TYPE vector(384);
```

Document this in the runbook.

---

### 7. Model Download Helper Script

Create `scripts/download-onnx-model.sh`:

```bash
#!/usr/bin/env bash
# Downloads all-MiniLM-L6-v2 ONNX model and tokenizer from HuggingFace
MODEL_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/ethos/models"
mkdir -p "$MODEL_DIR"

BASE="https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main"

echo "Downloading ONNX model..."
curl -L "$BASE/onnx/model.onnx" -o "$MODEL_DIR/all-MiniLM-L6-v2.onnx"

echo "Downloading tokenizer..."
curl -L "$BASE/tokenizer.json" -o "$MODEL_DIR/all-MiniLM-L6-v2-tokenizer.json"

echo "Done. Model saved to: $MODEL_DIR"
```

---

## Files to Create / Modify

| File | Action |
|------|--------|
| `ethos-core/src/embeddings.rs` | Add `EmbeddingBackend` trait, `FallbackEmbeddingClient`, `create_backend()` factory; refactor `GeminiEmbeddingClient` to implement trait |
| `ethos-core/src/onnx_embedder.rs` | New — `OnnxEmbeddingClient` implementation |
| `ethos-core/src/config.rs` | Add `onnx_model_path`, update `backend` field parsing |
| `ethos-core/Cargo.toml` | Add `ort`, `tokenizers` dependencies |
| `ethos-server/src/subsystems/embedder.rs` | Use `Box<dyn EmbeddingBackend>` instead of concrete type |
| `migrations/20260224000000_onnx_dimension_option.sql` | New — resize migration (manual only) |
| `scripts/download-onnx-model.sh` | New — model download helper |
| `ethos.toml.example` | Update with new config fields |
| `docs/runbooks/embedder.md` | Update with ONNX setup instructions |

---

## Acceptance Criteria

- [ ] `EmbeddingBackend` trait exists and both `GeminiEmbeddingClient` and `OnnxEmbeddingClient` implement it
- [ ] `FallbackEmbeddingClient` returns `Ok(None)` on Gemini error (no panic, no hard failure)
- [ ] With `backend = "onnx"` and model file present: `embed("hello world")` returns 384-dim vector
- [ ] With `backend = "onnx"` and model file missing: returns `EmbeddingError::ModelNotFound` with clear message
- [ ] With `backend = "gemini-fallback-onnx"` and Gemini unavailable: memory is stored with `NULL` embedding; retrieval falls back to keyword search
- [ ] `cargo test` passes (all existing embedder tests still pass; new ONNX tests added)
- [ ] `scripts/download-onnx-model.sh` runs successfully and places files in the right location
- [ ] `ethos.toml.example` updated; `docs/runbooks/embedder.md` updated with ONNX setup steps

---

## Out of Scope

- Auto-download of ONNX model at startup (use the script)
- Background re-embedding of NULL-embedding memories when Gemini comes back online (Story 013)
- Support for other ONNX models (users can swap the file, but only `all-MiniLM-L6-v2` is tested)
- Quantized / INT8 ONNX models

---

## References

- Current embedder: `ethos-core/src/embeddings.rs`
- Embedder subsystem: `ethos-server/src/subsystems/embedder.rs`
- Config struct: `ethos-core/src/config.rs`
- `ort` crate: https://docs.rs/ort/latest/ort/
- `tokenizers` crate: https://docs.rs/tokenizers/latest/tokenizers/
- Model: https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2
