-- Story 004: DB Ingest Extensions
-- These tables support the high-velocity raw ingest log before consolidation.

-- Raw session event log (pre-consolidation/pre-episodic)
CREATE TABLE IF NOT EXISTS session_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('user','assistant','system','tool')),
    content TEXT NOT NULL,
    tokens INTEGER,
    metadata JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Adjust memory_vectors to allow NULL vectors temporarily for ingest pipeline
ALTER TABLE memory_vectors ALTER COLUMN embedding DROP NOT NULL;
ALTER TABLE memory_vectors RENAME COLUMN embedding TO vector;
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS source TEXT;
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS content TEXT;
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS importance FLOAT DEFAULT 0.5;
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS access_count INTEGER DEFAULT 0;
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS last_accessed TIMESTAMPTZ;
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;

-- Allow source_type and source_id to be NULL temporarily for ingest pipeline (until promoted)
ALTER TABLE memory_vectors ALTER COLUMN source_type DROP NOT NULL;
ALTER TABLE memory_vectors ALTER COLUMN source_id DROP NOT NULL;
ALTER TABLE memory_vectors ALTER COLUMN model DROP NOT NULL;
ALTER TABLE memory_vectors ALTER COLUMN task_type DROP NOT NULL;
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS metadata JSONB DEFAULT '{}';
