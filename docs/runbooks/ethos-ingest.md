# Runbook: Ethos Ingest Hook (TypeScript)

## Overview
The `ethos-ingest` hook is a TypeScript-based integration for OpenClaw that captures message events and pipes them into the Ethos memory engine via a Unix Domain Socket.

- **Source Dir**: `ethos-ingest-ts/`
- **Protocol**: MessagePack over Unix Socket with 4-byte Little Endian length prefix.
- **Default Socket**: `/tmp/ethos.sock`

## Installation
The hook is intended to be loaded by OpenClaw. If running manually for development:
```bash
cd ethos-ingest-ts
npm install
npm run build
```

## Configuration
- `ETHOS_SOCKET`: Path to the Ethos IPC socket (default: `/tmp/ethos.sock`).

## Operations

### Testing
Run the test suite with:
```bash
npm test
```

### Coverage
Check test coverage (requires >90%):
```bash
npm run test:coverage
```

### Troubleshooting
- **Connection Refused**: Ensure `ethos-server` is running and listening on the socket path. The ingest hook will automatically retry with exponential backoff.
- **Message Dropping**: The hook is designed to be non-blocking. If the socket is unavailable, messages are logged as dropped but will not crash the agent.

## Implementation Details
- Uses `msgpack5` for serialization.
- Implements a framing protocol: `[4-byte LE Length] + [MessagePack Payload]`.
- Listens to `message:received` (user) and `message:sent` (assistant) events.
