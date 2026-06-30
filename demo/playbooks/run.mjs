#!/usr/bin/env node
// Xerj feature smoke-tests as reusable playbooks.
//
// Each playbook is a named use case (mapped to a docs page) made of steps.
// Every step is a REAL command run against a live xerj; we capture the exact
// command + its actual output, assert pass/fail, and emit three artifacts:
//
//   casts/<id>.md      human-readable cast ($ cmd → output) — for docs + screencasts
//   playbooks.json     structured steps+outputs — for AISE scene generation
//   SMOKE_REPORT.md    coverage + pass/fail summary
//
// Usage: node run.mjs            (expects xerj live on :9200 ES-compat + :8080 native)
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.dirname(new URL(import.meta.url).pathname);
const ES = 'http://localhost:9200';
const NATIVE = 'http://localhost:8080';
const CASTS = path.join(ROOT, 'casts');
fs.mkdirSync(CASTS, { recursive: true });

// ── one step: a real curl. `expect` may be {status} and/or {body:/re/}.
// label is the human caption; `cmd` is rendered + executed verbatim.
function curl({ method = 'GET', url, json, ndjson, headers = [] }) {
  const args = ['-s', '-S', '-m', '20', '-w', '\n__HTTP__%{http_code}', '-X', method];
  for (const h of headers) args.push('-H', h);
  let shown = `curl -s${method === 'GET' ? '' : 'X' + method === 'sXGET' ? '' : ''} `;
  if (json !== undefined) { args.push('-H', 'content-type: application/json', '--data', JSON.stringify(json)); }
  if (ndjson !== undefined) { args.push('-H', 'content-type: application/x-ndjson', '--data-binary', ndjson); }
  args.push(url);
  // Render a clean, copy-pasteable curl for the cast
  let render = `curl -s${method === 'GET' ? '' : ' -X' + method} "${url}"`;
  if (json !== undefined) render = `curl -s${method === 'GET' ? '' : ' -X' + method} "${url}" \\\n  -H 'content-type: application/json' \\\n  -d '${JSON.stringify(json)}'`;
  if (ndjson !== undefined) render = `curl -s -X${method} "${url}" \\\n  -H 'content-type: application/x-ndjson' \\\n  --data-binary $'${ndjson.replace(/\n/g, '\\n')}'`;
  let out = '';
  try { out = execFileSync('curl', args, { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 }); }
  catch (e) { out = (e.stdout || '') + (e.stderr || String(e.message)); }
  const m = out.lastIndexOf('\n__HTTP__');
  let status = 0, body = out;
  if (m >= 0) { status = parseInt(out.slice(m + 9).trim(), 10) || 0; body = out.slice(0, m); }
  // pretty-print JSON bodies for the cast; keep the raw body for assertions
  const raw = body.trim();
  let pretty = raw;
  try { pretty = JSON.stringify(JSON.parse(raw), null, 2); } catch {}
  return { render, status, raw, body: pretty };
}

function run(step) {
  const r = curl(step);
  const okStatus = step.expect?.status ? r.status === step.expect.status : (r.status >= 200 && r.status < 300);
  const okBody = step.expect?.body ? step.expect.body.test(r.raw) : true;
  const pass = okStatus && okBody;
  return { ...step, ...r, pass };
}

// Truncate long bodies for the cast (keep them readable)
const clip = (s, n = 1400) => (s.length > n ? s.slice(0, n) + `\n… (${s.length - n} more bytes)` : s);

