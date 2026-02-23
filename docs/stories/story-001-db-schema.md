# Story 001 — Ethos Database Schema

**Epic:** Ethos MVP — Foundation  
**Story:** Database schema setup (PostgreSQL + pgvector migrations)  
**Status:** Ready for implementation  
**Assigned:** Forge  
**Date:** 2026-02-22  

---

## Context

Ethos is the Animus Hippocampus memory engine — a Rust + Python service that provides multi-tier memory for OpenClaw agents. This story creates the PostgreSQL schema that all other Ethos components depend on.

**Full architecture spec:** `/home/revenantpulse/Projects/ethos/docs/gap-resolution.md`

---

## Acceptance Criteria

1. **Prerequisites checked** — PostgreSQL 16+ and pgvector extension are installed and accessible
2. **Database and user created** — `ethos` DB and `ethos` user exist with correct privileges
3. **Migration file written** — `migrations/001_initial_schema.sql` creates all 6 tables cleanly
4. **Idempotent** — running migrations twice doesn't error (`IF NOT EXISTS` everywhere)
5. **pgvector column** — `memory_vectors` table has `embedding vector(768)` with HNSW index
6. **Verified** — run `\dt` to confirm all tables exist, run `\d memory_vectors` to confirm vector column

---

## Technical Specification

### Database Setup

```bash
# Check prerequisites
psql --version          # must be 16+
psql -c "SELECT * FROM pg_available_extensions WHERE name = 'vector';"  # pgvector must show up

# Create user and database (run as postgres superuser)
createuser --no-superuser --no-createdb --no-createrole ethos
createdb --owner=ethos ethos
psql -d ethos -c "GRANT ALL PRIVILEGES ON DATABASE ethos TO ethos;"
psql -d ethos -c "CREATE EXTENSION IF NOT EXISTS vector;"
psql -d ethos -c "CREATE EXTENSION IF NOT EXISTS pg_trgm;"   -- for BM25-style fuzzy search
```

### Tables to Create

#### 1. `sessions` — Active conversation sessions
```sql
CREATE TABLE IF NOT EXISTS sessions (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_key     TEXT NOT NULL UNIQUE,   -- e.g. "agent:neko:main"
    agent_id        TEXT NOT NULL,          -- e.g. "neko"
    channel         TEXT,                   -- e.g. "telegram"
    started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_active_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    message_count   INTEGER NOT NULL DEFAULT 0,
    metadata        JSONB NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_sessions_key ON sessions(session_key);
CREATE INDEX IF NOT EXISTS idx_sessions_agent ON sessions(agent_id);
CREATE INDEX IF NOT EXISTS idx_sessions_active ON sessions(last_active_at DESC);
```

#### 2. `episodic_traces` — Turn-by-turn conversation episodes
```sql
CREATE TABLE IF NOT EXISTS episodic_traces (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id      UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_id        TEXT NOT NULL,
    turn_index      INTEGER NOT NULL,
    role            TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system')),
    content         TEXT NOT NULL,
    summary         TEXT,                   -- LLM-generated summary (null until consolidated)
    
    -- Salience scoring
    importance      FLOAT NOT NULL DEFAULT 0.5,     -- 0.0-1.0
    emotional_tone  FLOAT NOT NULL DEFAULT 0.0,     -- -1.0 (neg) to 1.0 (pos)
    novelty         FLOAT NOT NULL DEFAULT 0.5,     -- 0.0-1.0
    
    -- Topics and entities (extracted during consolidation)
    topics          TEXT[] NOT NULL DEFAULT '{}',
    entities        TEXT[] NOT NULL DEFAULT '{}',
    
    -- Lifecycle
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    consolidated_at TIMESTAMPTZ,            -- null = not yet consolidated
    retrieval_count INTEGER NOT NULL DEFAULT 0,
    last_retrieved_at TIMESTAMPTZ,
    
    -- Decay
    salience        FLOAT NOT NULL DEFAULT 1.0,     -- Ebbinghaus decay applied here
    pruned          BOOLEAN NOT NULL DEFAULT FALSE,
    
    -- Provenance
    user_id         TEXT NOT NULL DEFAULT 'michael',
    tenant_id       TEXT NOT NULL DEFAULT 'modern-method'
);

CREATE INDEX IF NOT EXISTS idx_episodes_session ON episodic_traces(session_id);
CREATE INDEX IF NOT EXISTS idx_episodes_agent ON episodic_traces(agent_id);
CREATE INDEX IF NOT EXISTS idx_episodes_importance ON episodic_traces(importance DESC);
CREATE INDEX IF NOT EXISTS idx_episodes_created ON episodic_traces(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_episodes_unconsolidated ON episodic_traces(consolidated_at) WHERE consolidated_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_episodes_topics ON episodic_traces USING GIN(topics);
CREATE INDEX IF NOT EXISTS idx_episodes_content_trgm ON episodic_traces USING GIN(content gin_trgm_ops);
```

