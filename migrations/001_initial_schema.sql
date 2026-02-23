-- Ethos: Initial Database Schema
-- Migration: 001
-- Date: 2026-02-22
-- Author: Neko (Technical Lead)
--
-- Run with:
--   psql -U ethos -d ethos -h localhost -f migrations/001_initial_schema.sql
--
-- See migrations/README.md for full setup instructions.

-- Extensions
CREATE EXTENSION IF NOT EXISTS vector;      -- pgvector: vector similarity search
CREATE EXTENSION IF NOT EXISTS pg_trgm;     -- trigram indexes: fast text search

-- ============================================================
-- TABLE 1: sessions
-- Tracks active OpenClaw agent sessions
-- ============================================================

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

CREATE INDEX IF NOT EXISTS idx_sessions_key    ON sessions(session_key);
CREATE INDEX IF NOT EXISTS idx_sessions_agent  ON sessions(agent_id);
CREATE INDEX IF NOT EXISTS idx_sessions_active ON sessions(last_active_at DESC);

-- ============================================================
-- TABLE 2: episodic_traces
-- Turn-by-turn conversation episodes with salience scoring
-- ============================================================

CREATE TABLE IF NOT EXISTS episodic_traces (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id          UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_id            TEXT NOT NULL,
    turn_index          INTEGER NOT NULL,
    role                TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system')),
    content             TEXT NOT NULL,
    summary             TEXT,               -- LLM-generated summary (null until consolidated)

    -- Salience scoring (0.0-1.0 scale)
    importance          FLOAT NOT NULL DEFAULT 0.5,
    emotional_tone      FLOAT NOT NULL DEFAULT 0.0,  -- -1.0 (neg) to 1.0 (pos)
    novelty             FLOAT NOT NULL DEFAULT 0.5,

    -- Extracted metadata (filled during consolidation)
    topics              TEXT[] NOT NULL DEFAULT '{}',
    entities            TEXT[] NOT NULL DEFAULT '{}',

    -- Lifecycle
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    consolidated_at     TIMESTAMPTZ,        -- NULL = not yet consolidated
    retrieval_count     INTEGER NOT NULL DEFAULT 0,
    last_retrieved_at   TIMESTAMPTZ,

    -- Ebbinghaus decay
    salience            FLOAT NOT NULL DEFAULT 1.0,
    pruned              BOOLEAN NOT NULL DEFAULT FALSE,

    -- Provenance
    user_id             TEXT NOT NULL DEFAULT 'michael',
    tenant_id           TEXT NOT NULL DEFAULT 'modern-method'
);

