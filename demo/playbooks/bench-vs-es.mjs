#!/usr/bin/env node
// XERJ vs Elasticsearch head-to-head benchmark.
//
// Runs IDENTICAL workloads against two engines on localhost and reports
// end-to-end (client wall-clock) latency side by side, plus bulk ingest
// throughput. Both engines run --insecure / security-off, single node, same
// machine, same corpus — so the only variable is the engine.
//
// Usage:
//   node bench-vs-es.mjs [N_DOCS] [XERJ_URL] [ES_URL]
//   node bench-vs-es.mjs 100000 http://localhost:9200 http://localhost:9201
import fs from 'node:fs';
import { execSync } from 'node:child_process';

const N = parseInt(process.argv[2] || '100000', 10);
const ENGINES = [
  { name: 'XERJ', url: process.argv[3] || 'http://localhost:9200' },
  { name: 'Elasticsearch', url: process.argv[4] || 'http://localhost:9201' },
];
const BATCH = 10000;
const CORPUS = '/home/claude/ai/xerj/demo/data/extras/chat-events.ndjson';
const SCRATCH = '/tmp/xerj';

const sh = (c) => execSync(c, { stdio: ['ignore', 'pipe', 'ignore'] }).toString();
async function req(base, method, p, body, ndjson = false) {
  const headers = { 'content-type': ndjson ? 'application/x-ndjson' : 'application/json' };
  const opts = { method, headers };
  if (body !== undefined) opts.body = ndjson ? body : JSON.stringify(body);
  const r = await fetch(base + p, opts);
  const txt = await r.text();
  let j = null; try { j = JSON.parse(txt); } catch {}
  return { status: r.status, j, txt };
}
function pct(a, p) { const s = [...a].sort((x, y) => x - y); return s[Math.min(s.length - 1, Math.floor(p / 100 * s.length))]; }
function f3(x) { return x === undefined || x === null ? '—' : x.toFixed(2); }

// Time `iters` end-to-end requests (wall clock) after `warmup`.
async function timed(base, p, body, iters = 80, warmup = 15) {
  for (let i = 0; i < warmup; i++) await req(base, 'POST', p, body);
  const lat = [];
  for (let i = 0; i < iters; i++) {
    const t0 = performance.now();
    await req(base, 'POST', p, body);
    lat.push(performance.now() - t0);
  }
  return { mean: lat.reduce((a, b) => a + b, 0) / lat.length, p50: pct(lat, 50), p95: pct(lat, 95), p99: pct(lat, 99), n: iters };
}

const READS = [
  ['match_all (size 10)', { query: { match_all: {} }, size: 10 }],
  ['term filter', { query: { term: { model: 'claude-haiku-4-5' } }, size: 10 }],
  ['bool must+filter', { query: { bool: { must: [{ match: { status: 'ok' } }], filter: [{ range: { latency_ms: { gte: 200 } } }] } }, size: 10 }],
  ['range', { query: { range: { cost_usd: { gte: 0.01 } } }, size: 10 }],
  ['agg: terms(model)', { size: 0, aggs: { m: { terms: { field: 'model', size: 10 } } } }],
  ['agg: stats(latency_ms)', { size: 0, aggs: { s: { stats: { field: 'latency_ms' } } } }],
  ['agg: date_histogram(day)', { size: 0, aggs: { t: { date_histogram: { field: '@timestamp', calendar_interval: 'day' } } } }],
  ['agg: terms+avg(cost)', { size: 0, aggs: { m: { terms: { field: 'model', size: 10 }, aggs: { c: { avg: { field: 'cost_usd' } } } } } }],
  ['agg: cardinality(top_doc)', { size: 0, aggs: { u: { cardinality: { field: 'top_doc' } } } }],
  ['_count match_all', null], // handled specially via _count
];

const MAPPING = {
  mappings: { properties: {
    '@timestamp': { type: 'date' },
    model: { type: 'keyword' }, intent: { type: 'keyword' }, status: { type: 'keyword' },
    tenant: { type: 'keyword' }, top_doc: { type: 'keyword' }, cache_hit: { type: 'boolean' },
    prompt_tokens: { type: 'integer' }, context_tokens: { type: 'integer' }, completion_tokens: { type: 'integer' },
    latency_ms: { type: 'integer' }, cost_usd: { type: 'double' },
  } },
};

async function ingest(base) {
  await req(base, 'DELETE', '/perf');
  await req(base, 'PUT', '/perf', MAPPING);
  const raw = fs.readFileSync(CORPUS, 'utf8').trim().split('\n');
  let written = 0; const t0 = performance.now();
  while (written < N) {
    let nd = '';
    for (let i = 0; i < BATCH && written < N; i++, written++) nd += '{"index":{}}\n' + raw[written % raw.length] + '\n';
    const f = `${SCRATCH}/_perf_batch.ndjson`; fs.writeFileSync(f, nd);
    sh(`curl -s -XPOST ${base}/perf/_bulk -H 'content-type: application/x-ndjson' --data-binary @${f} >/dev/null`);
  }
  const ms = performance.now() - t0;
  await req(base, 'POST', '/perf/_refresh');
  const c = await req(base, 'GET', '/perf/_count');
  return { ms, docs: c.j?.count ?? written, dps: Math.round(written / (ms / 1000)) };
}

