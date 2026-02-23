# Story 002 — Rust Project Scaffold

**Epic:** Ethos MVP — Foundation  
**Story:** Rust workspace scaffold (Cargo workspace, crates, DB connectivity, IPC listener)  
**Status:** Ready for implementation  
**Assigned:** Forge  
**Date:** 2026-02-22  
**Depends on:** Story 001 ✅ (DB schema deployed)

---

## Context

Ethos is a Rust + Python memory service for OpenClaw. It replaces the QMD memory backend with a neuromorphic, brain-inspired memory engine. This story creates the complete Rust project scaffold — the workspace, crates, dependencies, module skeleton, configuration, and a working end-to-end smoke test that proves Rust can connect to our PostgreSQL instance.

**Read these before starting:**
- `/home/revenantpulse/Projects/ethos/docs/gap-resolution.md` — full architecture spec (REQUIRED READING)
- `/home/revenantpulse/Projects/ethos/migrations/001_initial_schema.sql` — DB schema already deployed
- DB connection string: `postgresql://ethos:ethos_dev@localhost:5432/ethos`

---

## Acceptance Criteria

1. **Cargo workspace** at `/home/revenantpulse/Projects/ethos/` with `Cargo.toml`
2. **3 crates** scaffolded:
   - `ethos-core` — shared types, config, DB pool, error types
   - `ethos-server` — main service binary (IPC listener, subsystem orchestration)
   - `ethos-ingest` — ingestion pipeline stub (writes to session buffer)
3. **All dependencies** declared with correct versions (see spec below)
4. **`cargo build`** succeeds with no errors
5. **`cargo clippy`** passes with no warnings (or warnings suppressed with reason)
6. **DB connectivity verified** — `cargo run --bin ethos-server -- --health` connects to PostgreSQL, runs `SELECT version()`, prints PG version + pgvector version, exits cleanly
7. **Config loading** — `ethos.toml` is read at startup; missing file produces a helpful error
8. **Unix socket** — server starts listening on `/tmp/ethos.sock`, accepts connections, responds to a `{"action":"ping"}` message with `{"status":"ok","version":"0.1.0"}`
9. **Graceful shutdown** — Ctrl+C triggers clean shutdown (closes DB pool, removes socket file)
10. **README.md** updated with build/run instructions

---

## Architecture (from gap-resolution.md)

```
ethos/
├── Cargo.toml              ← workspace root
├── ethos.toml              ← runtime config (TOML)
├── ethos-core/             ← shared library crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── config.rs       ← Config struct + TOML loading
│       ├── db.rs           ← PgPool creation + health check
│       ├── error.rs        ← EthosError enum (thiserror)
│       ├── models/         ← DB model types (mirrors our 6 tables)
│       │   ├── mod.rs
│       │   ├── session.rs
│       │   ├── episode.rs
│       │   ├── fact.rs
│       │   ├── workflow.rs
│       │   ├── vector.rs
│       │   └── graph.rs
│       └── ipc.rs          ← IPC message types (Request/Response enums)
├── ethos-server/           ← main binary crate
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs         ← entrypoint, tokio::main, arg parsing
│       ├── server.rs       ← UnixListener + connection handler
│       ├── router.rs       ← dispatches IPC requests to subsystems
│       └── subsystems/
│           ├── mod.rs
│           ├── ingest.rs   ← STUB: receives messages, writes to session buffer
│           ├── consolidate.rs  ← STUB: DMN consolidation tick
│           └── retrieve.rs ← STUB: memory retrieval handler
└── ethos-ingest/           ← ingestion pipeline library crate (future: called by hook)
    ├── Cargo.toml
    └── src/
        ├── lib.rs
        └── pipeline.rs     ← STUB: IngestPipeline struct
```

---

## Dependencies (Exact Versions)

### Workspace `Cargo.toml`

