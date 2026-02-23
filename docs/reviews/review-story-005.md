# Code Review — Story 005: Embedder
**Reviewer:** Sage  
**Date:** 2026-02-22  
**Status:** APPROVED

## Summary
The Embedder subsystem is well-implemented. It correctly integrates with the Gemini API, handles retries with exponential backoff, and writes vectors back to PostgreSQL using `pgvector`. The asynchronous architecture ensures the IPC loop is never blocked.

## cargo audit
Clean (no vulnerabilities found).

## Scope Assessment
- **New Files**: `ethos-server/src/subsystems/embedder.rs` and `ethos-core/src/embeddings.rs` implemented. ✅
- **API Integration**: Correctly targets `gemini-embedding-001` with `RETRIEVAL_DOCUMENT` task type. ✅
- **Dimensions**: Enforces 768 dimensions. ✅
- **Async/Non-blocking**: Uses `tokio::spawn` for embedding tasks. ✅
- **Retries**: Implements exponential backoff for API failures. ✅
- **DB Updates**: Writes vectors correctly using `pgvector`. ✅

## Security Findings
| Severity | Issue | File:Line | Recommendation |
|----------|-------|-----------|----------------|
| Info | None | - | No hardcoded secrets or injection risks found. API keys are handled via config/env vars. |

## Quality Notes
- **Error Handling**: Excellent usage of `anyhow` and `thiserror`. The system fails gracefully (logs error, leaves vector NULL) rather than crashing.
- **Testing**: Comprehensive unit and integration tests using `wiremock` cover both success and error scenarios (429/500).
- **Optimization**: `embed_by_id` checks if the vector is already populated before calling the API, saving costs.

## Decision
**APPROVED**
