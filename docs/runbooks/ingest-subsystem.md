# Runbook: Ingest Subsystem

## Overview
The Ingest subsystem is responsible for receiving raw events from OpenClaw (via the `ethos-ingest` hook) and persisting them to the PostgreSQL database. It acts as the "write-ahead log" for Ethos memory before consolidation.

## Data Flow
1. **Source:** `ethos-ingest` (TypeScript) sends MessagePack encoded payloads over a Unix Domain Socket.
2. **Server:** `ethos-server` decodes the payload into an `EthosRequest::Ingest`.
3. **Subsystem:** `ingest_payload` extracts content and metadata.
4. **Database:** Two records are created in a single transaction:
    - `session_events`: Raw log of the conversation.
    - `memory_vectors`: Placeholder for semantic memory (vector is NULL until processed by the embedder).

## Database Tables
- `session_events`: Stores `session_id`, `agent_id`, `role`, `content`, and `metadata`.
- `memory_vectors`: Stores `content`, `source`, and `metadata`.

## Mapping Logic
- **Role Mapping:**
    - `user` -> `user`
    - `assistant` -> `assistant`
    - `system` -> `system`
    - `tool` -> `tool`
- **Session ID:** Extracted from `metadata.session_id`, defaults to `default`.
- **Agent ID:** Extracted from `metadata.agent_id`, defaults to `ethos`.
- **Author:** Extracted from `metadata.author`, defaults to the source.

## Troubleshooting

### DB Errors
If ingestion fails with a DB error, check the `ethos-server` logs for `sqlx` errors. Ensure the `session_events` and `memory_vectors` tables exist and the `ethos` user has write permissions.

### Missing Metadata
If `session_id` or `agent_id` are missing, the system uses safe defaults. Check the `metadata` column in the database to see the raw payload received.

### Connection Issues
If the TypeScript hook cannot connect, ensure the `ethos-server` is running and the Unix socket path matches the configuration in `ethos.toml`.

## Testing
Run integration tests to verify the pipeline:
```bash
cargo test --test ingest_integration
```