// ───────────────────────────── PLAYBOOKS ─────────────────────────────
// data-driven; each step is {label, method, url, json|ndjson, expect?}
const TS = '2026-04-26T19:47:24Z';
const PB = [
  { id: '01-boot-health', doc: 'install / quickstart', title: 'Boot & health — one binary is up',
    intro: 'Prove the single binary is serving the ES-compatible API (:9200) and the native ops API (:8080).',
    steps: [
      { label: 'cluster identity (ES-wire)', url: `${ES}/` },
      { label: 'cluster health is green', url: `${ES}/_cluster/health`, expect: { body: /"status":"green"/ } },
      { label: 'nodes listing', url: `${ES}/_cat/nodes?v` },
      { label: 'native health (:8080)', url: `${NATIVE}/v1/health` },
      { label: 'prometheus metrics (:8080)', url: `${NATIVE}/v1/metrics` },
    ] },

  { id: '02-quickstart-crud', doc: 'quickstart', title: 'Quickstart — index, document, get, search',
    intro: 'Create an index with a mapping, write a document, read it back, and search it.',
    steps: [
      { label: 'create index w/ mapping', method: 'PUT', url: `${ES}/articles`,
        json: { mappings: { properties: { title: { type: 'text' }, body: { type: 'text' }, views: { type: 'integer' }, tag: { type: 'keyword' } } } }, expect: { body: /"acknowledged":true/ } },
      { label: 'index a document', method: 'PUT', url: `${ES}/articles/_doc/1`,
        json: { title: 'Hello Xerj', body: 'one binary for search', views: 42, tag: 'intro' }, expect: { body: /"result":"created"/ } },
      { label: 'get the document', url: `${ES}/articles/_doc/1`, expect: { body: /"found":true/ } },
      { label: 'search it', method: 'POST', url: `${ES}/articles/_search`, json: { query: { match: { body: 'binary' } } }, expect: { body: /"title":"Hello Xerj"/ } },
    ] },

  { id: '03-ingest-bulk', doc: 'ingest', title: 'Bulk ingest — ES /_bulk wire protocol',
    intro: 'Point any ES collector (Logstash/Filebeat/SDK) at Xerj — /_bulk works unchanged.',
    steps: [
      { label: 'bulk index 4 docs', method: 'POST', url: `${ES}/articles/_bulk`,
        ndjson: ['{"index":{}}', '{"title":"A","body":"alpha vector","views":10,"tag":"a"}', '{"index":{}}', '{"title":"B","body":"beta search","views":20,"tag":"b"}', '{"index":{}}', '{"title":"C","body":"gamma logs","views":30,"tag":"a"}', '{"index":{}}', '{"title":"D","body":"delta agg","views":40,"tag":"b"}', ''].join('\n'), expect: { body: /"errors":false/ } },
      { label: 'refresh to make searchable', method: 'POST', url: `${ES}/articles/_refresh` },
      { label: 'count documents', url: `${ES}/articles/_count`, expect: { body: /"count":5/ } },
      { label: 'cat indices', url: `${ES}/_cat/indices?v` },
    ] },

  { id: '04-full-text', doc: 'queries / playbooks/full-text', title: 'Full-text search — the ES query families',
    intro: 'The query DSL your Discover muscle memory already knows.',
    steps: [
      { label: 'match', method: 'POST', url: `${ES}/articles/_search`, json: { query: { match: { body: 'search' } } } },
      { label: 'match_phrase', method: 'POST', url: `${ES}/articles/_search`, json: { query: { match_phrase: { body: 'alpha vector' } } } },
      { label: 'term (keyword)', method: 'POST', url: `${ES}/articles/_search`, json: { query: { term: { tag: 'a' } } } },
      { label: 'terms', method: 'POST', url: `${ES}/articles/_search`, json: { query: { terms: { tag: ['a', 'b'] } } } },
      { label: 'range', method: 'POST', url: `${ES}/articles/_search`, json: { query: { range: { views: { gte: 20 } } } } },
      { label: 'bool must+filter', method: 'POST', url: `${ES}/articles/_search`, json: { query: { bool: { must: [{ match: { body: 'logs' } }], filter: [{ term: { tag: 'a' } }] } } } },
      { label: 'prefix', method: 'POST', url: `${ES}/articles/_search`, json: { query: { prefix: { body: 'alp' } } } },
      { label: 'wildcard', method: 'POST', url: `${ES}/articles/_search`, json: { query: { wildcard: { body: 'sea*' } } } },
      { label: 'exists', method: 'POST', url: `${ES}/articles/_search`, json: { query: { exists: { field: 'title' } } } },
      { label: 'highlight + sort + _source', method: 'POST', url: `${ES}/articles/_search`, json: { query: { match: { body: 'vector' } }, highlight: { fields: { body: {} } }, sort: [{ views: 'desc' }], _source: ['title', 'views'] } },
    ] },

  { id: '05-aggregations', doc: 'aggregations', title: 'Aggregations — the boardroom view',
    intro: 'Metric + bucket aggregations over the live index.',
    steps: [
      { label: 'terms', method: 'POST', url: `${ES}/articles/_search`, json: { size: 0, aggs: { by_tag: { terms: { field: 'tag' } } } } },
      { label: 'stats', method: 'POST', url: `${ES}/articles/_search`, json: { size: 0, aggs: { v: { stats: { field: 'views' } } } } },
      { label: 'sum / avg / max / min', method: 'POST', url: `${ES}/articles/_search`, json: { size: 0, aggs: { s: { sum: { field: 'views' } }, a: { avg: { field: 'views' } }, mx: { max: { field: 'views' } }, mn: { min: { field: 'views' } } } } },
      { label: 'cardinality', method: 'POST', url: `${ES}/articles/_search`, json: { size: 0, aggs: { uniq: { cardinality: { field: 'tag' } } } } },
      { label: 'value_count', method: 'POST', url: `${ES}/articles/_search`, json: { size: 0, aggs: { c: { value_count: { field: 'views' } } } } },
      { label: 'histogram', method: 'POST', url: `${ES}/articles/_search`, json: { size: 0, aggs: { h: { histogram: { field: 'views', interval: 10 } } } } },
      { label: 'range', method: 'POST', url: `${ES}/articles/_search`, json: { size: 0, aggs: { r: { range: { field: 'views', ranges: [{ to: 20 }, { from: 20 }] } } } } },
      { label: 'percentiles', method: 'POST', url: `${ES}/articles/_search`, json: { size: 0, aggs: { p: { percentiles: { field: 'views' } } } } },
      { label: 'terms + nested avg', method: 'POST', url: `${ES}/articles/_search`, json: { size: 0, aggs: { by_tag: { terms: { field: 'tag' }, aggs: { avg_views: { avg: { field: 'views' } } } } } } },
    ] },

  { id: '06-vector-search', doc: 'vectors / playbooks/vector-search', title: 'Vector search — kNN, no Pinecone',
    intro: 'dense_vector field + kNN retrieval are first-class in the same engine.',
    steps: [
      { label: 'create vector index', method: 'PUT', url: `${ES}/vstore`, json: { mappings: { properties: { text: { type: 'text' }, embedding: { type: 'dense_vector', dims: 4 } } } }, expect: { body: /"acknowledged":true/ } },
      { label: 'bulk index vectors', method: 'POST', url: `${ES}/vstore/_bulk`, ndjson: ['{"index":{}}', '{"text":"red apple","embedding":[0.9,0.1,0.0,0.1]}', '{"index":{}}', '{"text":"green pear","embedding":[0.1,0.9,0.1,0.0]}', '{"index":{}}', '{"text":"blue car","embedding":[0.0,0.1,0.9,0.1]}', ''].join('\n'), expect: { body: /"errors":false/ } },
      { label: 'refresh', method: 'POST', url: `${ES}/vstore/_refresh` },
      { label: 'kNN nearest to [0.9,0.1,0,0.1]', method: 'POST', url: `${ES}/vstore/_search`, json: { knn: { field: 'embedding', query_vector: [0.9, 0.1, 0.0, 0.1], k: 2, num_candidates: 10 } } },
    ] },

  { id: '07-analyzers', doc: 'analyzers', title: 'Analyzers — _analyze',
    intro: 'Inspect tokenization the way ES does.',
    steps: [
      { label: 'standard analyzer', method: 'POST', url: `${ES}/_analyze`, json: { analyzer: 'standard', text: 'The Quick, Brown Fox!' } },
      { label: 'keyword analyzer', method: 'POST', url: `${ES}/_analyze`, json: { analyzer: 'keyword', text: 'The Quick, Brown Fox!' } },
      { label: 'whitespace analyzer', method: 'POST', url: `${ES}/_analyze`, json: { analyzer: 'whitespace', text: 'The Quick, Brown Fox!' } },
    ] },

  { id: '08-mappings-templates-aliases', doc: 'config / operations', title: 'Mappings, templates & aliases',
    intro: 'Schema introspection + index templates + aliases — ES-identical.',
    steps: [
      { label: 'get mapping', url: `${ES}/articles/_mapping` },
      { label: 'put index template', method: 'PUT', url: `${ES}/_index_template/logs_tpl`, json: { index_patterns: ['logs-*'], template: { mappings: { properties: { '@timestamp': { type: 'date' }, level: { type: 'keyword' }, msg: { type: 'text' } } } } }, expect: { body: /"acknowledged":true/ } },
      { label: 'add alias', method: 'POST', url: `${ES}/_aliases`, json: { actions: [{ add: { index: 'articles', alias: 'articles_live' } }] }, expect: { body: /"acknowledged":true/ } },
      { label: 'search via alias', method: 'POST', url: `${ES}/articles_live/_search`, json: { size: 0, query: { match_all: {} } } },
    ] },

  { id: '09-log-analytics', doc: 'playbooks/log-analytics', title: 'Log analytics — Splunk/Kibana workload',
    intro: 'Template-driven log index, time-series + level breakdown aggregations.',
    steps: [
      { label: 'bulk ingest logs (template applies)', method: 'POST', url: `${ES}/logs-2026-04/_bulk`, ndjson: [
        '{"index":{}}', `{"@timestamp":"${TS}","level":"info","msg":"request ok","svc":"api"}`,
        '{"index":{}}', `{"@timestamp":"${TS}","level":"error","msg":"timeout","svc":"api"}`,
        '{"index":{}}', `{"@timestamp":"${TS}","level":"error","msg":"5xx upstream","svc":"web"}`,
        '{"index":{}}', `{"@timestamp":"${TS}","level":"warn","msg":"slow query","svc":"db"}`, ''].join('\n'), expect: { body: /"errors":false/ } },
      { label: 'refresh', method: 'POST', url: `${ES}/logs-2026-04/_refresh` },
      { label: 'errors only (bool filter)', method: 'POST', url: `${ES}/logs-2026-04/_search`, json: { query: { term: { level: 'error' } } } },
      { label: 'count by level', method: 'POST', url: `${ES}/logs-2026-04/_search`, json: { size: 0, aggs: { by_level: { terms: { field: 'level' } } } } },
      { label: 'errors over time (date_histogram)', method: 'POST', url: `${ES}/logs-2026-04/_search`, json: { size: 0, query: { term: { level: 'error' } }, aggs: { t: { date_histogram: { field: '@timestamp', calendar_interval: 'day' } } } } },
    ] },

  { id: '10-migration-es', doc: 'migration-from-es', title: 'Migration from Elasticsearch — drop-in parity',
    intro: 'The exact ES request bodies return ES-shaped responses — clients, Logstash, Kibana keep working.',
    steps: [
      { label: 'ES version handshake (Kibana checks this)', url: `${ES}/`, expect: { body: /"version"/ } },
      { label: '_cat/indices (ops tooling)', url: `${ES}/_cat/indices?v` },
      { label: 'scroll API (pagination)', method: 'POST', url: `${ES}/articles/_search?scroll=1m`, json: { size: 2, query: { match_all: {} } }, expect: { body: /_scroll_id/ } },
      { label: '_delete_by_query', method: 'POST', url: `${ES}/articles/_delete_by_query`, json: { query: { term: { tag: 'b' } } }, expect: { body: /"deleted"/ } },
    ] },

  { id: '11-native-ops', doc: 'metrics / operations / api-native', title: 'Native ops API (:8080) — health & metrics',
    intro: 'The native v1 surface for ops. NOTE: this binary implements health + metrics; the rest of the documented /v1 API is not yet live (captured as gaps).',
    steps: [
      { label: 'native health', url: `${NATIVE}/v1/health`, expect: { body: /"status"/ } },
      { label: 'prometheus scrape', url: `${NATIVE}/v1/metrics`, expect: { body: /xerj_/ } },
      { label: 'DOC GAP: /v1/health/ready', url: `${NATIVE}/v1/health/ready`, expect: { status: 200 } },
      { label: 'DOC GAP: /v1/cluster/health', url: `${NATIVE}/v1/cluster/health`, expect: { status: 200 } },
      { label: 'DOC GAP: /v1/admin/flush', method: 'POST', url: `${NATIVE}/v1/admin/flush`, expect: { status: 200 } },
      { label: 'DOC GAP: /v1/admin/backup', method: 'POST', url: `${NATIVE}/v1/admin/backup`, json: {}, expect: { status: 200 } },
    ] },

  { id: '12-console-ux', doc: 'Xerj Console (xerj-ux)', title: 'Embedded Console UX',
    intro: 'The console SPA ships inside the binary and is served at /_xerj-console/.',
    steps: [
      { label: 'console SPA shell', url: `${ES}/_xerj-console/`, expect: { body: /<!doctype html>/i } },
      { label: 'console app bundle', url: `${ES}/_xerj-console/src/app.js`, expect: { body: /./ } },
      { label: 'first-launch setup page', url: `${ES}/_xerj-console/setup` },
    ] },
];

