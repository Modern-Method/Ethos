# Story 007 — Context Injection

**Status:** Ready for Development  
**Assigned:** Forge  
**Reviewer:** Sage  
**Priority:** High — completes the Ethos memory loop

---

## Goal

When a user sends a message, Ethos should automatically retrieve semantically relevant memories and inject them into the agent's context **before** the model generates a response. This closes the loop: passive recording (Story 003) + semantic search (Story 006) + automatic surfacing (this story).

---

## Why This Matters

Without context injection, Ethos is a write-only memory system. Agents can manually call `memory_search` but will never *automatically* benefit from their own history. This story is what makes Ethos neuromorphic — memories surface like the hippocampus does in the brain, without the agent having to consciously ask.

---

## Architecture

### Injection Mechanism

OpenClaw's `agent:bootstrap` hook fires **before** workspace files are injected into the agent's system prompt each turn. Hooks may mutate `context.bootstrapFiles` — appending entries here injects content directly into the model's context.

The flow:

```
message:received
  → [ethos-ingest]    records the raw message to DB
  → [ethos-context]   embeds the message → queries memory_vectors
                      → writes top-K results to ETHOS_CONTEXT.md
                        in the agent's workspace dir

agent:bootstrap
  → reads workspace files including ETHOS_CONTEXT.md
  → injects [System Memory] block into model context
  → model processes message WITH relevant memories already loaded
```

### Confidence Gating

Not every message warrants memory injection — injecting on every heartbeat or casual greeting wastes tokens. Use the `confidence_gate` from `ethos.toml` (default `0.12`) as a minimum similarity threshold:

- Top result score **≥ 0.75** → inject (high relevance)
- Top result score **0.12–0.74** → inject only if multiple results pass threshold
- Top result score **< 0.12** → skip injection, write empty/absent context file
- **Empty content / heartbeats** → always skip

### Output Format

`ETHOS_CONTEXT.md` written to `{workspaceDir}/ETHOS_CONTEXT.md`:

```markdown
<!-- ethos:context ts=2026-02-22T10:59:00Z query_score=0.87 -->
## 🧠 Memory Context

Relevant memories retrieved from Ethos (top 3, similarity ≥ 0.75):

1. **[2026-02-22]** Ethos uses gemini-embedding-001 with 768 dimensions. The outputDimensionality parameter must be set in the API request — default is 3072. *(score: 0.89)*

2. **[2026-02-22]** The ethos-server is managed by a systemd user service. Restart after builds with `./scripts/rebuild.sh`. *(score: 0.82)*

3. **[2026-02-21]** PostgreSQL pgvector extension requires superuser to install. Run CREATE EXTENSION vector as the postgres user, not as the ethos DB user. *(score: 0.77)*
```

If no results pass the confidence gate, write an **empty file** (so bootstrap doesn't error on a missing file — just no content injected).

---

## Implementation Plan

### New Crate: `ethos-context-hook`

A TypeScript OpenClaw hook at `~/.openclaw/hooks/ethos-context/`.

#### Files

```
~/.openclaw/hooks/ethos-context/
├── HOOK.md       # Metadata — events: message:received, agent:bootstrap
└── handler.ts    # Implementation
```

#### handler.ts Logic

```
On message:received:
  1. Extract content from event.context.content
  2. Skip if content is empty, heartbeat text, or < 10 chars
  3. Call EthosClient.send({ action: 'search', payload: { query: content, limit: 5 } })
  4. Receive SearchResults response
  5. Filter results below confidence_gate threshold
  6. Format as ETHOS_CONTEXT.md markdown block
  7. Write to {workspaceDir}/ETHOS_CONTEXT.md
     - workspaceDir = resolve agent workspace from sessionKey
     - Overwrite on every turn (always fresh)

On agent:bootstrap:
  - No action needed — ETHOS_CONTEXT.md is auto-loaded as a workspace file
    because it lives in the workspace dir. The model sees it automatically.
```

#### IPC: New Search Response Shape

The `EthosRequest::Search` (Story 006) already exists. The hook sends:

```json
{
  "action": "search",
  "payload": {
    "query": "<message content>",
    "limit": 5
  }
}
```

And receives (already implemented in retrieve.rs):

```json
{
  "status": "ok",
  "data": {
    "results": [
      {
        "id": "uuid",
        "content": "memory text",
        "score": 0.89,
        "metadata": { "channel": "telegram", "ts": "..." }
      }
    ]
  }
}
```

### Session → Workspace Mapping

The `agent:bootstrap` event provides `context.workspaceDir`. The `message:received` event provides `context.sessionKey`. We need to map `sessionKey → workspaceDir` to know where to write the file.

Options (pick one during implementation):
1. **Parse sessionKey**: OpenClaw session keys often encode the agentId (`neko:main`, etc.) — derive workspace from agentId
2. **Store mapping**: On `agent:bootstrap`, cache `sessionKey → workspaceDir` in module-level Map, then use it in `message:received`
3. **Fixed path**: For single-agent setups, hardcode to `~/.openclaw/underworld/ETHOS_CONTEXT.md`

**Recommended:** Option 2 (store mapping) — most correct for multi-agent setups.

---

## Acceptance Criteria

- [ ] `ETHOS_CONTEXT.md` is written to the correct workspace dir on every `message:received`
- [ ] Content includes memory entries with similarity scores and timestamps
- [ ] Empty file written (not absent) when no results pass confidence gate
- [ ] Agent context visibly reflects injected memories (verify with `openclaw logs`)
- [ ] Heartbeat messages and empty content are skipped (no unnecessary Gemini API calls)
- [ ] File is overwritten each turn — never stale
- [ ] Hook fails gracefully if Ethos server is down (write empty file, log warning, never crash)
- [ ] Unit tests: score filtering, markdown formatting, empty-result path
- [ ] Integration test: full turn — message in → ETHOS_CONTEXT.md written with relevant content

---

## Test Plan

### Unit Tests (Jest/ts-jest)

```typescript
describe('ethos-context hook', () => {
  test('formats results as markdown block')
  test('filters results below confidence gate')
  test('writes empty file when no results pass gate')
  test('skips heartbeat messages (< 10 chars)')
  test('handles Ethos server down gracefully')
  test('overwrites file on each call (no append)')
})
```

### Integration Test

1. Start ethos-server
2. Ingest 5 known memories via `EthosClient.send(ingest)`
3. Trigger a simulated `message:received` event
4. Assert `ETHOS_CONTEXT.md` exists and contains expected memory entries
5. Assert empty file written when query has no matches

---

## Dependencies

| Dependency | Status |
|------------|--------|
| Story 006 (Retrieval — `EthosRequest::Search`) | ✅ Complete |
| `ethos-ingest-ts/dist/client.js` (EthosClient) | ✅ Available |
| OpenClaw hook system (`agent:bootstrap` + `message:received`) | ✅ Confirmed |
| `ethos.toml` `[retrieval].confidence_gate` | ✅ Configured (0.12) |

---

## Runbook

To be written at `docs/runbooks/context-injection.md` as part of implementation.

---

## Notes

- `ETHOS_CONTEXT.md` should be **gitignored** — it's ephemeral, regenerated every turn
- If the confidence gate is too aggressive (no memories ever injected), lower `confidence_gate` in `ethos.toml`
- Story 008 (future): spreading activation — instead of top-K cosine, traverse the knowledge graph for richer context
- The 1-turn lag is acceptable for chat (memories from last exchange are still highly relevant to current exchange). Zero-lag injection requires awaitable hooks — worth investigating with OpenClaw team.