async function ingestVec(base) {
  await req(base, 'DELETE', '/perfvec');
  await req(base, 'PUT', '/perfvec', { mappings: { properties: { v: { type: 'dense_vector', dims: 16, index: true, similarity: 'cosine' } } } });
  const D = 16, M = 20000; let nd = '';
  for (let i = 0; i < M; i++) {
    const v = Array.from({ length: D }, (_, k) => Math.sin(i * 0.13 + k));
    nd += '{"index":{}}\n' + JSON.stringify({ v }) + '\n';
    if (nd.length > 4e6) { const f = `${SCRATCH}/_vec.ndjson`; fs.writeFileSync(f, nd); sh(`curl -s -XPOST ${base}/perfvec/_bulk -H 'content-type: application/x-ndjson' --data-binary @${f} >/dev/null`); nd = ''; }
  }
  if (nd) { const f = `${SCRATCH}/_vec.ndjson`; fs.writeFileSync(f, nd); sh(`curl -s -XPOST ${base}/perfvec/_bulk -H 'content-type: application/x-ndjson' --data-binary @${f} >/dev/null`); }
  await req(base, 'POST', '/perfvec/_refresh');
  return Array.from({ length: D }, (_, k) => Math.sin(0.07 + k));
}

async function main() {
  const out = {};
  for (const e of ENGINES) {
    const v = await req(e.url, 'GET', '/');
    process.stderr.write(`\n== ${e.name} (${e.url}) — version ${v.j?.version?.number || '?'} ==\n`);
    process.stderr.write(`   ingesting ${N.toLocaleString()} docs...\n`);
    const ing = await ingest(e.url);
    process.stderr.write(`   ingest: ${ing.docs.toLocaleString()} docs in ${(ing.ms / 1000).toFixed(1)}s = ${ing.dps.toLocaleString()} docs/s\n`);
    const reads = {};
    for (const [name, body] of READS) {
      if (body === null) { reads[name] = await timed(e.url, '/perf/_count', { query: { match_all: {} } }); }
      else { reads[name] = await timed(e.url, '/perf/_search', body); }
      process.stderr.write(`   ${name}: p50 ${f3(reads[name].p50)}ms\n`);
    }
    const qv = await ingestVec(e.url);
    const knn = await timed(e.url, '/perfvec/_search', { knn: { field: 'v', query_vector: qv, k: 10, num_candidates: 100 }, size: 10 });
    process.stderr.write(`   kNN k=10: p50 ${f3(knn.p50)}ms\n`);
    out[e.name] = { ing, reads, knn };
  }

  // ── Report ──
  const [A, B] = ENGINES.map((e) => e.name);
  let md = `# XERJ vs Elasticsearch — head-to-head benchmark\n\n`;
  md += `Identical workload, same machine (localhost), single node, security off. `;
  md += `End-to-end client wall-clock latency. Corpus: real LLM-telemetry, **${out[A].ing.docs.toLocaleString()} docs**.\n\n`;
  md += `## Ingest (bulk, ${BATCH.toLocaleString()}/batch)\n\n`;
  md += `| Engine | docs | wall | throughput |\n|---|--:|--:|--:|\n`;
  for (const e of ENGINES) md += `| ${e.name} | ${out[e.name].ing.docs.toLocaleString()} | ${(out[e.name].ing.ms / 1000).toFixed(1)}s | **${out[e.name].ing.dps.toLocaleString()} docs/s** |\n`;
  const speedup = (out[A].ing.dps / out[B].ing.dps);
  md += `\n_${A} ingest is **${speedup.toFixed(2)}×** ${speedup >= 1 ? 'faster' : 'slower'} than ${B}._\n\n`;
  md += `## Read-path latency (ms, end-to-end, ${out[A].reads['range'].n} iters after warmup)\n\n`;
  md += `| operation | ${A} p50 | ${B} p50 | ${A} p95 | ${B} p95 | ${A} p99 | ${B} p99 | p50 ratio |\n|---|--:|--:|--:|--:|--:|--:|--:|\n`;
  const rows = [...READS.map(([n]) => n), 'kNN k=10 (20k×16d)'];
  for (const name of rows) {
    const ra = name.startsWith('kNN') ? out[A].knn : out[A].reads[name];
    const rb = name.startsWith('kNN') ? out[B].knn : out[B].reads[name];
    const ratio = ra && rb && rb.p50 ? (rb.p50 / ra.p50) : null;
    md += `| ${name} | ${f3(ra?.p50)} | ${f3(rb?.p50)} | ${f3(ra?.p95)} | ${f3(rb?.p95)} | ${f3(ra?.p99)} | ${f3(rb?.p99)} | ${ratio ? ratio.toFixed(2) + '×' : '—'} |\n`;
  }
  md += `\n_p50 ratio = ${B} p50 ÷ ${A} p50; >1 means ${A} is faster._\n`;
  fs.writeFileSync('/home/claude/ai/xerj/demo/playbooks/BENCHMARK_VS_ES.md', md);
  process.stderr.write('\nwrote demo/playbooks/BENCHMARK_VS_ES.md\n');
  console.log(md);
}
main().catch((e) => { console.error(e); process.exit(1); });
