# Ethos Gap Resolution Document

> **Product Name**: Ethos  
> **Component**: Animus Hippocampus — standalone memory engine  
> **Integration Target**: OpenClaw  
> **Date**: 2026-02-21  
> **Authors**: Neko (Technical Lead), Michael (Architect)  
> **Status**: Draft — Pre-implementation

---

## 1. Context

Ethos is the Animus Hippocampus extracted as a standalone Rust + Python service, integrated into OpenClaw as its memory backend (replacing QMD). It provides multi-tier memory (session buffer → episodic → semantic → workflow → task/project), graph-based retrieval with spreading activation, and automatic consolidation.

The Hippocampus architecture is well-specced across 7 documents in the Animus Memory domain. However, Section 14 of `Hippocampus-Architecture.md` identifies several known gaps that must be resolved before implementation. This document resolves each gap and adds OpenClaw-specific integration design.

### Source Documents Referenced

| Document | Key Content |
|----------|-------------|
| `Memory/Hippocampus-Architecture.md` | Core tiers, tables, write paths, decay formula, workflow promotion |
| `Memory/Episode-Schema.md` | Episode JSON, SessionBufferEntry, SemanticFact, TaskGraphNode schemas |
| `Memory/Retrieval-and-Activation.md` | Hybrid retrieval, spreading activation algorithm, confidence gating |
| `Memory/Data-Consolidation.md` | pgvector, moka cache, desktop deployment |
| `Memory/Data-Substrate.md` | Storage layer, data governance, ingestion pipeline |
| `Memory/Vector-Indexing.md` | Embedding model selection, HNSW config |
| `Memory/Workflow-Memories.md` | Trajectory promotion pipeline |

---

## 2. Gap Resolutions

### Gap 1: Idle Detection for DMN Consolidation

**Original gap**: "Exact consolidation trigger — 'idle' detection mechanism for the DMN (how is 'idle' defined? CPU threshold? No active user session?)"

**Resolution**: In the Animus brain, the DMN (Default Mode Network / Background Cognition Engine) triggers consolidation during idle periods. Since Ethos runs standalone without the full consciousness layer, we implement a simplified idle detection model:

**Definition of "idle"**: No active LLM inference running for any agent session.

**Implementation**:

```
Idle Detection Strategy:
1. TIMER-BASED (primary): Run consolidation every 15 minutes (matching Animus spec)
2. SESSION-AWARE (gate): Skip if any agent session has received a message in the last 60 seconds
3. LOAD-AWARE (guard): Skip if system CPU > 80% (avoid competing with active inference)
```

**Mechanism**: Ethos runs a background Tokio task with a 15-minute interval timer. Before each consolidation cycle:
- Check OpenClaw session activity via file modification times on session JSONL files
- Check system load via `/proc/loadavg` (Linux) or `sysinfo` crate
- If idle: run full consolidation (episodic → semantic promotion, decay sweep, link updates)
- If busy: defer to next cycle

**Rationale**: This matches the Animus spec's 15-minute DMN rhythm while being practical for a standalone service. The session-aware gate prevents consolidation from competing with active conversations. When the full Animus brain is built, this logic migrates into the DMN's beat/rhythm system.

---

### Gap 2: Episodic → Semantic Promotion Thresholds

**Original gap**: "Promotion thresholds for episodic → semantic (when does a repeated pattern become a fact? How many repetitions? What confidence threshold?)"

**Resolution**: A fact is promoted from episodic to semantic when it meets ANY of the following criteria:

**Automatic Promotion Criteria:**

| Criterion | Threshold | Rationale |
|-----------|-----------|-----------|
| **High importance** | `salience.importance >= 0.8` | Single high-salience episodes (e.g., explicit decisions, preferences) are worth capturing immediately |
| **Repetition** | Same entity-predicate pattern appears in `>= 3` distinct episodes | Repeated information across sessions signals durable knowledge |
| **Retrieval frequency** | Episode retrieved `>= 5` times by any agent | Frequently accessed information should be promoted to faster access tier |
| **Explicit marker** | User says "remember this" or equivalent | Direct user instruction to store permanently |
| **Decision marker** | Episode contains a decision pattern (e.g., "we decided", "let's go with", "the plan is") | Decisions are inherently semantic — they define future behavior |

