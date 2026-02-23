# Ethos × OpenClaw Integration Guide

> **What this is:** A step-by-step guide for OpenClaw users who want to plug Ethos into their setup and give their agents real, persistent, decaying memory.
>
> **What you get:** Agents that passively record every conversation, consolidate important facts over time, surface relevant memories automatically before each response, and naturally forget things that aren't reinforced — just like a human would.

---

## How It Works (30-second version)

Ethos sits alongside OpenClaw as a standalone Rust service. Two lightweight TypeScript hooks bridge them:

```
Your message arrives
  → ethos-ingest hook fires   → records it to Ethos (fire-and-forget)
  → ethos-context hook fires  → searches Ethos for relevant memories
                              → writes ETHOS_CONTEXT.md to agent workspace
  → Agent responds (with memory context already in view)

Every 15 minutes (when idle):
  → Ethos consolidation engine promotes important episodes → semantic facts
  → Ebbinghaus decay sweep runs → stale memories fade, retrieved ones strengthen
```

No agent code changes. No tool calls required. Memory just works.

---

## Prerequisites

| Requirement | Version | Notes |
|-------------|---------|-------|
| OpenClaw | 2026.2.19-2+ | Hooks framework required |
| PostgreSQL | 16+ | Native install recommended (not Docker) |
| pgvector | 0.8.0+ | Vector similarity extension |
| Rust + Cargo | 1.75+ | For building Ethos |
| Node.js | 18+ | For the TypeScript hooks |
| Google AI Studio API key | — | Free tier works for embeddings |

### Install PostgreSQL + pgvector (Ubuntu/Debian)

```bash
# Ubuntu 25.10+ has pgvector in standard repos
sudo apt install postgresql-17 postgresql-17-pgvector

# Start PostgreSQL
sudo systemctl enable --now postgresql
```

---

## Step 1: Clone and Build Ethos

```bash
cd ~/Projects   # or wherever you keep your code
git clone https://github.com/modernmethod/ethos.git
cd ethos

# Build the server (first build takes a few minutes)
cargo build --release

# Verify it compiled
./target/release/ethos-server --help
```

---

## Step 2: Set Up the Database

```bash
# Create DB user and database
sudo -u postgres psql << 'SQL'
CREATE USER ethos WITH PASSWORD 'your_password_here';
CREATE DATABASE ethos OWNER ethos;
GRANT ALL PRIVILEGES ON DATABASE ethos TO ethos;
SQL

# Run migrations (must be run as postgres superuser for the vector extension)
sudo -u postgres psql -d ethos -f migrations/001_initial_schema.sql
psql -U ethos -d ethos -h localhost -f migrations/002_story_004_ingest.sql
```

---

## Step 3: Configure Ethos

Copy the example config and fill in your values:

```bash
cp ethos.example.toml ethos.toml
```

Key settings to update in `ethos.toml`:

```toml
[database]
url = "postgresql://ethos:your_password_here@localhost:5432/ethos"

[embedding]
backend = "gemini"
gemini_dimensions = 768   # 768 = good balance of quality vs storage

[consolidation]
interval_minutes = 15     # how often to promote episodes → semantic facts
importance_threshold = 0.8 # episodes above this salience get promoted

[decay]
base_tau_days = 7.0       # memories halve in ~10 days without reinforcement
ltp_multiplier = 1.5      # each retrieval extends memory lifetime by 1.5x
prune_threshold = 0.05    # memories below this salience are soft-deleted
```

Create a `.env` file for your API key (never commit this):

```bash
echo "GOOGLE_API_KEY=your_gemini_api_key_here" > .env
```

> **Get a free Gemini API key:** https://aistudio.google.com/app/apikey
> The free tier supports ~100-250 embedding requests/day — plenty for personal use.

---

## Step 4: Start the Ethos Server

### Option A: Manual (development)

```bash
cd ~/Projects/ethos
cargo run --release --bin ethos-server

# Verify it's running
cargo run --release --bin ethos-server -- --health
# ✅ PostgreSQL connected: PostgreSQL 17.x ...
# ✅ pgvector version: 0.8.0
```

### Option B: systemd service (recommended for always-on)

