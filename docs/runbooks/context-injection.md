# Runbook: Ethos Context Injection

**Last Updated:** 2026-02-22  
**Author:** Forge  
**Story:** 007 — Context Injection

---

## Overview

The `ethos-context` hook automatically injects relevant memories into an agent's context each turn. It listens for `message:received` events, searches Ethos for semantically similar memories, and writes them to `ETHOS_CONTEXT.md` in the agent's workspace directory.

This file is automatically loaded by OpenClaw's bootstrap system, injecting memories directly into the model's context before it generates a response.

---

## How It Works

### Event Flow

```
1. agent:bootstrap
   → Cache sessionKey → workspaceDir mapping

2. message:received
   → Check if content qualifies (length, not heartbeat)
   → Search Ethos via IPC
   → Filter results by confidence gate (0.12)
   → Write ETHOS_CONTEXT.md to workspace

3. agent:bootstrap (next turn)
   → OpenClaw loads ETHOS_CONTEXT.md
   → Model sees memories in context
```

### Confidence Gating

Not every message warrants memory injection. The hook uses a confidence gate threshold (default `0.12` from `ethos.toml`):

- **Score ≥ 0.75**: High relevance — inject
- **Score 0.12–0.74**: Medium relevance — inject only if multiple results pass
- **Score < 0.12**: Low relevance — write empty file (no injection)

### Content Filtering

The hook skips:
- Empty messages
- Messages < 10 characters
- Heartbeat messages (containing "heartbeat")
- Ping/pong messages

---

## Configuration

### Environment Variables

- `ETHOS_SOCKET`: Path to Ethos IPC socket (default: `/tmp/ethos.sock`)

### Hook Metadata

Location: `~/.openclaw/hooks/ethos-context/HOOK.md`

```yaml
events:
  - message:received
  - agent:bootstrap
```

### Confidence Gate

Configured in `/home/revenantpulse/Projects/ethos/ethos.toml`:

```toml
[retrieval]
confidence_gate = 0.12
```

---

## Deployment

### Prerequisites

1. **Ethos server running**:
   ```bash
   systemctl --user status ethos-server
   ```

2. **EthosClient compiled**:
   ```bash
   ls -l ~/Projects/ethos/ethos-ingest-ts/dist/client.js
   ```

3. **Hook compiled**:
   ```bash
   cd ~/.openclaw/hooks/ethos-context
   npm install
   npm run build
   ```

### Installation

The hook is automatically loaded by OpenClaw when it starts. No manual registration needed — OpenClaw scans `~/.openclaw/hooks/` for `HOOK.md` files.

### Verification

Check that the hook is loaded:

```bash
# Start a session and check logs
openclaw logs | grep ethos-context
```

Verify ETHOS_CONTEXT.md is being written:

```bash
# Watch the file in real-time
tail -f ~/.openclaw/underworld/ETHOS_CONTEXT.md
```

---

## Monitoring

### Log Messages

**Successful context injection:**
```
[ethos-context] Connected to Ethos at /tmp/ethos.sock
```

**Ethos server down:**
```
[ethos-context] Initial connect failed (will retry): connect ENOENT /tmp/ethos.sock
[ethos-context] Search failed: Request 1 timed out after 3000ms
```

**No workspace mapping:**
```
[ethos-context] No workspace mapping for session: unknown-session
```

### Health Checks

1. **Check Ethos server:**
   ```bash
   systemctl --user status ethos-server
   journalctl --user -u ethos-server -f
   ```

2. **Test IPC connection:**
   ```bash
   # Manually test search
   node -e "
     const { EthosClient } = require('/home/revenantpulse/Projects/ethos/ethos-ingest-ts/dist/client');
     const client = new EthosClient({ socketPath: '/tmp/ethos.sock' });
     client.connect().then(() => {
       return client.request({ action: 'search', payload: { query: 'test', limit: 5 } });
     }).then(console.log).catch(console.error);
   "
   ```

3. **Verify hook handler:**
   ```bash
   cd ~/.openclaw/hooks/ethos-context
   npm test
   ```

---

## Troubleshooting

### Issue: ETHOS_CONTEXT.md not being written

**Symptoms:**
- File doesn't exist in workspace
- No memory context injected into model

**Diagnosis:**
1. Check hook is loaded:
   ```bash
   ls -l ~/.openclaw/hooks/ethos-context/dist/handler.js
   ```

