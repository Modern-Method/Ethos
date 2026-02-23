# Code Review — Story 011: REST API + QMD Wire Protocol
**Reviewer:** Sage  
**Date:** 2026-02-23  
**Status:** APPROVED

## Summary
The HTTP REST API (`ethos-server/src/http.rs`) and `ethos-cli` binary successfully implement the requirements for Story 011. The server exposes health, search, ingest, and consolidate endpoints via Axum, running alongside the IPC server. The CLI tool correctly formats search results to match the QMD wire protocol (`--json` output), enabling drop-in replacement for OpenClaw's memory search.

## cargo audit
Clean — no advisories (checked via `cargo audit`).

## Scope Assessment
**PASS**
- **HTTP Server:** Implemented on port 8766 (configurable) with all required endpoints.
- **Inner Functions:** Business logic separated into `_inner` functions for direct testing (improves tarpaulin accuracy).
- **CLI Tool:** `ethos-cli` accepts `search` and `query` subcommands and outputs QMD-compatible JSON.
- **Config:** `HttpConfig` added to `EthosConfig`.

## Security Findings
| Severity | Issue | File:Line | Recommendation |
|----------|-------|-----------|----------------|
| Info     | `unsafe` usage | None | No `unsafe` blocks found. |
| Info     | SQL Injection | `http.rs` | Delegates to `router.rs`, which uses parameterized queries. Safe. |
| Info     | Input Validation | `search_inner` | Validates query is not empty/whitespace before processing. |
| Info     | Denial of Service | `search_inner` | `limit` parameter passed to IPC router (clamped to 20 in retrieval subsystem). |

## Quality Notes
- **Testing Strategy:** Excellent use of "inner" functions (`health_inner`, `search_inner`) allows unit testing of endpoint logic without spinning up the full Axum stack, resulting in fast and reliable tests.
- **Error Handling:** Standardized `ErrorResponse` struct ensures consistent JSON error format.
- **Integration:** The `ethos-cli` implementation uses `reqwest::blocking` as requested, keeping the CLI simple and robust.

## Decision
**APPROVED** — ready for Michael review.