Create `~/.config/systemd/user/ethos-server.service`:

```ini
[Unit]
Description=Ethos Memory Engine
After=network.target postgresql.service

[Service]
Type=simple
WorkingDirectory=%h/Projects/ethos
ExecStart=%h/Projects/ethos/target/release/ethos-server
Restart=on-failure
RestartSec=5
EnvironmentFile=%h/Projects/ethos/.env

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now ethos-server
systemctl --user status ethos-server
```

Verify the socket is live:

```bash
ls /tmp/ethos.sock   # should exist while server is running
```

---

## Step 5: Install the OpenClaw Hooks

Ethos ships with two hooks that live inside OpenClaw's hooks directory:

```bash
# Copy hooks to OpenClaw's hooks directory
cp -r hooks/ethos-ingest ~/.openclaw/hooks/
cp -r hooks/ethos-context ~/.openclaw/hooks/

# Install TypeScript dependencies for the ingest hook
cd ~/.openclaw/hooks/ethos-ingest
npm install
npm run build

# Install dependencies for the context hook
cd ~/.openclaw/hooks/ethos-context
npm install
npm run build
```

> **Note:** The hooks reference the Ethos client library via an absolute path. If you cloned Ethos somewhere other than `~/Projects/ethos`, update the `require()` path in both `handler.ts` files before building.

---

## Step 6: Update `openclaw.json`

Add the hooks to your OpenClaw configuration. Open `~/.openclaw/openclaw.json` and add the `hooks` section (or merge with your existing hooks config):

```json
{
  "hooks": {
    "internal": {
      "enabled": true,
      "entries": {
        "ethos-ingest": {
          "enabled": true
        },
        "ethos-context": {
          "enabled": true
        }
      }
    }
  }
}
```

Then restart OpenClaw to apply:

```bash
openclaw gateway restart
```

---

## Step 7: Verify It's Working

### Check passive recording

Send a message to your agent, then query the database:

```bash
PGPASSWORD=your_password psql -U ethos -d ethos -h localhost \
  -c "SELECT role, content, created_at FROM session_events ORDER BY created_at DESC LIMIT 5;"
```

You should see your recent messages appearing in real time.

### Check embeddings are being generated

```bash
PGPASSWORD=your_password psql -U ethos -d ethos -h localhost \
  -c "SELECT COUNT(*) FROM memory_vectors WHERE vector IS NOT NULL;"
```

### Check context injection is working

After a few messages, inspect the context file Ethos writes to your agent:

```bash
cat ~/.openclaw/<your-agent-workspace>/ETHOS_CONTEXT.md
```

Once enough memories accumulate (typically after a day or two of usage), you'll see relevant context appearing here before each response.

### Run the integration test

```bash
cd ~/Projects/ethos
node integration-test.mjs
# ✅ Ping: pong
# ✅ Health: PostgreSQL + pgvector connected
# ✅ Ingest: memory stored (id: ...)
# ✅ Search: results returned (score: 0.879)
```

---

## What Changed in OpenClaw

For completeness, here's the exact set of changes made to wire Ethos into an existing OpenClaw installation:

| Change | Where | Why |
|--------|-------|-----|
| Added `ethos-ingest` hook | `~/.openclaw/hooks/ethos-ingest/` | Records every message to Ethos (fire-and-forget, never blocks) |
| Added `ethos-context` hook | `~/.openclaw/hooks/ethos-context/` | Searches Ethos before each response, writes `ETHOS_CONTEXT.md` |
| Enabled both hooks | `openclaw.json` → `hooks.internal.entries` | Activates them in the OpenClaw hook pipeline |
| No changes to agent prompts | — | Hooks operate at the infrastructure layer; agents are unaware |
| No changes to routing or models | — | Ethos is additive — existing config untouched |

**That's it.** Ethos is designed as a drop-in addition, not a replacement. Your existing setup keeps working. Ethos adds memory on top.

---

## How Memory Recall Works

### Passive (automatic — no agent code needed)

On every inbound message, the `ethos-context` hook:
1. Takes the message content as a search query
2. Calls Ethos search (BM25 + vector similarity + spreading activation)
3. Filters results below the confidence gate (default: 0.12)
4. Writes formatted results to `{agent-workspace}/ETHOS_CONTEXT.md`

