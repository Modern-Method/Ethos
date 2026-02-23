#!/usr/bin/env node

/**
 * Integration Test: ethos-context hook
 * 
 * Verifies end-to-end context injection:
 * 1. Ethos server running
 * 2. Ingest known memories
 * 3. Trigger message:received simulation
 * 4. Verify ETHOS_CONTEXT.md written with expected content
 */

import path from 'path';
import fs from 'fs';
import os from 'os';
import { fileURLToPath } from 'url';
import { dirname } from 'path';
import { createRequire } from 'module';

const require = createRequire(import.meta.url);
const { EthosClient } = require('/home/revenantpulse/Projects/ethos/ethos-ingest-ts/dist/client');

const SOCKET_PATH = process.env.ETHOS_SOCKET ?? '/tmp/ethos.sock';
const TEMP_WORKSPACE = path.join(os.tmpdir(), `ethos-context-integration-test-${Date.now()}`);

// Test data
const TEST_MEMORIES = [
  { content: 'Ethos uses gemini-embedding-001 with 768 dimensions for vector embeddings', source: 'assistant' },
  { content: 'The ethos-server is managed by systemd user service on port /tmp/ethos.sock', source: 'assistant' },
  { content: 'PostgreSQL pgvector extension requires superuser to install via CREATE EXTENSION', source: 'assistant' },
];

async function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

async function main() {
  console.log('=== Ethos Context Integration Test ===\n');
  
  let client;
  let passed = 0;
  let failed = 0;
  
  try {
    // Setup
    console.log('1. Setting up temp workspace...');
    fs.mkdirSync(TEMP_WORKSPACE, { recursive: true });
    console.log(`   ✓ Created: ${TEMP_WORKSPACE}\n`);
    
    // Connect to Ethos
    console.log('2. Connecting to Ethos server...');
    client = new EthosClient({ socketPath: SOCKET_PATH });
    await client.connect();
    console.log('   ✓ Connected\n');
    
    // Ingest test memories
    console.log('3. Ingesting test memories...');
    const ingestPromises = TEST_MEMORIES.map((mem, i) => {
      return client.request({
        action: 'ingest',
        payload: {
          content: mem.content,
          source: mem.source,
          metadata: { test: true, index: i }
        }
      }, 5000);
    });
    
    const ingestResults = await Promise.all(ingestPromises);
    console.log(`   ✓ Ingested ${ingestResults.length} memories\n`);
    
    // Wait for embeddings to complete
    console.log('4. Waiting for embeddings (5s)...');
    await sleep(5000);
    console.log('   ✓ Done\n');
    
    // Search for relevant memories
    console.log('5. Searching for relevant memories...');
    const searchResponse = await client.request({
      action: 'search',
      query: 'How do I configure embeddings in Ethos?',
      limit: 5
    }, 5000);
    
    if (searchResponse.status !== 'ok') {
      throw new Error(`Search failed: ${searchResponse.error}`);
    }
    
    console.log(`   ✓ Found ${searchResponse.data.count} results`);
    console.log(`   Top result score: ${searchResponse.data.results[0]?.score.toFixed(3) || 'N/A'}\n`);
    
    // Simulate hook behavior: filter by confidence gate
    console.log('6. Filtering by confidence gate (0.12)...');
    const CONFIDENCE_GATE = 0.12;
    const filtered = searchResponse.data.results.filter(r => r.score >= CONFIDENCE_GATE);
    console.log(`   ✓ ${filtered.length} results pass threshold\n`);
    
    // Format as markdown
    console.log('7. Formatting context markdown...');
    const timestamp = new Date().toISOString();
    const topScore = filtered[0]?.score;
    const scoreInfo = topScore !== undefined ? ` query_score=${topScore.toFixed(2)}` : '';
    
    let markdown = '';
    if (filtered.length > 0) {
      const lines = [
        `<!-- ethos:context ts=${timestamp}${scoreInfo} -->`,
        '## 🧠 Memory Context',
        '',
        `Relevant memories retrieved from Ethos (top ${filtered.length}, similarity ≥ ${CONFIDENCE_GATE}):`,
        ''
      ];
      
      filtered.forEach((result, index) => {
        const date = new Date(result.created_at).toISOString().split('T')[0];
        const score = result.score.toFixed(2);
        lines.push(`${index + 1}. **[${date}]** ${result.content} *(score: ${score})*`);
        lines.push('');
      });
      
      markdown = lines.join('\n');
    }
    console.log('   ✓ Formatted\n');
    
    // Write to workspace
    console.log('8. Writing ETHOS_CONTEXT.md to workspace...');
    const contextPath = path.join(TEMP_WORKSPACE, 'ETHOS_CONTEXT.md');
    fs.writeFileSync(contextPath, markdown, 'utf-8');
    console.log(`   ✓ Written to: ${contextPath}\n`);
    
    // Verify
    console.log('9. Verifying results...');
    
    // Check file exists
    if (!fs.existsSync(contextPath)) {
      throw new Error('ETHOS_CONTEXT.md was not created');
    }
    console.log('   ✓ File exists');
    passed++;
    
    // Check file not empty
    const content = fs.readFileSync(contextPath, 'utf-8');
    if (content.length === 0) {
      throw new Error('ETHOS_CONTEXT.md is empty');
    }
    console.log('   ✓ File not empty');
    passed++;
    
    // Check contains memory context header
    if (!content.includes('## 🧠 Memory Context')) {
      throw new Error('Missing memory context header');
    }
    console.log('   ✓ Contains header');
    passed++;
    
    // Check contains at least one test memory
    const hasTestMemory = TEST_MEMORIES.some(mem => content.includes(mem.content.substring(0, 50)));
    if (!hasTestMemory) {
      throw new Error('No test memories found in context');
    }
    console.log('   ✓ Contains test memory');
    passed++;
    
    // Check contains score metadata
    if (!content.includes('score:')) {
      throw new Error('Missing score metadata');
    }
    console.log('   ✓ Contains scores');
    passed++;
    
    console.log('\n=== TEST PASSED ===\n');
    console.log(`Passed: ${passed}/5`);
    
  } catch (err) {
    console.error('\n❌ TEST FAILED');
    console.error(err.message);
    console.error('\nStack trace:');
    console.error(err.stack);
    failed++;
    process.exit(1);
  } finally {
    // Cleanup
    if (client) {
      client.destroy();
    }
    if (fs.existsSync(TEMP_WORKSPACE)) {
      fs.rmSync(TEMP_WORKSPACE, { recursive: true, force: true });
    }
  }
}

main().catch(err => {
  console.error('Fatal error:', err);
  process.exit(1);
});