**Confidence Assignment:**

```
Initial confidence for promoted facts:

- From high-importance episode:     0.85
- From repetition (3+ episodes):   0.75 + (0.05 * min(extra_episodes, 5))
- From retrieval frequency:         0.70
- From explicit user marker:        0.95
- From decision pattern:            0.90

Max confidence: 1.0
Confidence increases on subsequent retrievals: +0.02 per retrieval (LTP)
Confidence decays per Ebbinghaus formula if not retrieved
```

**Extraction Method**: During consolidation, an LLM summarizer (small model, e.g., GLM-5 or Haiku) processes candidate episodes and extracts structured facts:

```
Input: Episode with salience.importance = 0.85
       Summary: "Michael decided to use BMAD Method for all Modern Method projects"
       
Output: SemanticFact {
  kind: "decision",
  statement: "Modern Method uses BMAD Method for all development projects",
  subject: "Modern Method",
  predicate: "uses_dev_methodology",
  object: "BMAD Method",
  confidence: 0.90,
  source_episodes: ["ep_..."],
  topics: ["development", "process", "bmad"]
}
```

---

### Gap 3: Conflict Resolution for Semantic Facts

**Original gap**: "Conflict resolution when new facts contradict existing semantic facts (last-write-wins? confidence-weighted merge? human review?)"

**Resolution**: Tiered conflict resolution based on conflict type and severity:

#### 3.1 Conflict Detection

During consolidation, before upserting a new semantic fact:

1. Query existing facts with overlapping `subject` + `predicate` (or high embedding similarity > 0.85)
2. If found, classify the conflict:

| Conflict Type | Detection | Example |
|--------------|-----------|---------|
| **Update** | Same subject+predicate, non-contradictory (value changed) | "Michael's timezone is PST" → "Michael's timezone is PHT" |
| **Contradiction** | Same subject+predicate, logically incompatible values | "Animus uses Redis" vs "Animus uses moka (not Redis)" |
| **Refinement** | New fact adds detail to existing fact | "Michael likes pizza" → "Michael likes pizza, especially Japanese-style" |
| **Supersession** | Explicit decision overrides previous decision | "We decided to use Vue" → "We decided to use React instead" |

#### 3.2 Resolution Rules

```
Resolution Strategy (applied in order):

1. REFINEMENT → Merge: Update existing fact, append source_episodes, 
   bump confidence by +0.05. No supersession.

2. UPDATE (same source, temporal) → Supersede: New fact wins.
   Set old.superseded_by = new.fact_id.
   New confidence = max(old.confidence, new.confidence).

3. SUPERSESSION (explicit decision) → Supersede: New fact wins.
   Set old.superseded_by = new.fact_id.
   New confidence = 0.95 (explicit decision).

4. CONTRADICTION (ambiguous) → Flag for review:
   - If new.confidence > old.confidence + 0.15: auto-supersede
   - If confidence delta < 0.15: create both, flag for human review
   - Add to review queue: ~/.openclaw/shared/inbox/michael-memory-review.md
   - Include: both facts, source episodes, confidence scores
```

#### 3.3 Human Review Interface

When a conflict is flagged for review, Ethos writes to a review inbox:

```markdown
---
### [2026-02-21 15:00] Memory Conflict — Needs Review
**Subject:** Animus cache backend

**Existing fact** (confidence: 0.80):
"Animus uses Redis for session caching"
Source: ep_2026-02-19_session_30

**New fact** (confidence: 0.75):
"Animus uses moka for session caching (Redis eliminated for desktop)"
Source: ep_2026-02-21_consolidation_review

**Action needed:** Which is correct? Reply with:
- `keep-old` — Dismiss new fact
- `keep-new` — Supersede old fact
- `keep-both` — Both are valid (different contexts)
```

In future (Phoenix integration), this becomes a UI review queue. For now, the inbox-based approach works with OpenClaw agents.

---

### Gap 4: Embedding Worker Strategy

**Original gap**: "Embedding worker batch size and scheduling (inline vs async queue, max batch size, backpressure)"

**Resolution**: 

**Embedding Model**: `gemini-embedding-001` via Google AI Studio API.

