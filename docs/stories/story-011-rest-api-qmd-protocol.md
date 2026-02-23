# Story 011 — REST API + QMD Wire Protocol (memory_search integration)

**Status:** Ready for Implementation  
**Assigned:** Forge  
**Reviewer:** Sage  
**Priority:** P0 — Closes Ethos v1 functional scope

---

## Overview

Two deliverables in one story:

1. **HTTP REST API** — Axum-based HTTP server alongside the existing Unix socket IPC, exposing Ethos search and ingest over HTTP (port 8766, configurable). This enables external tools, dashboards, and future SDKs to query Ethos without needing Unix socket access.

2. **QMD wire protocol compatibility** — A thin `ethos-cli` binary that speaks QMD's CLI output format, enabling OpenClaw's `memory_search` tool to route through Ethos instead of QMD. Drop-in replacement: set `memory.qmd.command` in `openclaw.json` to point at `ethos-cli`.

After this story, `memory_search` transparently queries Ethos's semantic search + spreading activation instead of QMD's BM25.

---

## QMD Wire Protocol Reference

QMD search with `--json` flag outputs an array:

```json
[
  {
    "docid": "#7b5c24",
    "score": 0.87,
    "file": "qmd://collection-name/path/to/file.md",
    "title": "Document Title",
    "snippet": "@@ -9,4 @@ (8 before, 157 after)\n\nContent text here..."
  }
]
```

OpenClaw calls `qmd search <query> -n <limit> --json` (or `qmd query ...`) as a subprocess and parses stdout as JSON.

`ethos-cli` must accept the same arguments and produce the same JSON format.

---

## Files to Create / Modify

| File | Action | Description |
|------|--------|-------------|
| `ethos-server/src/http.rs` | **Create** | Axum HTTP server (port 8766) |
| `ethos-server/src/main.rs` | **Modify** | Spawn HTTP server alongside IPC server |
| `ethos-core/src/config.rs` | **Modify** | Add `HttpConfig` to `EthosConfig` |
| `ethos-cli/src/main.rs` | **Create** | Thin CLI binary for QMD wire protocol compat |
| `ethos-cli/Cargo.toml` | **Create** | New crate in workspace |
| `Cargo.toml` | **Modify** | Add `ethos-cli` to workspace members |
| `ethos.toml` + `ethos.toml.example` | **Modify** | Add `[http]` section |
| `docs/runbooks/runbook-011-http-api.md` | **Create** | Runbook |
| `tests/http_integration.rs` | **Create** | HTTP integration tests |

---

## Part 1: HTTP REST API (`ethos-server/src/http.rs`)

### Server Setup

```rust
use axum::{Router, routing::{get, post}, extract::{State, Json}, http::StatusCode};
use std::sync::Arc;
use tokio::net::TcpListener;

pub struct HttpState {
    pub pool: sqlx::PgPool,
    pub config: EthosConfig,
}

pub async fn start_http_server(
    pool: sqlx::PgPool,
    config: EthosConfig,
    mut shutdown: broadcast::Receiver<()>,
) -> Result<()> {
    let addr = format!("{}:{}", config.http.host, config.http.port);
    let state = Arc::new(HttpState { pool, config });

    let app = Router::new()
        .route("/health",    get(health_handler))
        .route("/version",   get(version_handler))
        .route("/search",    post(search_handler))
        .route("/ingest",    post(ingest_handler))
        .route("/consolidate", post(consolidate_handler))
        .with_state(state);

    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Ethos HTTP API listening on http://{}", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.recv().await;
        })
        .await?;
    Ok(())
}
```

### Endpoints

#### `GET /health`

```json
// Response 200
{
  "status": "healthy",
  "version": "1.0.0",
  "postgresql": "PostgreSQL 17.x",
  "pgvector": "0.8.0",
  "socket": "/tmp/ethos.sock"
}
```

#### `GET /version`

```json
{ "version": "1.0.0", "protocol": "ethos/1" }
```

#### `POST /search`

Request:
```json
{
  "query": "Animus brain regions",
  "limit": 5,
  "use_spreading": true,
  "min_score": 0.12
}
```

