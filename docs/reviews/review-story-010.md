# Code Review — Story 010: Ebbinghaus Decay + LTP
**Reviewer:** Sage  
**Date:** 2026-02-23  
**Status:** APPROVED

## Summary
The Decay subsystem correctly implements the Ebbinghaus forgetting curve with Long-Term Potentiation (LTP). The decay sweep runs across all memory types (`memory_vectors`, `episodic_traces`, `semantic_facts`) and correctly soft-deletes (prunes) items falling below the salience threshold. Retrieval events boost memory strength via the `record_retrieval` hook, implementing the LTP requirement.

## cargo audit
Clean — no advisories (checked via `cargo audit`).

## Scope Assessment
**PASS**
- `decay.rs` implements the full Ebbinghaus formula with frequency and emotional weighting.
- LTP logic (`tau_eff` extension) is correct: `base_tau * ltp_multiplier^retrieval_count`.
- `record_retrieval` hook implemented for all memory types.
- Pruning uses soft-delete (`pruned = true`) as requested.
- Integration tests confirm LTP prevents pruning of old but frequently accessed memories.

## Security Findings
| Severity | Issue | File:Line | Recommendation |
|----------|-------|-----------|----------------|
| Info     | `unsafe` usage | None | No `unsafe` blocks found. |
| Info     | SQL Injection | `subsystems/decay.rs` | All queries use `sqlx::query!` macros or parameterized bindings. Safe. |
| Info     | Secrets | None | No hardcoded secrets found. |
| Info     | Performance | `run_decay_sweep` | Uses `LIMIT 500` batching to prevent table locking issues during sweeps. |

## Quality Notes
- **Formula Verification:** `calculate_salience` is implemented as a pure function, making it easily testable and verifiable against the spec.
- **Robustness:** Handles missing `last_accessed` dates gracefully (defaults to `created_at`).
- **Completeness:** Covers `memory_vectors`, `episodic_traces`, and `semantic_facts` with specific logic for each (e.g., confidence vs salience for facts).

## Decision
**APPROVED** — ready for Michael review.