- **Dimensions**: 3072 (default). Consider reducing to 768 or 1536 for storage efficiency — configurable per request via `output_dimensionality` parameter.
- **Task types**: Use `RETRIEVAL_DOCUMENT` when embedding memories/facts, `RETRIEVAL_QUERY` when embedding search queries. This asymmetry improves retrieval quality.
- **API endpoint**: `POST https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-001:embedContent`
- **Batch endpoint**: `batchEmbedContents` supports multiple texts per request (reduces HTTP overhead).
- **Cost**: Free tier available (limited RPD). Paid tier is very cheap for embeddings.
- **No local GPU/CPU needed**: All embedding computation is server-side.

**Fallback**: `all-MiniLM-L6-v2` via ONNX Runtime in Rust (384 dimensions, ~90MB RAM). Used only if API is unreachable (network down, rate limited). Ensures Ethos never blocks on embedding failures.

**Strategy**: Async queue with batching.

```
Embedding Pipeline:

1. QUEUE: New memories and updated facts are pushed to an async channel
   - Channel capacity: 1000 items (backpressure: block writer if full)
   
2. BATCH: Worker drains up to 32 items per batch (or 100 for batchEmbedContents)
   - Wait up to 5 seconds for batch to fill before processing partial batch
   
3. EMBED: Generate embeddings via Gemini API
   - Primary: gemini-embedding-001 via Google AI Studio REST API
   - Use RETRIEVAL_DOCUMENT task type for memories/facts
   - Use RETRIEVAL_QUERY task type for search queries
   - Fallback: all-MiniLM-L6-v2 ONNX if API unreachable
   - Configurable via ethos.toml
   
4. STORE: Batch upsert into memory_vectors table
   
5. BACKPRESSURE: If queue > 800 items, log warning
   - If queue stays full for > 5 minutes, drop low-importance items (importance < 0.3)
   
6. RATE LIMITING: Respect Google AI Studio limits
   - Free tier: ~100-250 RPD
   - Tier 1: 250 RPD  
   - Implement exponential backoff on 429 responses
   - Switch to ONNX fallback if rate limited
```

**Dimension strategy**: Start with 768 dimensions (good balance of quality vs storage/speed). Can scale to 3072 for maximum quality if storage allows. The `output_dimensionality` parameter controls this per request.

**pgvector column**: `embedding vector(768)` — if we change dimensions later, requires table migration. Choose wisely at init.

---

### Gap 5: Cross-User Fact Isolation

**Original gap**: "Cross-user fact isolation in multi-tenant scenarios (RLS sufficient? Separate vector namespaces?)"

**Resolution**: Ethos is single-tenant for desktop deployment (matching Animus Desktop spec).

- **Single user**: `user_id` is fixed to the OpenClaw owner
- **Multi-agent**: All agents (neko, pixel, echo) share the same memory pool — this is intentional. Agents are a team, not isolated tenants. Agent provenance is tracked via `source` field on memories.
- **RLS**: Enabled but simplified — `tenant_id` column retained for future multi-tenant but defaults to a fixed value
- **Vector namespace**: Single `memory_vectors` table, filtered by `user_id` in queries

**Agent-scoped retrieval** (optional): Agents can optionally filter by `source` to retrieve only their own memories, but default behavior returns all team memories. This supports the "shared team memory" pattern we already built.

---

### Gap 6: Spreading Activation Parameters

**Original gap**: "Lateral inhibition parameters for spreading activation (when to enable, strength coefficients)"

**Resolution**: Defer lateral inhibition to v2. Initial implementation uses the base spreading activation algorithm from `Retrieval-and-Activation.md` without lateral inhibition.

**Rationale**: Lateral inhibition is an optimization for large memory graphs where multiple competing memory clusters need to be discriminated. At our current scale (hundreds to low thousands of memories), the base algorithm with fan-out normalization provides sufficient discrimination.

**Initial parameters** (tunable via `ethos.toml`):

```toml
[retrieval]
# Spreading activation
decay_factor = 0.15        # d: activation decay per iteration
spreading_strength = 0.85  # S: propagation strength
iterations = 3             # T: number of propagation iterations
anchor_top_k_episodes = 10 # k_E: initial episode anchors
anchor_top_k_facts = 10    # k_F: initial fact anchors

# Final scoring weights
weight_similarity = 0.5    # embedding similarity
weight_activation = 0.3    # spreading activation score
weight_structural = 0.2    # salience/confidence prior

# Confidence gating
confidence_threshold = 0.12  # tau_gate: below this, mark as low-confidence
```