```toml
[workspace]
members = ["ethos-core", "ethos-server", "ethos-ingest"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
authors = ["Modern Method Inc."]
license = "UNLICENSED"

[workspace.dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Database
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "uuid", "chrono", "json"] }

# pgvector extension for sqlx
pgvector = { version = "0.4", features = ["sqlx"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rmp-serde = "1"           # MessagePack for IPC

# Config
config = { version = "0.14", features = ["toml"] }

# Error handling
thiserror = "1"
anyhow = "1"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Utilities
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
tokio-util = { version = "0.7", features = ["codec"] }

# Python FFI (for spreading activation — Phase 1)
pyo3 = { version = "0.21", features = ["auto-initialize"] }
```

### `ethos-core/Cargo.toml`

```toml
[package]
name = "ethos-core"
version.workspace = true
edition.workspace = true

[dependencies]
tokio.workspace = true
sqlx.workspace = true
pgvector.workspace = true
serde.workspace = true
serde_json.workspace = true
rmp-serde.workspace = true
config.workspace = true
thiserror.workspace = true
anyhow.workspace = true
tracing.workspace = true
uuid.workspace = true
chrono.workspace = true
```

### `ethos-server/Cargo.toml`

```toml
[package]
name = "ethos-server"
version.workspace = true
edition.workspace = true

[[bin]]
name = "ethos-server"
path = "src/main.rs"

[dependencies]
ethos-core = { path = "../ethos-core" }
ethos-ingest = { path = "../ethos-ingest" }
tokio.workspace = true
sqlx.workspace = true
serde.workspace = true
serde_json.workspace = true
rmp-serde.workspace = true
thiserror.workspace = true
anyhow.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
uuid.workspace = true
chrono.workspace = true
tokio-util.workspace = true
```

### `ethos-ingest/Cargo.toml`

```toml
[package]
name = "ethos-ingest"
version.workspace = true
edition.workspace = true

[dependencies]
ethos-core = { path = "../ethos-core" }
tokio.workspace = true
sqlx.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
anyhow.workspace = true
tracing.workspace = true
uuid.workspace = true
chrono.workspace = true
```

---

## Key Implementations

### 1. `ethos-core/src/config.rs`

Loads `ethos.toml` using the `config` crate. The full TOML schema is in `gap-resolution.md §5`. At minimum implement:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct EthosConfig {
    pub service: ServiceConfig,
    pub database: DatabaseConfig,
    pub embedding: EmbeddingConfig,
    pub consolidation: ConsolidationConfig,
    pub retrieval: RetrievalConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServiceConfig {
    pub socket_path: String,   // default: "/tmp/ethos.sock"
    pub log_level: String,     // default: "info"
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,            // postgresql://ethos:ethos_dev@localhost:5432/ethos
    pub max_connections: u32,   // default: 10
}
```

### 2. `ethos-core/src/db.rs`

```rust
pub async fn create_pool(config: &DatabaseConfig) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(config.max_connections)
        .connect(&config.url)
        .await
}

pub async fn health_check(pool: &PgPool) -> Result<String, sqlx::Error> {
    // Returns PostgreSQL version string
    let row: (String,) = sqlx::query_as("SELECT version()").fetch_one(pool).await?;
    Ok(row.0)
}

pub async fn check_pgvector(pool: &PgPool) -> Result<String, sqlx::Error> {
    // Returns pgvector version
    let row: (String,) = sqlx::query_as(
        "SELECT extversion FROM pg_extension WHERE extname = 'vector'"
    ).fetch_one(pool).await?;
    Ok(row.0)
}
```

### 3. `ethos-core/src/ipc.rs`

MessagePack-framed messages over Unix socket:

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum EthosRequest {
    Ping,
    Health,
    Ingest { payload: serde_json::Value },
    Search { query: String, limit: Option<u32> },
    Get { id: uuid::Uuid },
    Consolidate { session: Option<String>, reason: Option<String> },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EthosResponse {
    pub status: String,      // "ok" | "error"
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
    pub version: String,     // "0.1.0"
}

impl EthosResponse {
    pub fn ok(data: impl serde::Serialize) -> Self { ... }
    pub fn err(msg: impl Into<String>) -> Self { ... }
    pub fn pong() -> Self {
        EthosResponse::ok(serde_json::json!({"pong": true, "version": "0.1.0"}))
    }
}
```