// ───────────────────────────── EXECUTE ─────────────────────────────
const results = [];
for (const pb of PB) {
  const steps = pb.steps.map(run);
  results.push({ ...pb, steps });
  const ok = steps.filter((s) => s.pass).length;
  console.log(`${ok === steps.length ? '✅' : '⚠️ '} ${pb.id}  ${ok}/${steps.length}  ${pb.title}`);

  // per-playbook cast (docs + screencast ready)
  let md = `# ${pb.title}\n\n_Use case doc: ${pb.doc}_\n\n${pb.intro}\n\n`;
  for (const s of steps) {
    md += `### ${s.pass ? '✅' : '❌'} ${s.label}\n\n\`\`\`bash\n${s.render}\n\`\`\`\n\n`;
    md += `\`\`\`json\n${clip(s.body)}\n\`\`\`\n\n_HTTP ${s.status}_\n\n`;
  }
  fs.writeFileSync(path.join(CASTS, `${pb.id}.md`), md);
}

// machine-readable (for AISE scene generation)
fs.writeFileSync(path.join(ROOT, 'playbooks.json'), JSON.stringify(
  results.map((p) => ({ id: p.id, doc: p.doc, title: p.title, steps: p.steps.map((s) => ({ label: s.label, cmd: s.render, status: s.status, pass: s.pass, output: s.body })) })), null, 2));