**Phase 2 additions** (lateral inhibition):
- Enable when memory graph exceeds 10K nodes
- Inhibition strength: start at 0.1, tune based on retrieval quality metrics

---

### Gap 7: Rust ↔ Python Port

**Original gap**: "Port spreading activation inner loop from Python to Rust once parameters stabilize (ndarray or tch)"

**Resolution**: Start with Python (NumPy) for spreading activation, plan port to Rust (`ndarray` crate) after parameters are validated.

**Phase 1**: Rust service calls Python via:
- **Option A**: Embedded Python via `pyo3` crate (preferred — single binary, no IPC overhead)
- **Option B**: Python subprocess via MessagePack over Unix socket (if pyo3 causes build issues)

**Phase 2** (after parameters stabilize): Port propagation loop to Rust using `ndarray`:
- Graph neighborhood is already built in Rust
- Propagation is just matrix multiplication — straightforward port
- Eliminates Python dependency entirely

**Decision**: Use pyo3 for Phase 1 unless build complexity is prohibitive on our VM.

---

## 3. OpenClaw Integration Design

### 3.1 Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                 OpenClaw Gateway                     │
│                                                      │
│  ┌──────────────────────────────────────────────┐   │
│  │              Hooks Layer                      │   │
│  │                                               │   │
│  │  message:received ──┐                         │   │
│  │  message:sent ──────┼──→ ethos-ingest hook    │   │
│  │  command:new ───────┘    (TypeScript)          │   │
│  │                          │                    │   │
│  └──────────────────────────│────────────────────┘   │
│                             │ IPC (Unix socket)      │
│  ┌──────────────────────────▼────────────────────┐   │
│  │            Ethos Service (Rust)                │   │
│  │                                               │   │
│  │  ┌─────────┐  ┌────────────┐  ┌───────────┐  │   │
│  │  │ Ingest  │  │ Consolidate│  │ Retrieve  │  │   │
│  │  │ Pipeline│  │ Engine     │  │ Engine    │  │   │
│  │  └────┬────┘  └─────┬──────┘  └─────┬─────┘  │   │
│  │       │             │               │         │   │
│  │  ┌────▼─────────────▼───────────────▼─────┐   │   │
│  │  │          PostgreSQL + pgvector          │   │   │
│  │  │  memories | episodic_traces | semantic  │   │   │
│  │  │  facts | workflow_memories | vectors    │   │   │
│  │  └────────────────────────────────────────┘   │   │
│  │                                               │   │
│  │  ┌─────────────┐  ┌────────────────┐          │   │
│  │  │ Python      │  │ Embedding      │          │   │
│  │  │ (pyo3)      │  │ Worker         │          │   │
│  │  │ Spreading   │  │ (ONNX/Ollama)  │          │   │
│  │  │ Activation  │  │                │          │   │
│  │  └─────────────┘  └────────────────┘          │   │
│  └───────────────────────────────────────────────┘   │
│                                                      │
│  memory_search ──→ Ethos Retrieve API               │
│  memory_get ────→ Ethos Retrieve API                │
│  (replaces QMD)                                      │
└─────────────────────────────────────────────────────┘
```

### 3.2 Hook: ethos-ingest

An OpenClaw hook that listens to message events and forwards them to Ethos:

```typescript
// ~/.openclaw/hooks/ethos-ingest/handler.ts

const handler: HookHandler = async (event) => {
  // Capture all messages (in and out)
  if (event.type === "message") {
    const payload = {
      type: event.action,           // "received" or "sent"
      content: event.context.content,
      from: event.context.from,
      to: event.context.to,
      channel: event.context.channelId,
      session: event.sessionKey,
      timestamp: event.timestamp.toISOString(),
      metadata: event.context.metadata,
    };
    
    // Fire-and-forget to Ethos via Unix socket
    void sendToEthos("ingest", payload);
  }
  
  // On session reset, trigger consolidation for that session
  if (event.type === "command" && event.action === "new") {
    void sendToEthos("consolidate", { 
      session: event.sessionKey,
      reason: "session_reset" 
    });
  }
};
```

### 3.3 Memory Backend Replacement

Ethos exposes a query API on a Unix socket that OpenClaw's `memory_search` tool calls instead of QMD:

```
memory_search("Animus brain regions") 
  → OpenClaw gateway 
    → Ethos Unix socket: { action: "search", query: "Animus brain regions" }
      → Ethos: BM25 + vector retrieval + spreading activation
        → Returns: ranked results with snippets, paths, confidence scores
