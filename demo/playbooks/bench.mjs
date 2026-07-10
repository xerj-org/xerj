#!/usr/bin/env node
// Xerj hot-path benchmark — bulk ingest throughput + query/agg/kNN latency.
//
// Self-contained: (re)creates a `perf` index, ingests a generated dataset, then
// times the read paths. Numbers are written to BENCHMARK.md.
//
// Usage: node bench.mjs [docCount]   (default 200000)
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

// ── DEPRECATION GUARD (quarantine) — H5 ─────────────────────────────────────
// This harness uses a NON-CANONICAL transport (Node's global fetch/undici, no
// shared keep-alive agent) and writes a canonical benchmark file (BENCHMARK.md).
// The canonical, honesty-audited harness is demo/playbooks/bench-matrix.mjs.
// Refuse to run (exit 2) unless the operator explicitly sets FORCE_LEGACY_BENCH=1.
process.stderr.write(
  'DEPRECATED — use bench-matrix.mjs; this harness uses a non-canonical transport ' +
  'and must not overwrite SCORECARD.md/BENCHMARK_VS_ES.md\n');
if (process.env.FORCE_LEGACY_BENCH !== '1') {
  process.stderr.write('   refusing to run (would overwrite a canonical benchmark file). Set FORCE_LEGACY_BENCH=1 to override.\n');
  process.exit(2);
}

const ES = 'http://localhost:9200';
const ROOT = path.dirname(new URL(import.meta.url).pathname);
const N = parseInt(process.argv[2] || '200000', 10);
const BATCH = 10000;            // docs per _bulk request
const CORPUS = '/tmp/xerj/corpus/chat-events.ndjson';

const sh = (cmd) => execFileSync('bash', ['-lc', cmd], { encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 });
const j = (s) => { try { return JSON.parse(s); } catch { return {}; } }
const ms = () => Number(process.hrtime.bigint() / 1000n) / 1000; // ms, sub-ms precision

async function req(method, p, body) {
  const t0 = process.hrtime.bigint();
  const r = await fetch(ES + p, body !== undefined
    ? { method, headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }
    : { method });
  const text = await r.text();
  const took = Number(process.hrtime.bigint() - t0) / 1e6;
  return { status: r.status, took, body: text };
}

function pct(arr, p) { const s = [...arr].sort((a, b) => a - b); return s[Math.min(s.length - 1, Math.floor(p / 100 * s.length))]; }
function statline(name, lat) {
  return { name, n: lat.length, mean: avg(lat), p50: pct(lat, 50), p95: pct(lat, 95), p99: pct(lat, 99), max: Math.max(...lat) };
}
const avg = (a) => a.reduce((x, y) => x + y, 0) / a.length;

async function bench(name, p, body, iters = 100, warmup = 10) {
  for (let i = 0; i < warmup; i++) await req('POST', p, body);
  const lat = [];
  for (let i = 0; i < iters; i++) { const r = await req('POST', p, body); lat.push(r.took); }
  return statline(name, lat);
}