### 4. `ethos-core/src/models/`

Rust structs that mirror the DB schema. Each struct should derive `sqlx::FromRow`, `Serialize`, `Deserialize`, `Debug`, `Clone`. 

Example for `session.rs`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Session {
    pub id: Uuid,
    pub session_key: String,
    pub agent_id: String,
    pub channel: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_active_at: DateTime<Utc>,
    pub message_count: i32,
    pub metadata: serde_json::Value,
}
```

Create similar structs for `EpisodicTrace`, `SemanticFact`, `WorkflowMemory`, `MemoryVector`, `MemoryGraphLink`. These are STUBS at this stage — don't implement full query logic, just the types.

### 5. `ethos-server/src/main.rs`

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Parse args: --health flag for smoke test mode
    // 2. Load config from ethos.toml (or path specified via --config)
    // 3. Init tracing/logging
    // 4. Connect to DB pool
    
    // If --health flag:
    //   run health_check() + check_pgvector()
    //   print results
    //   exit 0
    
    // Otherwise:
    // 5. Start Unix socket server
    // 6. Set up Ctrl+C handler for graceful shutdown
    // 7. Run until shutdown signal
}
```

### 6. `ethos-server/src/server.rs`

Unix domain socket listener using `tokio::net::UnixListener`:

```rust
pub async fn run_unix_server(
    socket_path: &str,
    pool: PgPool,
    shutdown: tokio::sync::broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    // Remove stale socket file if exists
    // Bind UnixListener to socket_path
    // Accept connections in loop
    // Per connection: spawn tokio task, read MessagePack frame, call router, write response
    // On shutdown signal: close listener, remove socket file
}
```

### 7. `ethos-server/src/router.rs`

```rust
pub async fn handle_request(
    request: EthosRequest,
    pool: &PgPool,
) -> EthosResponse {
    match request {
        EthosRequest::Ping => EthosResponse::pong(),
        EthosRequest::Health => {
            // Run DB health check, return version info
        }
        EthosRequest::Ingest { payload } => {
            // STUB: log the payload, return ok
            EthosResponse::ok(json!({"queued": true}))
        }
        EthosRequest::Search { query, limit } => {
            // STUB: return empty results
            EthosResponse::ok(json!({"results": [], "query": query}))
        }
        _ => EthosResponse::ok(json!({"stub": true}))
    }
}
```

---

## `ethos.toml` (Create at project root)

```toml
[service]
socket_path = "/tmp/ethos.sock"
log_level = "info"

[database]
url = "postgresql://ethos:ethos_dev@localhost:5432/ethos"
max_connections = 10

[embedding]
backend = "gemini"
gemini_model = "gemini-embedding-001"
gemini_dimensions = 768
onnx_model = "all-MiniLM-L6-v2"
onnx_dimensions = 384
batch_size = 32
batch_timeout_seconds = 5
queue_capacity = 1000
rate_limit_rpm = 15

[consolidation]
interval_minutes = 15
idle_threshold_seconds = 60
cpu_threshold_percent = 80
importance_threshold = 0.8
repetition_threshold = 3
retrieval_threshold = 5

[retrieval]
decay_factor = 0.15
spreading_strength = 0.85
iterations = 3
anchor_top_k_episodes = 10
anchor_top_k_facts = 10
weight_similarity = 0.5
weight_activation = 0.3
weight_structural = 0.2
confidence_gate = 0.12

[decay]
base_tau_days = 7.0
ltp_multiplier = 1.5
frequency_weight = 0.3
emotional_weight = 0.2
prune_threshold = 0.05

[conflict_resolution]
auto_supersede_confidence_delta = 0.15
review_inbox = "~/.openclaw/shared/inbox/michael-memory-review.md"

[governance]
default_sensitivity = "internal"
default_trust_level = "medium"
default_retention = "1y"
pii_detection = false
```

---

## Rust Toolchain Notes

