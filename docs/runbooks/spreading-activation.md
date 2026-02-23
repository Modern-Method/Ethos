# Spreading Activation — Runbook

**Story:** 008
**Status:** Complete
**Last Updated:** 2026-02-22

---

## Overview

Spreading activation replaces pure cosine retrieval with graph-based associative memory retrieval, modeled on hippocampal activation patterns. When you think of "Paris," you also recall connected concepts like "Eiffel Tower" and "baguettes" — not because they're semantically similar, but because they're *associated*.

---

## Architecture

### Two-Phase Retrieval

```
Phase 1: Anchor (existing cosine search)
  embed(query) → pgvector <=> cosine → top-K anchor nodes

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

### Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `graph.rs` | `ethos-core/src/` | Core spreading activation algorithm |
| `retrieve.rs` | `ethos-server/src/subsystems/` | Search with optional spreading |
| `linker.rs` | `ethos-server/src/subsystems/` | Auto-create graph edges on ingest |

---

## Configuration

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

## IPC Request

```json
{
  "action": "search",
  "query": "how does the IPC protocol work?",
  "limit": 5,
  "use_spreading": true
}
```

- `use_spreading: false` (default) — pure cosine search (Story 006 behavior)
- `use_spreading: true` — apply spreading activation over `memory_graph_links`

---

## Database Tables

### `memory_graph_links`

Edges between memories for spreading activation:

```sql
SELECT from_id, to_id, to_type, weight
FROM memory_graph_links
WHERE from_id = ANY($1) OR to_id = ANY($1)
ORDER BY weight DESC
LIMIT 500;
```

### Edge Creation (Linker)

After each ingest, `linker.rs`:
1. Finds top-3 similar existing memories
2. Creates edges for matches ≥ 0.6 similarity
3. Hebbian strengthening: `weight = min(1.0, old_weight + 0.1)`

```sql
INSERT INTO memory_graph_links (from_type, from_id, to_type, to_id, relation, weight)
VALUES ($1, $2, $3, $4, 'similarity', $5)
ON CONFLICT (from_type, from_id, to_type, to_id, relation)
DO UPDATE SET weight = LEAST(1.0, memory_graph_links.weight + 0.1);
```

---

## Edge Cases

| Scenario | Behavior |
|----------|----------|
| Empty graph (no edges) | Returns cosine results only |
| `spreading_strength = 0.0` | Collapses to pure cosine |
| Cycles in graph | Handled safely (iterative, not recursive) |
| Single anchor node | Returns anchor with cosine-based score |
| Database error | Returns `EthosError::Database` |

---

## Testing

### Unit Tests (35 total)

**`graph.rs` (14 tests):**
- `test_spread_single_anchor_no_edges`
- `test_spread_propagates_through_edges`
- `test_spread_decays_with_strength`
- `test_spread_zero_strength_returns_anchors_only`
- `test_spread_handles_cycles_safely`
- `test_final_score_weights_sum_correctly`
- `test_spread_empty_anchors_returns_empty`
- `test_spread_multiple_anchors_accumulate`
- `test_structural_score_centrality`
- (plus embedding tests)

**`linker.rs` (5 tests):**
- `test_linker_creates_edge_above_threshold`
- `test_linker_strengthens_existing_edge`
- `test_linker_skips_below_threshold`
- `test_linker_weight_caps_at_max`
- `test_linker_finds_top_k`

**`retrieve.rs` (21 tests):**
- All Story 006 tests (backward compat)
- `test_search_with_spreading_activation_backward_compat`
- `test_search_spreading_zero_strength_equals_cosine`

### Running Tests

```bash
# All tests
cargo test --lib

# Coverage
cargo tarpaulin --lib --out Stdout

# Clippy
cargo clippy -- -D warnings
```

---

## Debugging

### Check Graph Links

```sql
SELECT COUNT(*) FROM memory_graph_links;
SELECT * FROM memory_graph_links ORDER BY weight DESC LIMIT 10;
```

### Manual Link Creation

```sql
INSERT INTO memory_graph_links (from_type, from_id, to_type, to_id, relation, weight)
VALUES ('episode', '<uuid>', 'fact', '<uuid>', 'similarity', 0.8);
```

### Trace Spreading

Enable debug logging:
```toml
[service]
log_level = "debug"
```

---

## Future Enhancements (Story 009+)

- Episode → semantic_fact links (consolidation)
- Topic/entity-based structural linking
- Temporal edges (`temporal_next` relation)
- Contradiction detection (`contradicts` relation)

---

## References

- Collins & Loftus (1975) — Spreading activation theory
- Hebb's Rule — "Neurons that fire together, wire together"
- Story 006 — Pure cosine retrieval baseline
