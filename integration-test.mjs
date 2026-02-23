/**
 * Ethos End-to-End Integration Test
 * Tests: TypeScript hook framing → Rust server → response
 *
 * Run: node integration-test.mjs
 * Requires: ethos-server running (or started by this script)
 */

import { createConnection } from 'net';
import { encode, decode } from '@msgpack/msgpack';
import { execFile, spawn } from 'child_process';
import { promisify } from 'util';
import { setTimeout as sleep } from 'timers/promises';

const SOCKET = '/tmp/ethos.sock'; // matches ethos.toml default

// ─── Framing helpers ────────────────────────────────────────────────────────

function frameMessage(obj) {
  const payload = encode(obj);
  const header = Buffer.allocUnsafe(4);
  header.writeUInt32LE(payload.length, 0);
  return Buffer.concat([header, Buffer.from(payload)]);
}

function readFrame(socket) {
  return new Promise((resolve, reject) => {
    let buf = Buffer.alloc(0);

    const onData = (chunk) => {
      buf = Buffer.concat([buf, chunk]);
      if (buf.length < 4) return;
      const len = buf.readUInt32LE(0);
      if (buf.length < 4 + len) return;
      socket.off('data', onData);
      socket.off('error', onError);
      resolve(decode(buf.slice(4, 4 + len)));
    };

    const onError = (err) => {
      socket.off('data', onData);
      socket.off('error', onError);
      reject(err);
    };

    socket.on('data', onData);
    socket.on('error', onError);
  });
}

// ─── RPC helper ─────────────────────────────────────────────────────────────

async function rpc(socket, request) {
  socket.write(frameMessage(request));
  return await readFrame(socket);
}

// ─── Tests ──────────────────────────────────────────────────────────────────

async function runTests() {
  console.log('\n🔧 Starting ethos-server for integration test...');

  const server = spawn(
    'cargo',
    ['run', '--bin', 'ethos-server'],
    {
      cwd: '/home/revenantpulse/Projects/ethos',
      env: { ...process.env },
      stdio: ['ignore', 'pipe', 'pipe'],
    }
  );

  // Capture server output
  server.stdout.on('data', d => process.stdout.write(`  [server] ${d}`));
  server.stderr.on('data', d => process.stdout.write(`  [server] ${d}`));

  // Give server time to start
  await sleep(2000);

  let passed = 0;
  let failed = 0;

  const socket = createConnection(SOCKET);
  await new Promise((res, rej) => {
    socket.once('connect', res);
    socket.once('error', rej);
  });
  console.log('  ✅ Socket connected\n');

  // ── Test 1: Ping ──────────────────────────────────────────────────────────
  console.log('📋 Test 1: Ping');
  try {
    const resp = await rpc(socket, { action: 'ping' });
    if (resp.status === 'ok' && resp.data?.pong === true) {
      console.log('  ✅ PASS — got pong');
      passed++;
    } else {
      console.log(`  ❌ FAIL — unexpected response: ${JSON.stringify(resp)}`);
      failed++;
    }
  } catch (e) {
    console.log(`  ❌ FAIL — ${e.message}`);
    failed++;
  }

  // ── Test 2: Health ────────────────────────────────────────────────────────
  console.log('\n📋 Test 2: Health check');
  try {
    const resp = await rpc(socket, { action: 'health' });
    if (resp.status === 'ok' && resp.data?.status === 'healthy') {
      console.log(`  ✅ PASS — PG: ${resp.data.postgresql?.split(' on ')[0]}, pgvector: ${resp.data.pgvector}`);
      passed++;
    } else {
      console.log(`  ❌ FAIL — ${JSON.stringify(resp)}`);
      failed++;
    }
  } catch (e) {
    console.log(`  ❌ FAIL — ${e.message}`);
    failed++;
  }

  // ── Test 3: Ingest (correct format matching Rust IPC spec) ───────────────
  console.log('\n📋 Test 3: Ingest — {action:"ingest", payload:{content, source}}');
  try {
    const resp = await rpc(socket, {
      action: 'ingest',
      payload: { content: 'Hello Ethos! Integration test message.', source: 'user' }
    });
    if (resp.status === 'ok' && resp.data?.queued === true) {
      console.log('  ✅ PASS — message accepted by server (stub: logged to tracing)');
      passed++;
    } else {
      console.log(`  ❌ FAIL — ${JSON.stringify(resp)}`);
      failed++;
    }
  } catch (e) {
    console.log(`  ❌ FAIL — ${e.message}`);
    failed++;
  }

  // ── Test 4: Old TypeScript format correctly rejected ─────────────────────
  console.log('\n📋 Test 4: Protocol guard — old {type:"Ingest"} format is rejected');
  try {
    const resp = await rpc(socket, {
      type: 'Ingest',
      content: 'Old format that should fail.',
      source: 'user'
    });
    if (resp.status === 'error' && resp.error?.includes('missing field')) {
      console.log(`  ✅ PASS — correctly rejected: "${resp.error}"`);
      passed++;
    } else {
      console.log(`  ❌ FAIL — should have been rejected: ${JSON.stringify(resp)}`);
      failed++;
    }
  } catch (e) {
    console.log(`  ❌ Error — ${e.message}`);
    failed++;
  }

  socket.destroy();
  server.kill();

  // ── Summary ───────────────────────────────────────────────────────────────
  console.log('\n════════════════════════════════════');
  console.log(`Integration Test Results: ${passed} passed, ${failed} failed`);
  console.log('════════════════════════════════════\n');
  process.exit(failed > 0 ? 1 : 0);
}

runTests().catch(err => {
  console.error('Fatal:', err);
  process.exit(1);
});