#### 3. `semantic_facts` — Promoted long-term facts
```sql
CREATE TABLE IF NOT EXISTS semantic_facts (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    
    -- Structured fact fields
    kind            TEXT NOT NULL,          -- "fact", "decision", "preference", "entity", "relationship"
    statement       TEXT NOT NULL,          -- human-readable statement
    subject         TEXT NOT NULL,          -- e.g. "Michael"
    predicate       TEXT NOT NULL,          -- e.g. "prefers_language"
    object          TEXT NOT NULL,          -- e.g. "Rust"
    
    -- Topics and metadata
    topics          TEXT[] NOT NULL DEFAULT '{}',
    
    -- Confidence and lifecycle
    confidence      FLOAT NOT NULL DEFAULT 0.75 CHECK (confidence BETWEEN 0.0 AND 1.0),
    retrieval_count INTEGER NOT NULL DEFAULT 0,
    last_retrieved_at TIMESTAMPTZ,
    
    -- Conflict resolution
    superseded_by   UUID REFERENCES semantic_facts(id),
    flagged_for_review BOOLEAN NOT NULL DEFAULT FALSE,
    
    -- Source tracing
    source_episodes UUID[] NOT NULL DEFAULT '{}',
    source_agent    TEXT,
    
    -- Timestamps
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Decay
    salience        FLOAT NOT NULL DEFAULT 1.0,
    pruned          BOOLEAN NOT NULL DEFAULT FALSE,
    
    -- Provenance
    user_id         TEXT NOT NULL DEFAULT 'michael',
    tenant_id       TEXT NOT NULL DEFAULT 'modern-method'
);

CREATE INDEX IF NOT EXISTS idx_facts_subject ON semantic_facts(subject);
CREATE INDEX IF NOT EXISTS idx_facts_predicate ON semantic_facts(predicate);
CREATE INDEX IF NOT EXISTS idx_facts_subject_pred ON semantic_facts(subject, predicate);
CREATE INDEX IF NOT EXISTS idx_facts_kind ON semantic_facts(kind);
CREATE INDEX IF NOT EXISTS idx_facts_confidence ON semantic_facts(confidence DESC);
CREATE INDEX IF NOT EXISTS idx_facts_active ON semantic_facts(pruned, superseded_by) WHERE pruned = FALSE AND superseded_by IS NULL;
CREATE INDEX IF NOT EXISTS idx_facts_topics ON semantic_facts USING GIN(topics);
CREATE INDEX IF NOT EXISTS idx_facts_statement_trgm ON semantic_facts USING GIN(statement gin_trgm_ops);
```

#### 4. `workflow_memories` — Task/project trajectory memories
```sql
CREATE TABLE IF NOT EXISTS workflow_memories (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workflow_id     TEXT NOT NULL,          -- e.g. "ethos-implementation"
    workflow_kind   TEXT NOT NULL,          -- "task", "project", "sprint"
    
    -- Trajectory data
    title           TEXT NOT NULL,
    description     TEXT,
    outcome         TEXT,                   -- filled when task completes
    status          TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'completed', 'abandoned', 'paused')),
    
    -- Linked memories
    linked_episodes UUID[] NOT NULL DEFAULT '{}',
    linked_facts    UUID[] NOT NULL DEFAULT '{}',
    
    -- Temporal
    started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ,
    last_active_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Retention (workflows retained longer than regular episodes)
    retention_until TIMESTAMPTZ,            -- null = indefinite
    
    -- Provenance
    agent_id        TEXT NOT NULL,
    user_id         TEXT NOT NULL DEFAULT 'michael',
    tenant_id       TEXT NOT NULL DEFAULT 'modern-method',
    metadata        JSONB NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_workflows_id ON workflow_memories(workflow_id);
CREATE INDEX IF NOT EXISTS idx_workflows_status ON workflow_memories(status);
CREATE INDEX IF NOT EXISTS idx_workflows_agent ON workflow_memories(agent_id);
CREATE INDEX IF NOT EXISTS idx_workflows_active ON workflow_memories(last_active_at DESC);
```