Response `200`:
```json
{
  "results": [
    {
      "id": "uuid",
      "content": "The Thalamus acts as gateway...",
      "score": 0.87,
      "source": "user",
      "created_at": "2026-02-22T10:59:00Z",
      "metadata": {}
    }
  ],
  "query": "Animus brain regions",
  "count": 5,
  "took_ms": 12
}
```

Response `400` (bad request):
```json
{ "error": "query field is required", "status": "error" }
```

#### `POST /ingest`

Request: same as existing IPC `ingest` action payload.

Response:
```json
{ "queued": true, "id": "uuid" }
```

#### `POST /consolidate`

Request: `{}` or `{ "session": "optional", "reason": "manual trigger" }`

Response:
```json
{
  "episodes_scanned": 42,
  "episodes_promoted": 7,
  "facts_created": 5,
  "facts_updated": 2
}
```

### Config Addition (`ethos-core/src/config.rs`)

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct HttpConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "127.0.0.1".to_string(),
            port: 8766,
        }
    }
}
```

Add to `EthosConfig`:
```rust
#[serde(default)]
pub http: HttpConfig,
```

Add to `ethos.toml.example`:
```toml
[http]
enabled = true
host = "127.0.0.1"
port = 8766
```

### Wire into `main.rs`

```rust
if config.http.enabled {
    let http_pool = pool.clone();
    let http_config = config.clone();
    let http_shutdown = tx.subscribe();
    tokio::spawn(async move {
        if let Err(e) = http::start_http_server(http_pool, http_config, http_shutdown).await {
            tracing::error!("HTTP server error: {}", e);
        }
    });
}
```

---

## Part 2: `ethos-cli` Binary (QMD Wire Protocol)

### New Crate: `ethos-cli/Cargo.toml`

```toml
[package]
name = "ethos-cli"
version = "1.0.0"
edition = "2021"

[[bin]]
name = "ethos-cli"
path = "src/main.rs"

