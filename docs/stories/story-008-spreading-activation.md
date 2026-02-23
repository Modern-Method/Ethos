# Story 008 — Spreading Activation

**Status:** Ready for Development  
**Assigned:** Forge  
**Reviewer:** Sage  
**Priority:** High — upgrades retrieval from keyword matching to associative memory

---

## Goal

Replace the current top-K cosine retrieval (Story 006) with **spreading activation** — a graph-based retrieval algorithm modeled on how the hippocampus actually surfaces memories. When you think of "Paris," you also think of "Eiffel Tower," "baguettes," and that trip you took in 2019. Not because those are the most *similar* vectors to "Paris," but because they're *connected*.

---

## Background: The Neuroscience

Spreading activation was first proposed by Collins & Loftus (1975) to model human semantic memory. The key insight: memory retrieval isn't just pattern matching — it's **associative propagation** through a network.

In the brain:
- Neurons that fire together, wire together (Hebb's rule)
- Activating one memory node propagates energy to connected nodes
- Activation decays with each hop (not all associations are equally strong)
- The result: contextually rich retrieval, not just nearest-neighbor lookup

In Ethos:
- **Anchor nodes** = top-K cosine matches from `memory_vectors` (Story 006)
- **Graph** = `memory_graph_links` (already in DB — edges between episodes, facts, workflows)
- **Propagation** = iterative activation spread across edge weights
- **Combined score** = `weight_similarity * cosine + weight_activation * spread + weight_structural * graph_position`

Config (already in `ethos.toml`):
```toml
[retrieval]
spreading_strength = 0.85    # Activation decay per hop (< 1.0)
iterations = 3               # Propagation depth
weight_similarity = 0.5      # Cosine score weight in final ranking
weight_activation = 0.3      # Spread activation weight
weight_structural = 0.2      # Graph centrality weight
anchor_top_k_episodes = 10   # Anchor pool from cosine search
anchor_top_k_facts = 10
```

---

## Architecture

### Two-Phase Retrieval

```
Phase 1: Anchor (existing cosine search)
  embed(query) → pgvector <=> cosine → top-10 anchor nodes

Phase 2: Spread
  For each anchor node:
    activation[node] = cosine_score
  
  For iteration in 1..=3:
    For each node with activation > 0:
      For each edge (node → neighbor, weight):
        activation[neighbor] += activation[node] * weight * spreading_strength
  
  Final score = weight_similarity * cosine
              + weight_activation * spread_activation
              + weight_structural * (in_degree / max_in_degree)
  
  Return top-K by final_score
```

### Graph Link Creation (New)

Currently `memory_graph_links` is empty — nothing creates edges. This story adds **automatic link creation** during ingest:

When a new memory is ingested:
1. Find top-3 similar existing memories (cosine similarity)
2. For each, if similarity ≥ 0.6: create or strengthen a `memory_graph_links` edge
   - New edge: `weight = similarity_score`
   - Existing edge: `weight = min(1.0, old_weight + 0.1)` (Hebbian strengthening)
3. Also link based on shared `topics[]` / `entities[]` from `episodic_traces`

This builds the associative graph organically over time — every new memory self-indexes into the network.

---

## New IPC Request Type

Extend `EthosRequest` in `ethos-core/src/ipc.rs`:

```rust
Search {
    query: String,
    limit: Option<usize>,
    #[serde(default)]
    use_spreading: bool,   // NEW — false = pure cosine (Story 006), true = spreading activation
}
```

Default `use_spreading = false` for backward compat. The `ethos-context` hook should set `use_spreading: true`.

---

## Implementation Plan

### 1. `ethos-core/src/graph.rs` (new)

```rust
pub struct ActivationNode {
    pub id: Uuid,
    pub node_type: String,
    pub cosine_score: f32,
    pub spread_score: f32,
    pub structural_score: f32,
    pub final_score: f32,
}

pub async fn spread_activation(
    pool: &PgPool,
    anchors: &[ActivationNode],
    config: &RetrievalConfig,
) -> Result<Vec<ActivationNode>, EthosError>
```

Algorithm in pure Rust — load subgraph from `memory_graph_links` for anchor IDs, run iterative propagation, return ranked nodes.

### 2. `ethos-server/src/subsystems/retrieve.rs` (extend)

- Update `search_memory()` to accept `use_spreading: bool`
- When true: run Phase 1 (cosine anchors) then Phase 2 (spread)
- When false: return Phase 1 results directly (existing behavior)

### 3. `ethos-server/src/subsystems/linker.rs` (new)

Background task that:
- Triggers after each successful ingest
- Finds top-3 similar memories to the new one
- Creates/strengthens `memory_graph_links` edges
- Updates `episodic_traces.topics` / `entities` match-based links

---

## Database Queries

### Load subgraph for spreading

```sql
SELECT from_id, to_id, to_type, weight
FROM memory_graph_links
WHERE from_id = ANY($1)   -- anchor IDs
   OR to_id = ANY($1)
ORDER BY weight DESC
LIMIT 500;
```

### Create or strengthen link (upsert)

```sql
INSERT INTO memory_graph_links 
  (from_type, from_id, to_type, to_id, relation, weight)
VALUES ($1, $2, $3, $4, 'similarity', $5)
ON CONFLICT (from_type, from_id, to_type, to_id, relation)
DO UPDATE SET 
  weight = LEAST(1.0, memory_graph_links.weight + 0.1),
  updated_at = now();
```

---

## Acceptance Criteria

- [ ] `use_spreading: true` returns richer results than pure cosine on a seeded test dataset
- [ ] `use_spreading: false` returns identical results to Story 006 (regression-safe)
- [ ] Graph edges created automatically after each ingest (verify with `SELECT COUNT(*) FROM memory_graph_links`)
- [ ] Spreading terminates in ≤ 3 iterations (no infinite loops)
- [ ] `spreading_strength = 0.0` collapses to pure cosine (mathematical edge case test)
- [ ] Empty graph (no edges) → gracefully returns cosine results only
- [ ] Unit tests: activation math, edge cases (empty graph, single node, cycle detection)
- [ ] Integration test: seed 10 memories with known links → verify related memories surface
- [ ] Runbook at `docs/runbooks/spreading-activation.md`

---

## Test Plan

### Unit Tests (Rust)

```rust
#[test] fn test_spread_single_anchor_no_edges()
#[test] fn test_spread_propagates_through_edges()
#[test] fn test_spread_decays_with_strength()
#[test] fn test_spread_zero_strength_returns_anchors_only()
#[test] fn test_spread_handles_cycles_safely()
#[test] fn test_final_score_weights_sum_correctly()
#[test] fn test_linker_creates_edge_above_threshold()
#[test] fn test_linker_strengthens_existing_edge()
#[test] fn test_linker_skips_below_threshold()
```

### Integration Test

1. Ingest 10 memories about Ethos architecture (related topics)
2. Ingest 2 unrelated memories (weather, recipes)
3. Query: "how does the Ethos IPC protocol work?"
4. Assert: results include memories about msgpack, Unix sockets, and Rust — even if they weren't the top cosine matches individually
5. Assert: unrelated memories not in results

---

## Dependencies

| Dependency | Status |
|------------|--------|
| Story 006 (Retrieval — cosine search) | ✅ Complete |
| `memory_graph_links` table | ✅ In DB |
| `episodic_traces.topics` / `entities` columns | ✅ In DB |
| `ethos.toml` `[retrieval]` spreading config | ✅ Configured |

---

## Notes

- Graph traversal should use a `HashMap<Uuid, f32>` for activation scores — never recurse (iterative only, no stack overflow risk)
- Cap subgraph load at 500 edges to bound memory usage
- Story 009 (consolidation) will add `episode → semantic_fact` links to the graph — spreading activation will automatically benefit from those without code changes
- This is the feature that makes Ethos feel genuinely associative — the difference between a database and a memory