```

**Configuration** (openclaw.json):
```json
{
  "memory": {
    "backend": "qmd",
    "qmd": {
      "searchMode": "search"
    }
  }
}
```

**Strategy**: Ethos implements the QMD wire protocol (same CLI interface and response format). OpenClaw doesn't need to know it's talking to Ethos instead of QMD. The `qmd` binary becomes a thin proxy that routes to Ethos when available, falls back to native QMD when Ethos is down.

This means:
- Zero changes to OpenClaw config or codebase
- Drop-in replacement with automatic fallback
- `memory_search` and `memory_get` work identically from the agent's perspective

### 3.4 Data Flow — How Information Flows Without Agent Effort

```
User sends message on Telegram
  → OpenClaw receives message
    → Hook: ethos-ingest captures content + metadata
      → Ethos: Creates SessionBufferEntry in moka + PG UNLOGGED
      
Agent responds
  → OpenClaw sends response
    → Hook: ethos-ingest captures response
      → Ethos: Appends to SessionBufferEntry
      
Every 15 minutes (idle):
  → Ethos consolidation engine wakes up
    → Scans session buffers for high-salience content
    → Creates Episodes from turn clusters
    → Extracts SemanticFacts from high-importance episodes
    → Updates memory graph links
    → Runs embedding worker for new content
    → Prunes expired memories (Ebbinghaus decay)
    
Agent calls memory_search:
  → Ethos retrieval engine
    → BM25 keyword search + vector similarity
    → Spreading activation over memory graph
    → Confidence gating (low-confidence = caveat)
    → Returns ranked results
    
No agent writes to memory files.
No agent remembers to log events.
Information just flows.
```

### 3.5 Migration Path from QMD

1. **Phase 0** (current): QMD backend with BM25 search, agents manually write memory files
2. **Phase 1** (Ethos MVP): Ethos service running alongside QMD, hook ingesting messages, consolidation running. `memory_search` still uses QMD. Validates Ethos is working correctly.
3. **Phase 2** (switchover): Ethos implements QMD wire protocol. `memory_search` routes to Ethos as drop-in replacement. QMD remains installed as automatic fallback.
4. **Phase 3** (stable): Ethos is primary memory backend. QMD is cold standby. Memory markdown files auto-generated by Ethos as human-readable exports + fallback data source.

**Key invariant**: At no point do agents lose access to memory. QMD + markdown files always available as fallback.

---

## 4. Technology Stack

| Component | Technology | Rationale |
|-----------|-----------|-----------|
| **Core service** | Rust (tokio async runtime) | Performance, memory safety, matches Animus spec |
| **Spreading activation** | Python via pyo3 (NumPy) | Rapid iteration on graph parameters |
| **Database** | PostgreSQL 16 + pgvector (native install) | No container overhead, direct access, matches Animus Desktop spec |
| **Session cache** | moka crate (in-process) | Zero overhead, crash recovery via PG UNLOGGED |
| **Embeddings (primary)** | gemini-embedding-001 via Google AI Studio API | 768-3072 dim, task-type awareness, no local compute needed |
| **Embeddings (fallback)** | all-MiniLM-L6-v2 via ONNX Runtime | 384-dim, ~90MB RAM, offline fallback |
| **IPC** | Unix domain socket (MessagePack) | Low latency, no network overhead |
| **OpenClaw hook** | TypeScript (OpenClaw hook framework) | Native integration, fire-and-forget |
| **Config** | TOML (ethos.toml) | Matches Animus convention |

---

## 5. Configuration

```toml
# ethos.toml

[service]
socket_path = "/tmp/ethos.sock"
log_level = "info"

[database]
# Native PostgreSQL install (not containerized)
url = "postgresql://ethos:ethos@localhost:5432/ethos"
max_connections = 10

[cache]
max_sessions = 1000
session_ttl_hours = 24
session_idle_timeout_hours = 1

