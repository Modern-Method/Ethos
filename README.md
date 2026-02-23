# 🧠 Ethos — Neuromorphic Memory Engine

> *Give your AI agents genuine long-term memory.*

Ethos is a standalone memory service that gives AI agents the ability to remember, forget, and grow wiser over time — just like a human mind does.

It passively records every conversation, embeds memories using Gemini, surfaces relevant context automatically before each agent response, consolidates important facts into durable knowledge, and lets unimportant memories fade away naturally via Ebbinghaus decay. Your agents start remembering things without you ever having to tell them twice.

Built with **Rust + PostgreSQL + pgvector**. Integrates with [OpenClaw](https://openclaw.ai) out of the box.

---

## Why Ethos?

Most AI agents have no persistent memory. They start each conversation from scratch, forget what you told them yesterday, and have no sense of what's important vs. noise.

Ethos solves this at the infrastructure level — not with prompt hacks or manual memory files, but with a proper memory engine inspired by how the human hippocampus actually works:

- **Consolidation** — raw conversation turns are periodically distilled into structured facts (decisions, preferences, entities), the same way sleep consolidates short-term memories into long-term knowledge
- **Spreading activation** — when searching for memories, related concepts light up associatively, not just keyword matches
- **Ebbinghaus decay** — memories that aren't reinforced naturally fade; memories that are accessed frequently become stronger (Long-Term Potentiation)
- **Confidence gating** — low-confidence memories are surfaced with caveats, not stated as facts

The result: agents that get *smarter and more contextually aware the longer you use them*, while naturally pruning noise.

---

## ✨ Features

- **Passive recording** — hooks into OpenClaw's message pipeline; every conversation captured automatically, zero agent code changes
- **Automatic context injection** — relevant memories surfaced into agent context before every response via `ETHOS_CONTEXT.md`
- **Semantic search** — 768-dim Gemini embeddings + pgvector cosine similarity finds what's *relevant*, not just what matches keywords
- **Spreading activation** — graph-based retrieval propagates relevance through connected memories, surfacing context you didn't know to ask for
- **Background consolidation engine** — episodic memories promoted to semantic facts every 15 minutes; important decisions and preferences extracted automatically
- **Conflict resolution** — when new facts contradict old ones, Ethos applies tiered resolution (refinement → update → supersession → human review flag)
- **Ebbinghaus decay + LTP** — salience(t) = S₀ × e^(−t/τ) × (1 + α×f) × (1 + β×E); memories retrieved frequently live longer, memories ignored fade to pruning
- **Async embedding pipeline** — ingest is instant; Gemini API calls happen in a background Tokio task, never blocking conversation delivery
- **Graceful degradation** — Ethos being down never affects chat; all hooks are fire-and-forget with empty-file fallback
- **Production-grade** — atomic DB transactions, 90%+ test coverage, structured tracing logs, health checks, systemd service

---

## How Memory Works

```
Your conversation
       │
       ▼
 session_events          ← raw turn log (fast, UNLOGGED)
       │
       │  every 15 min (idle)
       ▼
 episodic_traces         ← turn clusters with salience scoring
       │
       │  consolidation: importance ≥ 0.8
       │               retrieval_count ≥ 5
       │               decision/preference patterns
       ▼
 semantic_facts          ← durable structured knowledge
       │
       │  Ebbinghaus decay sweep (every 15 min)
       │  salience(t) = S₀ × e^(-t/τ_eff)
       │  τ_eff = base_tau × ltp_multiplier^retrieval_count
       ▼
 pruned = true           ← soft-deleted (never hard-deleted)
```

**Default decay parameters:**
| Retrievals | τ_eff | Memory half-life |
|-----------|-------|-----------------|
| 0 (never accessed) | 7 days | ~10 days to prune |
| 5 retrievals (LTP) | 53 days | ~75 days to prune |
| 10 retrievals | 406 days | ~1.5 years to prune |

Things you talk about frequently become nearly permanent. Things never revisited fade away naturally.

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    OpenClaw Gateway                      │
│                                                          │
│  message:received ──→ [ethos-ingest hook]  (records)   │
│  message:sent     ──→ [ethos-ingest hook]  (records)   │
│  message:received ──→ [ethos-context hook] (injects)   │
└──────────────────────────┬──────────────────────────────┘
                           │ Unix socket IPC
                           │ LE-framed MessagePack
                           ▼
┌─────────────────────────────────────────────────────────┐
│                   ethos-server (Rust)                    │
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────┐  │
│  │  Ingest  │  │ Embedder │  │ Retrieve │  │Consoli-│  │
│  │subsystem │  │subsystem │  │subsystem │  │ dation │  │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └───┬────┘  │
│       │             │             │             │        │
│  ┌────▼─────────────▼─────────────▼─────────────▼────┐  │
│  │              PostgreSQL + pgvector                  │  │
│  │                                                     │  │
│  │  session_events   episodic_traces   semantic_facts  │  │
│  │  memory_vectors   memory_graph_links workflow_mem   │  │
│  └─────────────────────────────────────────────────────┘  │
│                                                          │
│  ┌──────────────────┐  ┌─────────────────────────────┐  │
│  │   Decay Engine   │  │    Spreading Activation      │  │
│  │  (Ebbinghaus +   │  │   (graph.rs + linker.rs)    │  │
│  │      LTP)        │  │                             │  │
│  └──────────────────┘  └─────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
                           │
        ETHOS_CONTEXT.md written to agent workspace
        Agent reads it automatically → memory in context
```

### IPC Protocol

All communication uses a **Unix domain socket** at `/tmp/ethos.sock`:

```
┌───────────────┬─────────────────────────────────┐
│  4-byte LE u32│     MessagePack payload          │
│  (length)     │     (named fields / maps)        │
└───────────────┴─────────────────────────────────┘
```

> ⚠️ **Little-endian** length prefix — not big-endian.

---

## Prerequisites

| Requirement | Version | Notes |
|-------------|---------|-------|
| Rust + Cargo | 1.75+ | `rustup` recommended |
| PostgreSQL | 16+ | 17 recommended; native install preferred |
| pgvector | 0.8.0+ | `postgresql-17-pgvector` on Ubuntu 25.10+ |
| Node.js | 18+ | For the TypeScript hooks |
| Google AI Studio API key | — | Free tier works; ~100–250 embeds/day |

---

## Installation

### 1. PostgreSQL + pgvector

**Ubuntu 25.10+:**
```bash
sudo apt install postgresql-17 postgresql-17-pgvector
sudo systemctl enable --now postgresql
```

**macOS:**
```bash
brew install postgresql pgvector
brew services start postgresql
```

### 2. Create the Database

```bash
# Run as postgres superuser (required for CREATE EXTENSION vector)
sudo -u postgres psql << 'SQL'
CREATE USER ethos WITH PASSWORD 'your_password_here';
CREATE DATABASE ethos OWNER ethos;
\c ethos
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
GRANT ALL PRIVILEGES ON DATABASE ethos TO ethos;
SQL
```

### 3. Clone and Build

```bash
git clone https://github.com/modernmethod/ethos.git
cd ethos
cargo build --release
```

### 4. Configure

```bash
cp ethos.toml.example ethos.toml
```

Edit `ethos.toml` with your database URL, then create `.env` for secrets:

```bash
cat > .env << 'EOF'
GOOGLE_API_KEY=your_gemini_api_key_here
EOF
```

> **Get a free Gemini API key:** https://aistudio.google.com/app/apikey

### 5. Run Migrations

```bash
psql -U ethos -d ethos -h localhost -f migrations/001_initial_schema.sql
psql -U ethos -d ethos -h localhost -f migrations/002_story_004_ingest.sql
```

> **Note:** Run migrations as the `postgres` superuser if the vector extension step fails:
> `sudo -u postgres psql -d ethos -f migrations/001_initial_schema.sql`

### 6. Health Check

```bash
cargo run --release --bin ethos-server -- --health
# ✅ PostgreSQL connected: PostgreSQL 17.x ...
# ✅ pgvector version: 0.8.0
```

---

## Running

### systemd service (recommended)

```bash
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/ethos-server.service << 'EOF'
[Unit]
Description=Ethos Memory Engine
After=network.target postgresql.service

[Service]
Type=simple
WorkingDirectory=/path/to/ethos
ExecStart=/path/to/ethos/target/release/ethos-server
Restart=on-failure
RestartSec=5
EnvironmentFile=/path/to/ethos/.env

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now ethos-server
loginctl enable-linger "$USER"   # start at boot without login
```

### Manual

```bash
cd /path/to/ethos
cargo run --release --bin ethos-server
```

---

## OpenClaw Integration

Ethos integrates with [OpenClaw](https://openclaw.ai) via two lightweight TypeScript hooks:

📖 **[Full Integration Guide → docs/OPENCLAW_INTEGRATION.md](docs/OPENCLAW_INTEGRATION.md)**

**TL;DR — three changes to your OpenClaw config:**

```json
{
  "hooks": {
    "internal": {
      "enabled": true,
      "entries": {
        "ethos-ingest": { "enabled": true },
        "ethos-context": { "enabled": true }
      }
    }
  }
}
```

No agent prompt changes. No routing changes. Memory just appears.

---

## IPC API Reference

### Ping

```json
→ { "action": "ping" }
← { "status": "ok", "data": { "pong": true } }
```

### Health

```json
→ { "action": "health" }
← { "status": "ok", "data": { "postgresql": "17.x", "pgvector": "0.8.0", "status": "healthy" } }
```

### Ingest

```json
→ {
    "action": "ingest",
    "payload": {
      "content": "Michael prefers Bridge mode over NAT for VMware",
      "source": "user",
      "metadata": { "channel": "telegram", "ts": "2026-02-22T10:59:00Z" }
    }
  }
← { "status": "ok", "data": { "queued": true, "id": "uuid" } }
```

Embedding happens asynchronously in the background.

### Search

```json
→ {
    "action": "search",
    "payload": { "query": "VMware network setup", "limit": 5 }
  }
← {
    "status": "ok",
    "data": {
      "results": [
        {
          "id": "uuid",
          "content": "Michael prefers Bridge mode over NAT for VMware",
          "score": 0.89,
          "source": "user",
          "created_at": "2026-02-22T10:59:00Z"
        }
      ],
      "query": "VMware network setup",
      "count": 1
    }
  }
```

`score` is cosine similarity (1.0 = identical, 0.0 = unrelated). Results above the `confidence_gate` (default 0.12) only.

---

## Project Structure

```
ethos/
├── ethos-core/              # Shared types, DB, embeddings, IPC
│   └── src/
│       ├── config.rs        # EthosConfig (from ethos.toml)
│       ├── db.rs            # PostgreSQL pool
│       ├── embeddings.rs    # Gemini embedding client
│       ├── graph.rs         # Spreading activation algorithm
│       ├── ipc.rs           # Request/response protocol (MessagePack)
│       └── models/          # DB row types (episode, fact, vector, etc.)
├── ethos-server/            # Main binary — IPC server + all subsystems
│   └── src/
│       ├── main.rs          # Entry point, config, health check
│       ├── server.rs        # Unix socket listener
│       ├── router.rs        # Request dispatch
│       └── subsystems/
│           ├── ingest.rs    # Write session_events + memory_vectors
│           ├── embedder.rs  # Background Gemini embedding loop
│           ├── retrieve.rs  # pgvector cosine search + spreading activation
│           ├── consolidate.rs # DMN consolidation loop (episodic → semantic)
│           ├── decay.rs     # Ebbinghaus decay + LTP sweep
│           └── linker.rs    # Memory graph edge builder
├── hooks/
│   ├── ethos-ingest/        # OpenClaw hook: passive message recorder
│   └── ethos-context/       # OpenClaw hook: automatic memory injection
├── migrations/              # SQL migration files
├── docs/
│   ├── OPENCLAW_INTEGRATION.md  # Full OpenClaw setup guide
│   ├── stories/             # BMAD implementation stories
│   └── runbooks/            # Operational runbooks
├── ethos.toml               # Configuration
└── Cargo.toml               # Workspace manifest
```

---

## Development

```bash
# Run all tests
cargo test

# TypeScript hook tests
cd hooks/ethos-ingest && npm test
cd hooks/ethos-context && npm test

# Coverage (requires cargo-tarpaulin)
cargo install cargo-tarpaulin
cargo tarpaulin --out Stdout

# Lint
cargo clippy -- -D warnings
cargo fmt --check
```

**Target:** 90%+ test coverage across all crates.

---

## Roadmap

| Story | Feature | Status |
|-------|---------|--------|
| 001 | DB schema (6 tables, HNSW index) | ✅ Done |
| 002 | Rust IPC server scaffold | ✅ Done |
| 003 | TypeScript ingest hook | ✅ Done |
| 004 | DB ingest (atomic transactions) | ✅ Done |
| 005 | Gemini embedding (async pipeline) | ✅ Done |
| 006 | Semantic retrieval (pgvector) | ✅ Done |
| 007 | Context injection (ETHOS_CONTEXT.md) | ✅ Done |
| 008 | Spreading activation (graph traversal) | ✅ Done |
| 009 | Consolidation engine (episodic → semantic) | ✅ Done |
| 010 | Ebbinghaus decay + LTP | ✅ Done |
| 011 | REST API + QMD wire protocol (memory_search integration) | ✅ Done |
| 012 | ONNX embedding fallback (offline mode) | 📋 Planned |

---

## Contributing

Ethos uses the [BMAD Method](https://github.com/bmad-code-org/BMAD-METHOD) for development — spec first, then implementation, then review.

1. Read the story spec in `docs/stories/`
2. Write tests first (TDD)
3. Implement to make tests pass — target 90% coverage
4. Run `cargo fmt` + `cargo clippy -- -D warnings`
5. Open a PR with a runbook in `docs/runbooks/`

---

## License

Apache 2.0 — see [LICENSE](LICENSE).

---

## About

Built by [Modern Method Inc.](https://modernmethod.io) as part of the [Animus](https://animus.modernmethod.io) neuromorphic AI framework — a brain-inspired multi-agent AI system where Ethos serves as the Hippocampus.

> *"Ship the future of intelligence."*