// summary report
const total = results.reduce((a, p) => a + p.steps.length, 0);
const passed = results.reduce((a, p) => a + p.steps.filter((s) => s.pass).length, 0);
let rep = `# Xerj feature smoke-tests — coverage & results\n\n`;
rep += `Generated by \`demo/playbooks/run.mjs\` against a live xerj (ES-compat :9200 + native :8080).\n\n`;
rep += `**${passed}/${total} steps passing across ${results.length} use cases.**\n\n`;
rep += `| # | use case | doc | steps | pass | cast |\n|---|---|---|---|---|---|\n`;
for (const p of results) {
  const ok = p.steps.filter((s) => s.pass).length;
  rep += `| ${p.id.split('-')[0]} | ${p.title} | ${p.doc} | ${p.steps.length} | ${ok === p.steps.length ? `✅ ${ok}/${p.steps.length}` : `⚠️ ${ok}/${p.steps.length}`} | [cast](casts/${p.id}.md) |\n`;
}
rep += `\n## Failures / documented-but-unimplemented\n\n`;
for (const p of results) for (const s of p.steps) if (!s.pass) rep += `- **${p.id}** · ${s.label} → HTTP ${s.status}\n`;
rep += `\n_Each cast under \`casts/\` shows the exact \`curl\` + real response — copy into docs or drive an AISE screencast scene. Structured form in \`playbooks.json\`._\n`;
fs.writeFileSync(path.join(ROOT, 'SMOKE_REPORT.md'), rep);

console.log(`\n${passed}/${total} steps passing → demo/playbooks/SMOKE_REPORT.md (+ casts/, playbooks.json)`);
