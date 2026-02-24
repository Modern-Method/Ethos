-- Migration: Resize embedding columns for ONNX backend (384-dim)
--
-- This migration is ONLY needed when switching from "gemini" to "onnx" backend.
-- It resizes the embedding column from vector(768) to vector(384).
--
-- WARNING: This DESTROYS existing embeddings. After applying, re-embed all rows
-- using the ONNX backend. See docs/runbooks/embedder.md for instructions.
--
-- NOT applied automatically by `sqlx migrate run`.
-- Apply manually: sqlx migrate run --target-version 20260224000000

ALTER TABLE episodic_traces
  ALTER COLUMN embedding TYPE vector(384);

ALTER TABLE semantic_facts
  ALTER COLUMN embedding TYPE vector(384);