#### 5. `memory_vectors` — Embedding index (pgvector)
```sql
-- IMPORTANT: vector(768) — Gemini embeddings with output_dimensionality=768
-- Changing dimensions later requires a table migration. This is final.
CREATE TABLE IF NOT EXISTS memory_vectors (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    
    -- What this embedding represents
    source_type     TEXT NOT NULL CHECK (source_type IN ('episode', 'fact', 'workflow', 'query')),
    source_id       UUID NOT NULL,          -- FK to the relevant table
    
    -- The embedding
    embedding       vector(768) NOT NULL,
    
    -- Embedding metadata
    model           TEXT NOT NULL DEFAULT 'gemini-embedding-001',
    task_type       TEXT NOT NULL DEFAULT 'RETRIEVAL_DOCUMENT',  -- RETRIEVAL_DOCUMENT or RETRIEVAL_QUERY
    dimensions      INTEGER NOT NULL DEFAULT 768,
    
    -- Versioning (if we re-embed with a better model)
    embedding_version INTEGER NOT NULL DEFAULT 1,
    
    -- Content snapshot (for debugging / fallback)
    content_snippet TEXT,
    
    -- Timestamps
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Provenance
    user_id         TEXT NOT NULL DEFAULT 'michael',
    tenant_id       TEXT NOT NULL DEFAULT 'modern-method'
);

-- HNSW index: fast approximate nearest neighbor search
-- m=16 (connections per node), ef_construction=64 (build quality)
-- cosine distance for Gemini embeddings (unit vectors)
CREATE INDEX IF NOT EXISTS idx_vectors_hnsw 
    ON memory_vectors 
    USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);

CREATE INDEX IF NOT EXISTS idx_vectors_source ON memory_vectors(source_type, source_id);
CREATE INDEX IF NOT EXISTS idx_vectors_version ON memory_vectors(embedding_version);
```

#### 6. `memory_graph_links` — Spreading activation graph edges
```sql
CREATE TABLE IF NOT EXISTS memory_graph_links (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    
    -- Edge endpoints (can link episodes ↔ episodes, episodes ↔ facts, facts ↔ facts)
    from_type       TEXT NOT NULL CHECK (from_type IN ('episode', 'fact', 'workflow')),
    from_id         UUID NOT NULL,
    to_type         TEXT NOT NULL CHECK (to_type IN ('episode', 'fact', 'workflow')),
    to_id           UUID NOT NULL,
    
    -- Edge properties for spreading activation
    relation        TEXT NOT NULL,          -- "temporal_next", "semantic_similar", "derived_from", "contradicts", "supports"
    weight          FLOAT NOT NULL DEFAULT 0.5 CHECK (weight BETWEEN 0.0 AND 1.0),
    
    -- Lifecycle
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    UNIQUE(from_type, from_id, to_type, to_id, relation)
);

CREATE INDEX IF NOT EXISTS idx_links_from ON memory_graph_links(from_type, from_id);
CREATE INDEX IF NOT EXISTS idx_links_to ON memory_graph_links(to_type, to_id);
CREATE INDEX IF NOT EXISTS idx_links_relation ON memory_graph_links(relation);
CREATE INDEX IF NOT EXISTS idx_links_weight ON memory_graph_links(weight DESC);
```

---

## Implementation Steps (for Forge)

1. **Check prerequisites**:
   ```bash
   psql --version
   sudo -u postgres psql -c "SELECT * FROM pg_available_extensions WHERE name = 'vector';"
   ```
   If pgvector missing: `sudo apt install postgresql-16-pgvector` (Ubuntu) or check `https://github.com/pgvector/pgvector`

2. **Create DB user and database**:
   ```bash
   sudo -u postgres createuser --no-superuser --no-createdb --no-createrole ethos
   sudo -u postgres psql -c "ALTER USER ethos WITH PASSWORD 'ethos_dev';"
   sudo -u postgres createdb --owner=ethos ethos
   ```

3. **Write the migration file** at `migrations/001_initial_schema.sql` — combine all the SQL above in order, with a header comment

4. **Run the migration**:
   ```bash
   psql -U ethos -d ethos -h localhost -f migrations/001_initial_schema.sql
   ```

5. **Verify**:
   ```bash
   psql -U ethos -d ethos -h localhost -c "\dt"
   psql -U ethos -d ethos -h localhost -c "\d memory_vectors"
   psql -U ethos -d ethos -h localhost -c "SELECT COUNT(*) FROM pg_indexes WHERE tablename = 'memory_vectors';"
   ```

6. **Write a `README.md`** in `migrations/` documenting how to run and reset

---

## Output Expected

- `migrations/001_initial_schema.sql` — clean, runnable SQL file
- `migrations/README.md` — how to set up, run, reset
- Terminal output showing all 6 tables created and indexes in place
- Notes on any issues encountered (pgvector version mismatch, permission issues, etc.)

---

## Notes

- **Do NOT start Rust code** — that's Story 002. This story is DB only.
- **Do NOT modify OpenClaw config** — just DB setup.
- **Password** `ethos_dev` is dev only. Production creds are a separate concern.
- If PostgreSQL isn't installed at all, document what's needed and stop — don't install system packages without Michael's approval.
- pgvector HNSW requires pgvector >= 0.5.0 — verify version before creating index.

---

*Story 001 of the Ethos MVP epic. Next: Story 002 — Rust project scaffold (Cargo.toml, workspace, module structure).*
