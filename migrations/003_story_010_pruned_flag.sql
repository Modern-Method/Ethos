-- Story 010: Add pruned column to memory_vectors for decay support
-- Migration: 003
-- Date: 2026-02-23
-- Author: Forge (Backend Engineer)

-- Add pruned column to memory_vectors if it doesn't exist
ALTER TABLE memory_vectors ADD COLUMN IF NOT EXISTS pruned BOOLEAN NOT NULL DEFAULT FALSE;

-- Create index for decay sweep queries
CREATE INDEX IF NOT EXISTS idx_vectors_pruned ON memory_vectors(pruned) WHERE pruned = FALSE;
