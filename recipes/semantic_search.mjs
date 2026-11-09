#!/usr/bin/env node
/**
 * Semantic + hybrid search over a real knowledge base — zero dependencies.
 *
 * JavaScript / Node.js ES module port of `semantic_search.py`.
 * Uses only Node.js standard library (native `fetch`, `node:fs`, `node:path`).
 * Zero npm dependencies, zero build steps.
 *
 * Indexes the 40-article AI engineering KB that ships in this repo
 * (demo/data/ai_kb.ndjson) into a `semantic_text` field. XERJ embeds every
 * document at ingest with its built-in embedder (no external services, no
 * API keys), then answers the same question three ways:
 *
 *   1. match     — classic BM25 keyword search
 *   2. semantic  — embed the query, k-NN over the document vectors
 *   3. hybrid    — BM25 + semantic fused with Reciprocal Rank Fusion
 *
 * Run it:
 *     xerj --insecure --data-dir ./data &        # start XERJ
 *     node recipes/semantic_search.mjs
 *
 * Optional env:
 *     XERJ_URL   (default http://localhost:9200)
 */

import { readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const XERJ = process.env.XERJ_URL || 'http://localhost:9200';
const KB_PATH = resolve(__dirname, '../demo/data/ai_kb.ndjson');
const INDEX = 'ai-kb';

/**
 * Minimal HTTP helper using standard Node 18+ global fetch.
 * No external HTTP client required.
 */
async function call(method, path, body = null) {
  const headers = {};
  let payload = undefined;

  if (body !== null) {
    if (typeof body === 'string') {
      payload = body;
      headers['Content-Type'] = 'application/x-ndjson';
    } else {
      payload = JSON.stringify(body);
      headers['Content-Type'] = 'application/json';
    }
  }

  const response = await fetch(`${XERJ}${path}`, {
    method,
    headers,
    body: payload,
  });

  if (!response.ok && response.status !== 404) {
    const errorText = await response.text();
    throw new Error(`HTTP ${response.status} ${method} ${path}: ${errorText}`);
  }

  return response.json();
}

async function main() {
  // ── 1. Fresh index: `content` is semantic_text, so XERJ embeds it at
  //       ingest time and `semantic` queries embed the question the same
  //       way. `category`/`source` stay keyword for exact filtering.
  try {
    await call('DELETE', `/${INDEX}`);
  } catch {
    // didn't exist yet
  }

  await call('PUT', `/${INDEX}`, {
    mappings: {
      properties: {
        title:    { type: 'text' },
        content:  { type: 'semantic_text', dimensions: 384 },
        category: { type: 'keyword' },
        source:   { type: 'keyword' },
      },
    },
  });

  // ── 2. Bulk-ingest the real KB (40 hand-written articles).
  const rawKb = readFileSync(KB_PATH, 'utf-8');
  const bulkLines = [];

  for (const line of rawKb.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;

    const doc = JSON.parse(trimmed);
    delete doc.embedding; // let semantic_text embed the content itself

    bulkLines.push(JSON.stringify({ index: { _index: INDEX, _id: String(doc.id) } }));
    bulkLines.push(JSON.stringify(doc));
  }

  await call('POST', '/_bulk?refresh=true', bulkLines.join('\n') + '\n');
  const countRes = await call('GET', `/${INDEX}/_count`);
  console.log(`indexed ${countRes.count} KB articles into \`${INDEX}\`\n`);

  // ── 3. Ask the same question three ways.
  const question = "how do I keep an LLM agent's context from overflowing?";

  const modes = {
    'match (BM25)': { match: { content: question } },
    'semantic':     { semantic: { field: 'content', query: question, k: 5 } },
    'hybrid (RRF)': {
      hybrid: {
        queries: [
          { query: { match: { content: question } }, weight: 1.0 },
          { query: { semantic: { field: 'content', query: question, k: 10 } }, weight: 1.0 },
        ],
      },
    },
  };

  console.log(`Q: ${question}\n`);

  for (const [name, query] of Object.entries(modes)) {
    const searchRes = await call('POST', `/${INDEX}/_search`, { query, size: 3 });
    const hits = searchRes.hits?.hits || [];

    console.log(`── ${name}`);
    for (const h of hits) {
      const src = h._source || {};
      const score = (h._score || 0).toFixed(3).padStart(6, ' ');
      const cat = (src.category || '').padStart(9, ' ');
      console.log(`   ${score}  [${cat}]  ${src.title}`);
    }
    console.log();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