[dependencies]
clap = { version = "4", features = ["derive"] }
reqwest = { version = "0.12", features = ["json", "blocking"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

### CLI Interface (`ethos-cli/src/main.rs`)

Must accept the same subcommands OpenClaw uses when calling QMD:

```bash
ethos-cli search <query> [-n <limit>] [--json]
ethos-cli query  <query> [-n <limit>] [--json]   # same as search for Ethos
ethos-cli status                                   # show server status
```

**QMD-compatible JSON output** (maps Ethos results to QMD format):

```rust
#[derive(Serialize)]
struct QmdResult {
    docid: String,       // "#" + first 6 chars of UUID
    score: f64,          // Ethos similarity score (0.0-1.0)
    file: String,        // "ethos://memory/{id}" 
    title: String,       // first line of content (truncated to 60 chars)
    snippet: String,     // "@@ -1,4 @@\n\n" + content (truncated to 300 chars)
}
```

**Full CLI implementation:**

```rust
fn search(query: &str, limit: usize, json_output: bool, server: &str) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let resp: SearchResponse = client
        .post(format!("{}/search", server))
        .json(&serde_json::json!({ "query": query, "limit": limit }))
        .send()?
        .json()?;

    if json_output {
        // Output QMD-compatible JSON array
        let qmd_results: Vec<QmdResult> = resp.results.iter().map(|r| QmdResult {
            docid: format!("#{}", &r.id.replace('-', "")[..6]),
            score: r.score,
            file: format!("ethos://memory/{}", r.id),
            title: r.content.lines().next().unwrap_or("").chars().take(60).collect(),
            snippet: format!("@@ -1,4 @@\n\n{}", r.content.chars().take(300).collect::<String>()),
        }).collect();
        println!("{}", serde_json::to_string_pretty(&qmd_results)?);
    } else {
        // Human-readable format (matches QMD text output)
        for r in &resp.results {
            println!("ethos://memory/{} #{:.6}", r.id, r.id.replace('-', ""));
            println!("Score:  {:.0}%\n", r.score * 100.0);
            println!("{}\n", r.content.chars().take(200).collect::<String>());
        }
    }
    Ok(())
}
```

**Default server URL:** `http://127.0.0.1:8766` (matches `HttpConfig` default port)  
Configurable via `--server` flag or `ETHOS_HTTP_URL` env var.

### Build & Install

```bash
cargo build --release --bin ethos-cli
# Install to PATH
cp target/release/ethos-cli ~/.local/bin/ethos-cli
```

---

## OpenClaw Integration (Post-Install)

After building and installing `ethos-cli`, update `openclaw.json`:

```json
{
  "memory": {
    "backend": "qmd",
    "qmd": {
      "command": "/home/YOUR_USER/.local/bin/ethos-cli",
      "searchMode": "search",
      ...
    }
  }
}
```

`memory_search` will now transparently call Ethos semantic search + spreading activation instead of QMD BM25. Restart the gateway to apply.

**Fallback behavior:** If `ethos-cli` returns a non-zero exit code or empty results, OpenClaw's QMD manager gracefully returns empty results (no crash). The ethos-context hook (Story 007) continues to work independently via Unix socket.

---

## Tests

### Unit Tests (in `http.rs`)

1. **`test_health_endpoint`** — GET /health returns 200 with expected fields
2. **`test_version_endpoint`** — GET /version returns version string
3. **`test_search_empty_query`** — POST /search with empty query returns 400
4. **`test_search_valid`** — POST /search returns results array (may be empty if DB has no data)
5. **`test_ingest_and_search`** — Ingest a document via HTTP, then search for it, verify it appears

### Unit Tests (in `ethos-cli`)

6. **`test_qmd_format_output`** — Given a mock search response, verify JSON output matches QMD schema exactly (docid starts with `#`, file starts with `ethos://memory/`, snippet starts with `@@ -1,4 @@`)
7. **`test_cli_search_flag`** — `search` and `query` subcommands both work

### Integration Tests (`tests/http_integration.rs`)

8. **`test_http_server_starts`** — Spawn ethos-server, verify HTTP health check responds
9. **`test_search_roundtrip_http`** — Ingest via Unix socket, search via HTTP, verify result
10. **`test_ingest_via_http`** — Ingest via HTTP endpoint, verify stored in DB

---

## Acceptance Criteria

- [ ] HTTP server starts on port 8766 (configurable) alongside IPC server
- [ ] GET /health returns 200 with DB status
- [ ] POST /search returns semantically relevant results with scores
- [ ] POST /ingest queues content for embedding (reuses existing ingest subsystem)
- [ ] `ethos-cli search <query> --json` outputs QMD-compatible JSON array
- [ ] `ethos-cli query <query> --json` works identically to `search`
- [ ] Setting `qmd.command = ethos-cli` in OpenClaw routes `memory_search` to Ethos
- [ ] `cargo build --release` succeeds for all crates
- [ ] `cargo test` passes ≥ 90% coverage
- [ ] `cargo clippy` clean
- [ ] Runbook at `docs/runbooks/runbook-011-http-api.md`
- [ ] `ethos.toml.example` updated with `[http]` section

---

## Definition of Done

- All acceptance criteria checked
- Sage code review: APPROVED
- Live test: `ethos-cli search "test query" --json` returns valid QMD-compatible JSON
- Live test: `memory_search` tool returns Ethos results after openclaw.json update

---

## Notes for Forge

- Use `axum` crate (already in ecosystem, battle-tested with tokio)
- Use `reqwest` with `blocking` feature for ethos-cli (simpler than async for a CLI tool)
- The ethos-cli does NOT need to implement all QMD subcommands — only `search`, `query`, and `status` are called by OpenClaw
- HTTP server must start AFTER the DB pool is ready (same pattern as IPC server in main.rs)
- The HTTP server and IPC server run concurrently in separate tokio tasks
- Both share the same `broadcast::Receiver<()>` shutdown signal
- `ethos-cli` should exit with code 1 on error and print nothing to stdout (so OpenClaw sees empty results rather than garbage JSON)
- Add `ethos-cli` to the `[workspace]` members in root `Cargo.toml`
