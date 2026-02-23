# Code Review — Story 002: Rust Project Scaffold
**Reviewer:** Sage (initial) + Neko (fix verification)
**Date:** 2026-02-22  
**Status:** ✅ APPROVED (after fixes)

## Summary
The Rust workspace scaffold is solid. Three crates (`ethos-core`, `ethos-server`, `ethos-ingest`) are correctly structured, all dependencies are correct, DB health check passes, and the Unix socket IPC server is live. Sage caught two issues in initial review — both fixed within the same session.

## cargo audit
`RUSTSEC-2023-0071` (rsa 0.9.10, Marvin Attack timing side-channel) — **documented ignore**.
- `rsa` appears in Cargo.lock as a stale transitive entry via `sqlx-mysql`
- `cargo tree -i rsa` and `cargo tree -i sqlx-mysql` both return empty — not in active dep tree
- We disabled MySQL backend (`sqlx default-features = false, postgres-only`)
- No fix available upstream; not applicable to our use case
- Documented in `audit.toml` with rationale
- **Effective result: No actionable vulnerabilities**

## Scope Assessment
- Workspace & 3 crates: ✅
- All dependencies at correct versions: ✅
- DB health check passes (PG17.7 + pgvector 0.8.0): ✅
- Unix socket IPC server: ✅
- `ethos.toml` config: ✅
- No scope creep: ✅

## Security Findings (initial → fixed)

| Severity | Issue | File:Line | Status |
|----------|-------|-----------|--------|
| **Medium** | IPC length prefix was Big Endian — spec requires Little Endian | `ethos-server/src/server.rs:29-30` | ✅ **FIXED** — changed to `LengthDelimitedCodec::builder().little_endian().new_codec()` |
| **Low** | `rmp_serde::to_vec(...).unwrap()` in connection handler — potential panic | `ethos-server/src/server.rs:40,47` | ✅ **FIXED** — proper `Result` handling with `tracing::error!` and graceful connection close |

## Quality Notes
- Clean `cargo clippy -- -D warnings` pass
- Good use of `tracing` throughout (no `println!` in lib code)
- Architecture matches spec faithfully

## Decision
**APPROVED** ✅ — Story 002 complete, review gate passed, ready for Story 003.