[embedding]
# "gemini" or "onnx"
backend = "gemini"
# Gemini API settings (primary)
gemini_api_key_env = "GEMINI_API_KEY"   # reads from env var
gemini_model = "gemini-embedding-001"
gemini_dimensions = 768                  # 128-3072, 768 = good balance
gemini_base_url = "https://generativelanguage.googleapis.com/v1beta"
# ONNX settings (fallback when API unreachable)
onnx_model = "all-MiniLM-L6-v2"
onnx_dimensions = 384
# Worker settings
batch_size = 32                          # per batch for Gemini API
batch_timeout_seconds = 5
queue_capacity = 1000
rate_limit_rpm = 15                      # Gemini free tier: ~15 RPM
rate_limit_backoff_ms = 1000             # initial backoff on 429

[consolidation]
interval_minutes = 15
idle_threshold_seconds = 60    # no messages for this long = idle
cpu_threshold_percent = 80     # skip if CPU above this
importance_threshold = 0.8     # auto-promote episodes above this
repetition_threshold = 3       # promote after N occurrences
retrieval_threshold = 5        # promote after N retrievals

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
# Ebbinghaus: salience(t) = S_0 * e^(-t/tau) * (1 + alpha*f) * (1 + beta*E)
base_tau_days = 7.0           # base decay time constant
ltp_multiplier = 1.5          # tau grows by this factor per recall
frequency_weight = 0.3        # alpha: access frequency boost
emotional_weight = 0.2        # beta: emotional intensity boost
prune_threshold = 0.05        # memories below this salience are pruned

[conflict_resolution]
auto_supersede_confidence_delta = 0.15  # auto-supersede if new > old + this
review_inbox = "~/.openclaw/shared/inbox/michael-memory-review.md"

[governance]
default_sensitivity = "internal"
default_trust_level = "medium"
default_retention = "1y"
pii_detection = false          # enable for PII auto-tagging (future)
```

---

## 6. Decisions (Resolved 2026-02-21)

| # | Question | Decision | Rationale |
|---|----------|----------|-----------|
| 1 | PostgreSQL deployment | **Native install** | No container overhead on the VM. Direct access, simpler ops. |
| 2 | Embedding model | **Gemini API** (`gemini-embedding-001`) | No local compute needed, high quality (3072-dim capable), task-type awareness (RETRIEVAL_DOCUMENT vs RETRIEVAL_QUERY). ONNX `all-MiniLM-L6-v2` as offline fallback. |
| 3 | OpenClaw backend | **QMD drop-in replacement** with **QMD fallback** | Implement QMD wire protocol so OpenClaw doesn't need modification. If Ethos breaks, agents automatically fall back to QMD and keep their memories. |
| 4 | v1 scope | **Full spreading activation** | Go all in. No half-measures. Consolidation + retrieval + spreading activation in v1. |
| 5 | Memory files | **Keep generating** | Safety net. If Ethos breaks, agents still have markdown files to fall back to. Files become human-readable exports from Ethos. |

### QMD Fallback Strategy

Ethos is a prototype — it wasn't originally designed for OpenClaw. To protect against data loss:

```
Startup:
1. Ethos starts → registers as memory backend
2. QMD remains installed but inactive
3. Ethos periodically exports to markdown files (memory/YYYY-MM-DD.md, MEMORY.md)

If Ethos fails:
1. OpenClaw detects Ethos socket unreachable
2. Automatically falls back to QMD backend
3. QMD still has indexed markdown files (generated by Ethos)
4. Agents continue working with slightly stale but functional memory
5. Alert sent to Michael via Telegram

Recovery:
1. Ethos restarts
2. Re-ingests any missed messages from session transcripts
3. Resumes as primary backend
```

This ensures the team never loses memory, even if Ethos has bugs in early versions.

---

## 7. Cross-References

- Animus Memory docs: `~/Underworld/Project Ideas/Animus/Memory/`
- OpenClaw hooks docs: `~/.npm-global/lib/node_modules/openclaw/docs/automation/hooks.md`
- Shared team memory system: `~/.openclaw/shared/README.md`
- BMAD Method: `https://docs.bmad-method.org`

---

*This document resolves all known gaps from Hippocampus-Architecture.md §14 and defines the OpenClaw integration architecture. Ready for BMAD workflow: Analysis → Planning → Solutioning → Implementation.*