Your agent reads this file automatically as part of their workspace context — no tool calls, no prompting required.

### Active (explicit tool call)

Agents can also search Ethos directly using the `memory_search` tool:

```
memory_search("Animus brain regions")
```

> **Note (as of Ethos v1):** `memory_search` currently routes to QMD (OpenClaw's built-in memory backend), not Ethos. Ethos v1.1 will implement the QMD wire protocol, making `memory_search` transparently route to Ethos as a drop-in replacement. Until then, passive injection via `ETHOS_CONTEXT.md` is the primary recall path.

---

## How Memory Evolves Over Time

Ethos runs a background consolidation + decay cycle every 15 minutes (when the system is idle):

```
session_events (raw turns)
  ↓ consolidation (high-salience episodes promoted)
episodic_traces (turn clusters with importance scores)
  ↓ consolidation (repeated/important episodes extracted)
semantic_facts (durable knowledge: decisions, preferences, entities)
  ↓ Ebbinghaus decay (salience fades without reinforcement)
  ↑ LTP boost (salience strengthens on each retrieval)
  ↓ pruning (salience < 0.05 → soft-deleted, never hard-deleted)
```

**Decay curve (no retrievals, τ = 7 days):**
- After 1 day: 87% salience remaining
- After 1 week: 37% salience
- After 2 weeks: 14% salience
- After 1 month: pruned

**With LTP (5 retrievals, τ_eff ≈ 53 days):**
- After 1 month: 57% — safely retained
- After 3 months: 18% — still alive
- After 6 months: pruned

Things you talk about frequently become essentially permanent. Things never revisited fade away naturally.

---

## Troubleshooting

### Ethos server won't start

```bash
# Check PostgreSQL is running
sudo systemctl status postgresql

# Check the socket isn't stale from a crash
rm -f /tmp/ethos.sock
cargo run --release --bin ethos-server -- --health
```

### No memories appearing in ETHOS_CONTEXT.md

- **Ethos server not running:** Check `ls /tmp/ethos.sock`
- **Not enough data yet:** Confidence gate (0.12) filters low-quality matches. Needs a few days of usage to populate meaningfully.
- **Embeddings not processed:** The async embedding queue processes in batches. Wait 5–30 seconds after ingestion.

### Hook not firing

```bash
# Verify hooks are enabled in openclaw.json
openclaw config get hooks

# Check OpenClaw logs for hook errors
journalctl --user -u openclaw-gateway -f | grep -i ethos
```

### Database connection refused

```bash
# Verify PostgreSQL is accepting connections
psql -U ethos -d ethos -h localhost -c "SELECT 1;"

# Check pg_hba.conf allows local connections
sudo grep ethos /etc/postgresql/17/main/pg_hba.conf
```

---

## Configuration Reference

See [`ethos.toml`](../ethos.toml) for the full annotated configuration file. Key tuning knobs:

| Setting | Default | Effect |
|---------|---------|--------|
| `consolidation.interval_minutes` | 15 | How often the DMN cycle runs |
| `consolidation.importance_threshold` | 0.8 | Salience required for auto-promotion |
| `decay.base_tau_days` | 7.0 | Base memory half-life in days |
| `decay.ltp_multiplier` | 1.5 | How much each retrieval extends memory life |
| `decay.prune_threshold` | 0.05 | Salience below which memories are soft-deleted |
| `retrieval.confidence_gate` | 0.12 | Minimum score to appear in ETHOS_CONTEXT.md |
| `embedding.gemini_dimensions` | 768 | Vector dimensions (128–3072; higher = more accurate, more storage) |

---

## What's Next (Ethos v1.1)

- **Story 011:** REST API + QMD wire protocol — `memory_search` transparently routes to Ethos
- **Story 012:** ONNX offline fallback — embeddings work without internet access
- **Phoenix integration:** Memory conflict review UI instead of inbox-file workflow
- **Multi-tenant support:** Isolated memory pools per user

---

*Built by [Modern Method Inc.](https://modernmethod.io) — "Ship the future of intelligence."*
*Ethos is open source (MIT). Contributions welcome.*