CREATE INDEX IF NOT EXISTS idx_episodes_session        ON episodic_traces(session_id);
CREATE INDEX IF NOT EXISTS idx_episodes_agent          ON episodic_traces(agent_id);
CREATE INDEX IF NOT EXISTS idx_episodes_importance     ON episodic_traces(importance DESC);
CREATE INDEX IF NOT EXISTS idx_episodes_created        ON episodic_traces(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_episodes_unconsolidated ON episodic_traces(consolidated_at) WHERE consolidated_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_episodes_topics         ON episodic_traces USING GIN(topics);
CREATE INDEX IF NOT EXISTS idx_episodes_content_trgm   ON episodic_traces USING GIN(content gin_trgm_ops);

-- ============================================================
-- TABLE 3: semantic_facts
-- Promoted long-term facts: subject → predicate → object triples
-- ============================================================

CREATE TABLE IF NOT EXISTS semantic_facts (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Structured fact (RDF-style triple)
    kind            TEXT NOT NULL,          -- "fact", "decision", "preference", "entity", "relationship"
    statement       TEXT NOT NULL,          -- human-readable
    subject         TEXT NOT NULL,          -- e.g. "Michael"
    predicate       TEXT NOT NULL,          -- e.g. "prefers_language"
    object          TEXT NOT NULL,          -- e.g. "Rust"

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

    -- Ebbinghaus decay
    salience        FLOAT NOT NULL DEFAULT 1.0,
    pruned          BOOLEAN NOT NULL DEFAULT FALSE,

    -- Provenance
    user_id         TEXT NOT NULL DEFAULT 'michael',
    tenant_id       TEXT NOT NULL DEFAULT 'modern-method'
);

CREATE INDEX IF NOT EXISTS idx_facts_subject       ON semantic_facts(subject);
CREATE INDEX IF NOT EXISTS idx_facts_predicate     ON semantic_facts(predicate);
CREATE INDEX IF NOT EXISTS idx_facts_subject_pred  ON semantic_facts(subject, predicate);
CREATE INDEX IF NOT EXISTS idx_facts_kind          ON semantic_facts(kind);
CREATE INDEX IF NOT EXISTS idx_facts_confidence    ON semantic_facts(confidence DESC);
CREATE INDEX IF NOT EXISTS idx_facts_active        ON semantic_facts(pruned, superseded_by) WHERE pruned = FALSE AND superseded_by IS NULL;
CREATE INDEX IF NOT EXISTS idx_facts_topics        ON semantic_facts USING GIN(topics);
CREATE INDEX IF NOT EXISTS idx_facts_statement_trgm ON semantic_facts USING GIN(statement gin_trgm_ops);

-- ============================================================
-- TABLE 4: workflow_memories
-- Task and project trajectory memories (long retention)
-- ============================================================

CREATE TABLE IF NOT EXISTS workflow_memories (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workflow_id     TEXT NOT NULL,          -- e.g. "ethos-implementation"
    workflow_kind   TEXT NOT NULL,          -- "task", "project", "sprint"

    title           TEXT NOT NULL,
    description     TEXT,
    outcome         TEXT,                   -- filled when task completes
    status          TEXT NOT NULL DEFAULT 'active'
                        CHECK (status IN ('active', 'completed', 'abandoned', 'paused')),

    -- Linked memories
    linked_episodes UUID[] NOT NULL DEFAULT '{}',
    linked_facts    UUID[] NOT NULL DEFAULT '{}',

    -- Temporal
    started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ,
    last_active_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    retention_until TIMESTAMPTZ,            -- NULL = indefinite

    -- Provenance
    agent_id        TEXT NOT NULL,
    user_id         TEXT NOT NULL DEFAULT 'michael',
    tenant_id       TEXT NOT NULL DEFAULT 'modern-method',
    metadata        JSONB NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_workflows_id     ON workflow_memories(workflow_id);
CREATE INDEX IF NOT EXISTS idx_workflows_status ON workflow_memories(status);
CREATE INDEX IF NOT EXISTS idx_workflows_agent  ON workflow_memories(agent_id);
CREATE INDEX IF NOT EXISTS idx_workflows_active ON workflow_memories(last_active_at DESC);

-- ============================================================
-- TABLE 5: memory_vectors
-- pgvector embedding index — 768-dim Gemini embeddings
--
-- IMPORTANT: vector(768) is fixed at creation.
-- Changing dimensions requires a new migration with table rebuild.
-- ============================================================

CREATE TABLE IF NOT EXISTS memory_vectors (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    source_type     TEXT NOT NULL CHECK (source_type IN ('episode', 'fact', 'workflow', 'query')),
    source_id       UUID NOT NULL,

    -- The embedding vector (768 dimensions — Gemini gemini-embedding-001 with output_dimensionality=768)
    embedding       vector(768) NOT NULL,

    -- Embedding metadata
    model           TEXT NOT NULL DEFAULT 'gemini-embedding-001',
    task_type       TEXT NOT NULL DEFAULT 'RETRIEVAL_DOCUMENT',  -- RETRIEVAL_DOCUMENT or RETRIEVAL_QUERY
    dimensions      INTEGER NOT NULL DEFAULT 768,
    embedding_version INTEGER NOT NULL DEFAULT 1,

    -- Content snapshot for debugging
    content_snippet TEXT,

    -- Timestamps
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Provenance
    user_id         TEXT NOT NULL DEFAULT 'michael',
    tenant_id       TEXT NOT NULL DEFAULT 'modern-method'
);

-- HNSW index: approximate nearest neighbor, cosine distance
-- m=16 connections per node, ef_construction=64 build quality
-- cosine distance appropriate for Gemini embeddings (normalized vectors)
CREATE INDEX IF NOT EXISTS idx_vectors_hnsw
    ON memory_vectors
    USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);

CREATE INDEX IF NOT EXISTS idx_vectors_source  ON memory_vectors(source_type, source_id);
CREATE INDEX IF NOT EXISTS idx_vectors_version ON memory_vectors(embedding_version);

-- ============================================================
-- TABLE 6: memory_graph_links
-- Directed edges for spreading activation graph
-- ============================================================

CREATE TABLE IF NOT EXISTS memory_graph_links (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    from_type   TEXT NOT NULL CHECK (from_type IN ('episode', 'fact', 'workflow')),
    from_id     UUID NOT NULL,
    to_type     TEXT NOT NULL CHECK (to_type IN ('episode', 'fact', 'workflow')),
    to_id       UUID NOT NULL,

    -- Edge properties
    relation    TEXT NOT NULL,   -- "temporal_next", "semantic_similar", "derived_from", "contradicts", "supports"
    weight      FLOAT NOT NULL DEFAULT 0.5 CHECK (weight BETWEEN 0.0 AND 1.0),

    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE(from_type, from_id, to_type, to_id, relation)
);

CREATE INDEX IF NOT EXISTS idx_links_from     ON memory_graph_links(from_type, from_id);
CREATE INDEX IF NOT EXISTS idx_links_to       ON memory_graph_links(to_type, to_id);
CREATE INDEX IF NOT EXISTS idx_links_relation ON memory_graph_links(relation);
CREATE INDEX IF NOT EXISTS idx_links_weight   ON memory_graph_links(weight DESC);

-- ============================================================
-- Done. Verify with:
--   \dt                                                (list all tables)
--   \d memory_vectors                                  (confirm vector column + HNSW index)
--   SELECT COUNT(*) FROM pg_indexes WHERE schemaname = 'public';  (count indexes)
-- ============================================================