- **Rust version:** Stable (1.75+ for workspace features)
- **Check Rust is installed:** `rustc --version`, `cargo --version`
- **If not installed:** `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **pyo3:** Requires Python 3.8+ and `python3-dev` package. Check with `python3 --version`. If pyo3 causes build issues, stub it out and note it — don't block on it.
- **sqlx:** Uses compile-time query checking. For the scaffold, use `sqlx::query` (dynamic) rather than `sqlx::query!` macro to avoid needing DATABASE_URL at compile time.

**Environment setup before build:**
```bash
export DATABASE_URL="postgresql://ethos:ethos_dev@localhost:5432/ethos"
export SQLX_OFFLINE=false  # allow online query checking
```

---

## Implementation Steps

1. **Check Rust toolchain** — `rustc --version`. Install if missing.
2. **Create workspace `Cargo.toml`** at `/home/revenantpulse/Projects/ethos/Cargo.toml`
3. **Create all 3 crate directories** with their `Cargo.toml` and `src/` stubs
4. **Implement `ethos-core`** — config, db, error, ipc, model stubs
5. **Implement `ethos-server`** — main, server, router, subsystem stubs
6. **Implement `ethos-ingest`** — lib, pipeline stub
7. **Create `ethos.toml`** at project root
8. **Run `cargo build`** — fix any compilation errors
9. **Run `cargo clippy`** — fix any warnings
10. **Run `cargo run --bin ethos-server -- --health`** — confirm DB connection works
11. **Run `cargo run --bin ethos-server`** — confirm server starts + socket is created
12. **Test IPC ping:**
    ```bash
    # In another terminal:
    python3 -c "
    import socket, msgpack
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect('/tmp/ethos.sock')
    s.sendall(msgpack.packb({'action': 'ping'}))
    data = s.recv(1024)
    print(msgpack.unpackb(data))
    "
    ```
    Expected: `{'status': 'ok', 'data': {'pong': True, 'version': '0.1.0'}, 'version': '0.1.0'}`
13. **Update `migrations/README.md`** with build instructions

---

## Output Expected

- Complete Cargo workspace at `/home/revenantpulse/Projects/ethos/`
- `cargo build` passes (no errors)  
- `cargo clippy` passes (no warnings or documented suppressions)
- `cargo run --bin ethos-server -- --health` output:
  ```
  ✅ PostgreSQL connected: PostgreSQL 17.x on x86_64-pc-linux-gnu
  ✅ pgvector version: 0.8.0
  ✅ Ethos DB health check passed
  ```
- `cargo run --bin ethos-server` runs and listens on `/tmp/ethos.sock`
- IPC ping test returns `{"status":"ok","data":{"pong":true}}`
- Graceful shutdown on Ctrl+C

---

## What NOT to Do in This Story

- ❌ Don't implement the actual ingest/consolidation/retrieval logic — stubs only
- ❌ Don't write the OpenClaw TypeScript hook yet (Story 003)
- ❌ Don't implement spreading activation (Story 004)
- ❌ Don't implement the embedding worker (Story 005)
- ❌ Don't add pyo3 Python embedding if it blocks build — stub it with a feature flag
- ❌ Don't touch the PostgreSQL schema — it's already deployed

---

## Notes for Forge

- **The spec is in gap-resolution.md — read it before writing a single line of code.** The architecture decisions are already made. Your job is to implement them faithfully, not redesign.
- The DB schema is already deployed at `postgresql://ethos:ethos_dev@localhost:5432/ethos` — run `--health` to verify before diving in.
- Use `anyhow::Result` for main error handling, `thiserror` for defined error types in `ethos-core`.
- Keep the Unix socket framing simple: 4-byte little-endian length prefix + MessagePack payload. This is the IPC contract the TypeScript hook will need to speak.
- If `pyo3` causes any build headaches on this machine, gate it behind a `python-ffi` feature flag and stub it out. Don't burn hours on it.
- Log everything with `tracing::info!` / `tracing::warn!` / `tracing::error!` — no `println!` except in the health check output.

---

*Story 002 of the Ethos MVP epic. Next: Story 003 — ethos-ingest OpenClaw hook (TypeScript), Story 004 — spreading activation engine, Story 005 — embedding worker.*