2. Check Ethos server is running:
   ```bash
   systemctl --user status ethos-server
   ```

3. Check session has workspace mapping:
   - Verify `agent:bootstrap` event fired
   - Check logs for "No workspace mapping" warning

**Resolution:**
- Restart OpenClaw to reload hooks
- Ensure Ethos server is healthy
- Check that agent:bootstrap fires before message:received

### Issue: Empty file written even when memories exist

**Symptoms:**
- ETHOS_CONTEXT.md exists but is empty
- Memories exist in database but don't appear

**Diagnosis:**
1. Check confidence gate threshold:
   ```bash
   grep confidence_gate /home/revenantpulse/Projects/ethos/ethos.toml
   ```

2. Check memory scores:
   ```bash
   # Manually search to see scores
   node -e "
     const { EthosClient } = require('/home/revenantpulse/Projects/ethos/ethos-ingest-ts/dist/client');
     const client = new EthosClient({ socketPath: '/tmp/ethos.sock' });
     client.connect().then(() => {
       return client.request({ action: 'search', payload: { query: 'your test query', limit: 5 } });
     }).then(r => {
       r.data.results.forEach(m => console.log(m.score, m.content));
     }).catch(console.error);
   "
   ```

**Resolution:**
- Lower `confidence_gate` in `ethos.toml` if too aggressive
- Ensure memories have vectors (check `memory_vectors` table)
- Verify embedding API is working

### Issue: Hook crashes on startup

**Symptoms:**
- OpenClaw fails to load hook
- Error in logs about missing module

**Diagnosis:**
1. Check dependencies:
   ```bash
   cd ~/.openclaw/hooks/ethos-context
   npm install
   ```

2. Check EthosClient path:
   ```bash
   ls -l /home/revenantpulse/Projects/ethos/ethos-ingest-ts/dist/client.js
   ```

**Resolution:**
- Run `npm install` in hook directory
- Rebuild EthosClient: `cd ~/Projects/ethos/ethos-ingest-ts && npm run build`

---

## Performance Considerations

### IPC Latency

- Search requests timeout after 3000ms
- If Ethos is slow, empty file is written (graceful degradation)
- Average search latency: 50-200ms (embedding + pgvector query)

### Token Usage

- Each injected memory costs tokens in the model context
- Top-K limit: 5 results
- Average memory length: 100-500 characters
- Estimated token cost per injection: 50-250 tokens

### Optimization Tips

1. **Adjust confidence gate** to reduce low-relevance injections
2. **Lower top-K limit** in handler.ts if token usage too high
3. **Monitor injection frequency** via logs

---

## Security Considerations

### File Permissions

- `ETHOS_CONTEXT.md` written with user permissions
- Workspace directory should be user-owned
- No elevated permissions required

### Data Privacy

- Memories may contain sensitive conversation data
- Context file is ephemeral (overwritten each turn)
- Consider `.gitignore` for workspace directories

### IPC Security

- Socket path `/tmp/ethos.sock` is user-specific
- Only local processes can connect
- No network exposure

---

## Testing

### Unit Tests

```bash
cd ~/.openclaw/hooks/ethos-context
npm test
```

Coverage: >90% (21 tests)

### Integration Test

1. Start Ethos server:
   ```bash
   systemctl --user start ethos-server
   ```

2. Ingest test data:
   ```bash
   # Use ethos-ingest hook or manual ingest
   ```

3. Trigger message:received event:
   ```bash
   # Send a message to the agent
   ```

4. Verify ETHOS_CONTEXT.md:
   ```bash
   cat ~/.openclaw/underworld/ETHOS_CONTEXT.md
   ```

---

## Related Documentation

- [Story 007 Spec](/home/revenantpulse/Projects/ethos/docs/stories/story-007-context-injection.md)
- [Ethos Architecture](/home/revenantpulse/Projects/ethos/README.md)
- [Retrieval Subsystem Runbook](/home/revenantpulse/Projects/ethos/docs/runbooks/retrieval.md)

---

## Changelog

### 2026-02-22 — Initial Implementation (Story 007)

- Created ethos-context hook
- Implemented message:received handler
- Implemented agent:bootstrap handler
- Added confidence gating (0.12 threshold)
- Added graceful degradation for Ethos-down scenarios
- 21 unit tests with >90% coverage
- Integration test verified