async function main() {
  console.log(`[bench] target ${N} docs, batch ${BATCH}`);
  // ── 1. fresh perf index
  sh(`curl -s -XDELETE ${ES}/perf >/dev/null 2>&1; true`);
  await req('PUT', '/perf', {
    mappings: { properties: {
      model: { type: 'keyword' }, intent: { type: 'keyword' }, status: { type: 'keyword' },
      tenant: { type: 'keyword' }, top_doc: { type: 'keyword' }, cache_hit: { type: 'boolean' },
      prompt_tokens: { type: 'integer' }, context_tokens: { type: 'integer' },
      completion_tokens: { type: 'integer' }, latency_ms: { type: 'integer' },
      cost_usd: { type: 'float' }, '@timestamp': { type: 'date' },
    } },
  });

  // ── 2. build a dataset of N docs by cycling the real corpus, write bulk batches
  const corpus = fs.readFileSync(CORPUS, 'utf8').trim().split('\n');
  let written = 0, ingestMs = 0;
  while (written < N) {
    const take = Math.min(BATCH, N - written);
    let nd = '';
    for (let i = 0; i < take; i++) nd += '{"index":{}}\n' + corpus[(written + i) % corpus.length] + '\n';
    const f = `/tmp/xerj/_perfbatch.ndjson`;
    fs.writeFileSync(f, nd);
    const t0 = ms();
    sh(`curl -s -XPOST ${ES}/perf/_bulk -H 'content-type: application/x-ndjson' --data-binary @${f} >/dev/null`);
    ingestMs += ms() - t0;
    written += take;
    if (written % 50000 === 0 || written === N) console.log(`[bench]   ingested ${written}/${N}`);
  }
  await req('POST', '/perf/_refresh');
  const count = j((await req('GET', '/perf/_count')).body).count;
  const docsPerSec = Math.round(written / (ingestMs / 1000));

  // ── 3. read-path latency benchmarks
  const results = [];
  results.push(await bench('match_all (size 10)', '/perf/_search', { query: { match_all: {} }, size: 10 }));
  results.push(await bench('term filter', '/perf/_search', { query: { term: { model: 'claude-haiku-4-5' } }, size: 10 }));
  results.push(await bench('bool must+filter', '/perf/_search', { query: { bool: { must: [{ match: { status: 'ok' } }], filter: [{ range: { latency_ms: { gte: 200 } } }] } }, size: 10 }));
  results.push(await bench('range', '/perf/_search', { query: { range: { cost_usd: { gte: 0.01 } } }, size: 10 }));
  results.push(await bench('agg: terms(model)', '/perf/_search', { size: 0, aggs: { m: { terms: { field: 'model', size: 10 } } } }));
  results.push(await bench('agg: stats(latency_ms)', '/perf/_search', { size: 0, aggs: { s: { stats: { field: 'latency_ms' } } } }));
  results.push(await bench('agg: date_histogram', '/perf/_search', { size: 0, aggs: { t: { date_histogram: { field: '@timestamp', calendar_interval: 'day' } } } }));
  results.push(await bench('agg: terms+nested avg', '/perf/_search', { size: 0, aggs: { m: { terms: { field: 'model', size: 10 }, aggs: { c: { avg: { field: 'cost_usd' } } } } } }));
  results.push(await bench('cardinality', '/perf/_search', { size: 0, aggs: { u: { cardinality: { field: 'top_doc' } } } }));
  const countLat = []; for (let i = 0; i < 50; i++) countLat.push((await req('GET', '/perf/_count')).took);
  results.push(statline('_count', countLat));

  // ── 4. kNN vector latency (separate small index)
  let knn = null;
  sh(`curl -s -XDELETE ${ES}/perfvec >/dev/null 2>&1; true`);
  await req('PUT', '/perfvec', { mappings: { properties: { v: { type: 'dense_vector', dims: 16 } } } });
  { // 20k random-ish vectors derived deterministically
    let nd = '';
    for (let i = 0; i < 20000; i++) {
      const vec = Array.from({ length: 16 }, (_, k) => ((i * 7 + k * 13) % 100) / 100);
      nd += '{"index":{}}\n' + JSON.stringify({ v: vec }) + '\n';
      if (nd.length > 4e6) { fs.writeFileSync('/tmp/xerj/_vec.ndjson', nd); sh(`curl -s -XPOST ${ES}/perfvec/_bulk -H 'content-type: application/x-ndjson' --data-binary @/tmp/xerj/_vec.ndjson >/dev/null`); nd = ''; }
    }
    if (nd) { fs.writeFileSync('/tmp/xerj/_vec.ndjson', nd); sh(`curl -s -XPOST ${ES}/perfvec/_bulk -H 'content-type: application/x-ndjson' --data-binary @/tmp/xerj/_vec.ndjson >/dev/null`); }
    await req('POST', '/perfvec/_refresh');
    const qv = Array.from({ length: 16 }, (_, k) => (k * 13 % 100) / 100);
    knn = await bench('kNN k=10 (20k×16d)', '/perfvec/_search', { knn: { field: 'v', query_vector: qv, k: 10, num_candidates: 100 } });
  }

  // ── 5. report
  const f3 = (x) => x.toFixed(2);
  let md = `# Xerj hot-path benchmark\n\n`;
  md += `Single node, \`--insecure\`, release binary. Generated by \`demo/playbooks/bench.mjs\`.\n\n`;
  md += `## Bulk ingest\n\n`;
  md += `- Docs ingested: **${count.toLocaleString()}** (real LLM-telemetry corpus, ${BATCH.toLocaleString()}/batch)\n`;
  md += `- Wall time (bulk only): **${(ingestMs / 1000).toFixed(1)} s**\n`;
  md += `- Throughput: **${docsPerSec.toLocaleString()} docs/s**\n\n`;
  md += `## Read-path latency (ms) — ${results[0].n} iters after warmup, over ${count.toLocaleString()} docs\n\n`;
  md += `| operation | mean | p50 | p95 | p99 | max |\n|---|--:|--:|--:|--:|--:|\n`;
  for (const r of [...results, knn]) md += `| ${r.name} | ${f3(r.mean)} | ${f3(r.p50)} | ${f3(r.p95)} | ${f3(r.p99)} | ${f3(r.max)} |\n`;
  md += `\n_All latencies are end-to-end HTTP round-trip from localhost. kNN over a separate 20k×16-d HNSW index._\n`;
  fs.writeFileSync(path.join(ROOT, 'BENCHMARK.md'), md);
  console.log('\n' + md);
}
main().catch((e) => { console.error(e); process.exit(1); });
