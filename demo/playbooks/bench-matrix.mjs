#!/usr/bin/env node
// XERJ vs Elasticsearch — BENCHMARK MATRIX runner.
//
// One harness that sweeps a matrix of {docs} × {concurrent clients} for bulk
// ingest, then measures the full read surface (every query + agg + pipeline
// family that runs on the flat LLM-telemetry corpus), plus optional mixed
// read-under-write, kNN latency+recall, and on-disk index size — against TWO
// engines running the IDENTICAL workload (XERJ :9200, Elasticsearch :9201,
// both --insecure / security-off, single node, same machine, same corpus).
//
// Emits demo/playbooks/SCORECARD.md: one row per dimension with XERJ value,
// ES value, ratio, and a plain-ASCII WIN/LOSE verdict. Any LOSE => exit 1, so
// the whole thing can gate CI.
//
// Helpers/style are lifted from demo/playbooks/bench-vs-es.mjs (canonical);
// `statline`-style max is folded in from demo/playbooks/bench.mjs.
//
// Usage:
//   node demo/playbooks/bench-matrix.mjs
//   node demo/playbooks/bench-matrix.mjs --docs 100k,1m --clients 1,8 --knn --mixed
//   node demo/playbooks/bench-matrix.mjs --xerj http://localhost:9200 --es http://localhost:9201
//   node demo/playbooks/bench-matrix.mjs --reads-only
//   node demo/playbooks/bench-matrix.mjs --ingest-only --docs 1m --clients 1,4,8
//
// Requires: Node 24, global fetch, curl on PATH. No deps beyond node builtins.
import fs from 'node:fs';
import { exec, spawn } from 'node:child_process';
import { promisify } from 'node:util';

const execP = promisify(exec);

// ── constants (authoritative paths from the scout spec) ──
const BATCH = 10000;
const CORPUS = '/home/claude/ai/xerj/demo/data/extras/chat-events.ndjson';
const SCRATCH = '/tmp/xerj';
const DEFAULT_OUT = '/home/claude/ai/xerj/demo/playbooks/SCORECARD.md';
const HEADTOHEAD_OUT = '/home/claude/ai/xerj/demo/playbooks/BENCHMARK_VS_ES.md';

// ────────────────────────────── helpers (lifted from bench-vs-es.mjs) ──────────────────────────────
async function req(base, method, p, body, ndjson = false) {
  const headers = { 'content-type': ndjson ? 'application/x-ndjson' : 'application/json' };
  const opts = { method, headers };
  // Apples-to-apples total-hits: ES caps hits.total.value at 10,000 by default
  // (track_total_hits=10000, relation "gte") while XERJ returns the exact total.
  // Force BOTH engines to compute the true total so (a) hit counts are
  // comparable for the correctness/no-op guard and (b) neither engine gets a
  // latency edge from short-circuiting the count. Only for JSON _search bodies:
  // this covers every READ_FAMILIES / MIXED_OPS / kNN body (all POST .../_search
  // JSON objects); _count is exact by definition and _mget has no hit total.
  // _msearch NDJSON bodies are DELIBERATELY not patched (the injection is
  // JSON-object-only), so its per-line totals keep engine-default semantics.
  let sendBody = body;
  if (!ndjson && body && typeof body === 'object' && !Array.isArray(body)
      && /\/_search(\?|$)/.test(p) && body.track_total_hits === undefined) {
    sendBody = { ...body, track_total_hits: true };
  }
  if (sendBody !== undefined) opts.body = ndjson ? sendBody : JSON.stringify(sendBody);
  // Bound each request so an engine that collapses under load surfaces as a
  // timeout (recorded, scored) instead of hanging the whole matrix.
  const ac = new AbortController();
  const to = setTimeout(() => ac.abort(), 15000);
  opts.signal = ac.signal;
  let r, txt = '', j = null, failed = false;
  try {
    r = await fetch(base + p, opts);
    txt = await r.text();
    try { j = JSON.parse(txt); } catch {}
  } catch (e) {
    failed = true; // timeout / connection reset — engine unresponsive under load
  } finally { clearTimeout(to); }
  if (failed) return { status: 0, j: null, txt: '', failed: true };
  return { status: r.status, j, txt };
}

// percentile: p in [0,100], nearest-rank on a sorted copy.
function pct(a, p) { if (!a || !a.length) return null; const s = [...a].sort((x, y) => x - y); const idx = Math.min(s.length - 1, Math.max(0, Math.ceil(p / 100 * s.length) - 1)); return s[idx]; }

// safe 2-decimal formatter, '—' for null/undefined.
function f3(x) { return x === undefined || x === null ? '—' : x.toFixed(2); }

// Extract a correctness signal from a search/count/agg response so a query that
// silently no-ops (HTTP 200 but 0 hits / empty aggregation) can be detected
// instead of being scored as a fast latency WIN. Returns {hits, agg} where either
// may be null when not applicable.
function readSignal(j) {
  if (!j || typeof j !== 'object') return null;
  // _count endpoint: {count: N}
  if (typeof j.count === 'number' && !j.hits && !j.aggregations) return { hits: j.count, agg: null };
  const ht = j.hits && j.hits.total;
  const hits = typeof ht === 'number' ? ht : (ht && typeof ht.value === 'number' ? ht.value : null);
  let agg = null;
  if (j.aggregations) {
    const first = Object.values(j.aggregations)[0];
    if (first && typeof first === 'object') {
      if (Array.isArray(first.buckets)) agg = first.buckets.length;
      else if (typeof first.value === 'number') agg = first.value;
      else if (typeof first.count === 'number') agg = first.count;
      else if (typeof first.doc_count === 'number') agg = first.doc_count;
    }
  }
  return { hits, agg };
}
// Compact printable form of a correctness signal for logs.
function sigStr(s) {
  if (!s) return '—';
  if (s.hits != null) return String(s.hits);
  if (s.agg != null) return 'agg=' + s.agg;
  return '—';
}
// Returns a mismatch reason string when the two engines' results materially
// diverge (one empty while the other is non-empty, or hit counts differ beyond a
// small tolerance), else null. Used to mark a read family N/A instead of awarding
// a latency WIN to whichever engine short-circuited the query.
function signalMismatch(xs, es) {
  if (!xs || !es) return null; // not comparable (e.g. _msearch/_mget) — leave to normal scoring
  const xh = xs.hits, eh = es.hits;
  if (xh != null && eh != null) {
    if ((xh === 0) !== (eh === 0)) return `hits ${xh} vs ${eh}`;
    const tol = Math.max(1, 0.1 * Math.max(xh, eh));
    if (Math.abs(xh - eh) > tol) return `hits ${xh} vs ${eh}`;
  }
  const xa = xs.agg, ea = es.agg;
  if (xa != null && ea != null) {
    if ((xa === 0) !== (ea === 0)) return `agg ${xa} vs ${ea}`;
  }
  return null;
}

// End-to-end latency of `iters` requests after `warmup` untimed calls, driven
// OPEN-LOOP to avoid coordinated omission. Extended from bench-vs-es.mjs's
// `timed`: also captures `max` (from bench.mjs's `statline`), takes a
// `method`/`ndjson` opts pair so GET/_msearch/_mget/_count all route through one
// path, returns {unsupported:true} on a 4xx feasibility probe, and captures a
// correctness `signal` from the probe response so no-op queries are visible.
//
// Coordinated-omission correction: request start times are scheduled on a FIXED
// cadence (t0 + i/rate) independent of when prior responses return; requests may
// be concurrently in flight; latency is measured from the INTENDED start time, so
// a server stall (GC/flush/merge) inflates the tail of the backlog instead of
// silently shifting the schedule and omitting the would-be-slow samples. Also
// reports offered vs achieved rate so the load level is visible.
async function timed(base, p, body, opts = {}) {
  const { iters = 120, warmup = 15, method = 'POST', ndjson = false, rate = 200 } = opts;
  const chk = await req(base, method, p, body, ndjson);
  // Collapse (timeout / connection reset under load) is a FAILURE to serve, not
  // a missing feature: score it as the worst possible latency (LOSE) rather than
  // 'unsupported' (which would N/A the row and hide the regression).
  if (chk.failed) return { collapsed: true, p50: Infinity, p95: Infinity, p99: Infinity, max: Infinity, mean: Infinity, n: 0 };
  if (chk.status >= 400) return { unsupported: true, status: chk.status };
  const signal = readSignal(chk.j);
  for (let i = 0; i < warmup; i++) await req(base, method, p, body, ndjson);
  const lat = new Array(iters);
  const tasks = new Array(iters);
  const t0 = performance.now();
  for (let i = 0; i < iters; i++) {
    const intended = t0 + (i / rate) * 1000; // fixed cadence, independent of prior responses
    tasks[i] = (async () => {
      const wait = intended - performance.now();
      if (wait > 0) await new Promise((r) => setTimeout(r, wait));
      await req(base, method, p, body, ndjson);
      lat[i] = performance.now() - intended; // CO-corrected: measured from intended start
    })();
  }
  await Promise.all(tasks);
  const wallMs = performance.now() - t0;
  return {
    mean: lat.reduce((a, b) => a + b, 0) / lat.length,
    p50: pct(lat, 50), p95: pct(lat, 95), p99: pct(lat, 99),
    max: Math.max(...lat), n: iters,
    offeredRate: rate, achievedRate: Math.round(iters / (wallMs / 1000)),
    signal,
  };
}
async function safeTimed(base, p, body, opts) {
  try { return await timed(base, p, body, opts); }
  catch (e) { return { error: String(e && e.message || e) }; }
}
// Quick liveness probe (GET /) used by phase-failure diagnostics so a dead
// engine (crashed between phases) is distinguishable from a missing feature.
async function engineAlive(base) {
  const r = await req(base, 'GET', '/');
  return r.failed ? 'DEAD (no response to GET /)' : `alive (GET / -> HTTP ${r.status})`;
}
// true stat value (p50 or recall etc.) or null if unsupported/errored.
function statVal(r, key = 'p50') {
  return r && !r.unsupported && !r.error && typeof r[key] === 'number' ? r[key] : null;
}

// ────────────────────────────── mapping (verbatim from bench-vs-es.mjs) ──────────────────────────────
const MAPPING = {
  mappings: { properties: {
    '@timestamp': { type: 'date' },
    model: { type: 'keyword' }, intent: { type: 'keyword' }, status: { type: 'keyword' },
    tenant: { type: 'keyword' }, top_doc: { type: 'keyword' }, cache_hit: { type: 'boolean' },
    prompt_tokens: { type: 'integer' }, context_tokens: { type: 'integer' }, completion_tokens: { type: 'integer' },
    latency_ms: { type: 'integer' }, cost_usd: { type: 'double' },
  } },
};

// ────────────────────────────── read families (corpus-runnable subset) ──────────────────────────────
// Every family the scouts enumerated that runs on the flat LLM-telemetry corpus.
// entry: { label, body, path?, method?, ndjson? }. Anything needing a geo_point,
// ip, nested/join, span, or dense_vector field is in SKIPPED (below) with a reason.
const S = '/perf/_search';
const READ_FAMILIES = [
  // §1 full-text (on keyword these behave as exact-token matches)
  { label: 'q: match_all', body: { query: { match_all: {} }, size: 10 } },
  { label: 'q: match_none', body: { query: { match_none: {} }, size: 10 } },
  { label: 'q: match(model)', body: { query: { match: { model: 'claude-haiku-4-5' } }, size: 10 } },
  { label: 'q: match_phrase(top_doc)', body: { query: { match_phrase: { top_doc: 'runbook/oncall.md' } }, size: 10 } },
  { label: 'q: match_phrase_prefix', body: { query: { match_phrase_prefix: { top_doc: 'runbook' } }, size: 10 } },
  { label: 'q: match_bool_prefix', body: { query: { match_bool_prefix: { top_doc: 'runbook' } }, size: 10 } },
  { label: 'q: multi_match', body: { query: { multi_match: { query: 'claude-haiku-4-5', fields: ['model', 'top_doc'] } }, size: 10 } },
  { label: 'q: combined_fields', body: { query: { combined_fields: { query: 'claude-haiku-4-5', fields: ['model', 'top_doc'] } }, size: 10 } },
  { label: 'q: query_string', body: { query: { query_string: { query: 'model:claude-haiku-4-5 AND status:ok' } }, size: 10 } },
  { label: 'q: simple_query_string', body: { query: { simple_query_string: { query: 'ok', fields: ['status'] } }, size: 10 } },
  { label: 'q: more_like_this', body: { query: { more_like_this: { fields: ['top_doc'], like: 'runbook/oncall.md', min_term_freq: 1, min_doc_freq: 1 } }, size: 10 } },
  // §2 term-level
  { label: 'q: term(status)', body: { query: { term: { status: 'ok' } }, size: 10 } },
  { label: 'q: terms(model)', body: { query: { terms: { model: ['claude-haiku-4-5', 'claude-sonnet-4-5'] } }, size: 10 } },
  { label: 'q: range(latency_ms)', body: { query: { range: { latency_ms: { gte: 100, lt: 500 } } }, size: 10 } },
  { label: 'q: range(@timestamp)', body: { query: { range: { '@timestamp': { gte: '2026-01-01', lte: '2026-12-31' } } }, size: 10 } },
  { label: 'q: range(cost_usd)', body: { query: { range: { cost_usd: { gte: 0.01 } } }, size: 10 } },
  { label: 'q: prefix(model)', body: { query: { prefix: { model: 'claude-' } }, size: 10 } },
  { label: 'q: wildcard(model)', body: { query: { wildcard: { model: 'claude-*' } }, size: 10 } },
  { label: 'q: regexp(model)', body: { query: { regexp: { model: 'claude-[a-z]+.*' } }, size: 10 } },
  { label: 'q: fuzzy(model)', body: { query: { fuzzy: { model: { value: 'claude-haiku-4-6', fuzziness: 1 } } }, size: 10 } },
  { label: 'q: exists(cost_usd)', body: { query: { exists: { field: 'cost_usd' } }, size: 10 } },
  { label: 'q: ids', body: { query: { ids: { values: ['1', '2', '3'] } }, size: 10 } },
  { label: 'q: term(cache_hit)', body: { query: { term: { cache_hit: true } }, size: 10 } },
  // §3 compound
  { label: 'q: bool must+filter+should+must_not', body: { query: { bool: { must: [{ match: { status: 'ok' } }], filter: [{ range: { latency_ms: { lte: 1000 } } }], should: [{ term: { model: 'claude-haiku-4-5' } }], must_not: [{ term: { cache_hit: true } }] } }, size: 10 } },
  { label: 'q: constant_score', body: { query: { constant_score: { filter: { term: { status: 'ok' } }, boost: 1.2 } }, size: 10 } },
  { label: 'q: boosting', body: { query: { boosting: { positive: { term: { status: 'ok' } }, negative: { term: { cache_hit: true } }, negative_boost: 0.3 } }, size: 10 } },
  { label: 'q: dis_max', body: { query: { dis_max: { queries: [{ term: { model: 'claude-haiku-4-5' } }, { term: { model: 'claude-sonnet-4-5' } }], tie_breaker: 0.3 } }, size: 10 } },
  { label: 'q: function_score', body: { query: { function_score: { query: { match_all: {} }, field_value_factor: { field: 'cost_usd', factor: 2.0, modifier: 'log1p' } } }, size: 10 } },
  { label: 'q: pinned', body: { query: { pinned: { ids: ['1', '2'], organic: { term: { status: 'ok' } } } }, size: 10 } },
  // §6a metric aggs
  { label: 'agg: avg', body: { size: 0, aggs: { a: { avg: { field: 'latency_ms' } } } } },
  { label: 'agg: sum', body: { size: 0, aggs: { a: { sum: { field: 'cost_usd' } } } } },
  { label: 'agg: min', body: { size: 0, aggs: { a: { min: { field: 'latency_ms' } } } } },
  { label: 'agg: max', body: { size: 0, aggs: { a: { max: { field: 'latency_ms' } } } } },
  { label: 'agg: stats', body: { size: 0, aggs: { a: { stats: { field: 'latency_ms' } } } } },
  { label: 'agg: extended_stats', body: { size: 0, aggs: { a: { extended_stats: { field: 'latency_ms' } } } } },
  { label: 'agg: value_count', body: { size: 0, aggs: { a: { value_count: { field: 'model' } } } } },
  { label: 'agg: cardinality', body: { size: 0, aggs: { a: { cardinality: { field: 'top_doc' } } } } },
  { label: 'agg: percentiles', body: { size: 0, aggs: { a: { percentiles: { field: 'latency_ms', percents: [50, 95, 99] } } } } },
  { label: 'agg: percentile_ranks', body: { size: 0, aggs: { a: { percentile_ranks: { field: 'latency_ms', values: [200, 500] } } } } },
  { label: 'agg: median_absolute_deviation', body: { size: 0, aggs: { a: { median_absolute_deviation: { field: 'latency_ms' } } } } },
  { label: 'agg: matrix_stats', body: { size: 0, aggs: { a: { matrix_stats: { fields: ['latency_ms', 'cost_usd'] } } } } },
  { label: 'agg: scripted_metric', body: { size: 0, aggs: { a: { scripted_metric: { init_script: 'state.s=0', map_script: 'state.s+=doc.latency_ms.value', combine_script: 'return state.s', reduce_script: 'double t=0;for(s in states)t+=s;return t' } } } } },
  { label: 'agg: top_hits (sub)', body: { size: 0, aggs: { by_model: { terms: { field: 'model' }, aggs: { top: { top_hits: { size: 1, sort: [{ latency_ms: 'desc' }] } } } } } } },
  // §6b bucket aggs
  { label: 'agg: terms', body: { size: 0, aggs: { a: { terms: { field: 'model', size: 10 } } } } },
  { label: 'agg: rare_terms', body: { size: 0, aggs: { a: { rare_terms: { field: 'model', max_doc_count: 2 } } } } },
  { label: 'agg: significant_terms', body: { size: 0, aggs: { a: { significant_terms: { field: 'model' } } } } },
  { label: 'agg: histogram', body: { size: 0, aggs: { a: { histogram: { field: 'latency_ms', interval: 100 } } } } },
  { label: 'agg: date_histogram', body: { size: 0, aggs: { a: { date_histogram: { field: '@timestamp', calendar_interval: 'day' } } } } },
  { label: 'agg: auto_date_histogram', body: { size: 0, aggs: { a: { auto_date_histogram: { field: '@timestamp', buckets: 24 } } } } },
  { label: 'agg: variable_width_histogram', body: { size: 0, aggs: { a: { variable_width_histogram: { field: 'latency_ms', buckets: 10 } } } } },
  { label: 'agg: range', body: { size: 0, aggs: { a: { range: { field: 'latency_ms', ranges: [{ to: 100 }, { from: 100, to: 500 }, { from: 500 }] } } } } },
  { label: 'agg: date_range', body: { size: 0, aggs: { a: { date_range: { field: '@timestamp', ranges: [{ from: '2026-01-01', to: '2026-07-01' }, { from: '2026-07-01' }] } } } } },
  { label: 'agg: filter', body: { size: 0, aggs: { a: { filter: { term: { status: 'ok' } }, aggs: { c: { avg: { field: 'latency_ms' } } } } } } },
  { label: 'agg: filters', body: { size: 0, aggs: { a: { filters: { filters: { ok: { term: { status: 'ok' } }, err: { term: { status: 'error' } } } } } } } },
  { label: 'agg: missing', body: { size: 0, aggs: { a: { missing: { field: 'cost_usd' } } } } },
  { label: 'agg: global', body: { size: 0, query: { term: { status: 'error' } }, aggs: { all: { global: {}, aggs: { c: { value_count: { field: 'model' } } } } } } },
  { label: 'agg: adjacency_matrix', body: { size: 0, aggs: { a: { adjacency_matrix: { filters: { ok: { term: { status: 'ok' } }, fast: { range: { latency_ms: { lt: 100 } } } } } } } } },
  { label: 'agg: composite', body: { size: 0, aggs: { a: { composite: { size: 100, sources: [{ m: { terms: { field: 'model' } } }, { s: { terms: { field: 'status' } } }] } } } } },
  { label: 'agg: random_sampler', body: { size: 0, aggs: { s: { random_sampler: { probability: 0.1 }, aggs: { c: { avg: { field: 'latency_ms' } } } } } } },
  { label: 'agg: terms+avg(cost)', body: { size: 0, aggs: { m: { terms: { field: 'model', size: 10 }, aggs: { c: { avg: { field: 'cost_usd' } } } } } } },
  // §6c pipeline aggs
  { label: 'pipe: sum_bucket', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } } } }, total: { sum_bucket: { buckets_path: 'per_day>cost' } } } } },
  { label: 'pipe: avg_bucket', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } } } }, a: { avg_bucket: { buckets_path: 'per_day>cost' } } } } },
  { label: 'pipe: max_bucket', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } } } }, a: { max_bucket: { buckets_path: 'per_day>cost' } } } } },
  { label: 'pipe: stats_bucket', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } } } }, a: { stats_bucket: { buckets_path: 'per_day>cost' } } } } },
  { label: 'pipe: percentiles_bucket', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } } } }, a: { percentiles_bucket: { buckets_path: 'per_day>cost' } } } } },
  { label: 'pipe: derivative', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } }, d: { derivative: { buckets_path: 'cost' } } } } } } },
  { label: 'pipe: cumulative_sum', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } }, c: { cumulative_sum: { buckets_path: 'cost' } } } } } } },
  { label: 'pipe: moving_fn', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } }, m: { moving_fn: { buckets_path: 'cost', window: 3, script: 'MovingFunctions.unweightedAvg(values)' } } } } } } },
  { label: 'pipe: serial_diff', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } }, s: { serial_diff: { buckets_path: 'cost', lag: 1 } } } } } } },
  { label: 'pipe: bucket_script', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } }, cnt: { value_count: { field: 'model' } }, r: { bucket_script: { buckets_path: { c: 'cost', n: 'cnt' }, script: 'params.c/params.n' } } } } } } },
  { label: 'pipe: bucket_selector', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } }, sel: { bucket_selector: { buckets_path: { c: 'cost' }, script: 'params.c>0' } } } } } } },
  { label: 'pipe: bucket_sort', body: { size: 0, aggs: { per_day: { date_histogram: { field: '@timestamp', calendar_interval: 'day' }, aggs: { cost: { sum: { field: 'cost_usd' } }, srt: { bucket_sort: { sort: [{ cost: 'desc' }], size: 5 } } } } } } },
  // search features
  { label: 'feat: sort-heavy', body: { query: { match_all: {} }, size: 50, sort: [{ latency_ms: 'desc' }, { cost_usd: 'asc' }] } },
  { label: 'feat: deep from+size (from 500)', body: { query: { match_all: {} }, from: 500, size: 50 } },
  // search_after: sort on REAL corpus fields. Stock ES 8 rejects sorting on _id
  // (fielddata on _id is disabled -> 400), so the previous [{_id:'asc'}] tiebreak
  // made the probe read "unsupported" on BOTH engines. Page 1 (untimed) captures
  // the last hit's sort values; the benchmarked body pages past them via
  // search_after. Timing still goes through the exact same safeTimed path as
  // every other family (feasibility probe + warmup + open-loop iters).
  { label: 'feat: search_after', makeBody: async (base) => {
    const sort = [{ latency_ms: 'asc' }, { '@timestamp': 'asc' }];
    const first = await req(base, 'POST', S, { query: { match_all: {} }, size: 50, sort });
    const hits = first.j?.hits?.hits || [];
    const last = hits.length ? hits[hits.length - 1].sort : null;
    // Sentinel [0, 0] (int, epoch-millis) when capture failed: safeTimed's own
    // feasibility probe then classifies the family (4xx -> unsupported,
    // no response -> collapsed) instead of silently benchmarking page 1.
    return { query: { match_all: {} }, size: 50, sort, search_after: (last && last.length) ? last : [0, 0] };
  } },
  { label: 'feat: highlight', body: { query: { match: { status: 'ok' } }, size: 10, highlight: { fields: { status: {} } } } },
  // multi-op endpoints
  { label: 'feat: _count', body: { query: { match_all: {} } }, path: '/perf/_count' },
  { label: 'feat: _msearch', body: '{}\n{"query":{"match_all":{}},"size":1}\n', path: '/perf/_msearch', ndjson: true },
  { label: 'feat: _mget', body: { ids: ['1', '2', '3'] }, path: '/perf/_mget' },
];

// Families that need a purpose-built index/mapping the flat corpus lacks —
// logged (not silently dropped) so coverage gaps are explicit.
const SKIPPED_FAMILIES = [
  ['geo_distance / geo_bounding_box / geo_polygon / geo_shape', 'no geo_point/geo_shape field in corpus'],
  ['geohash_grid / geotile_grid / geo_distance-agg / geo_bounds / geo_centroid', 'no geo_point field in corpus'],
  ['ip_range / ip_prefix', 'no ip field in corpus'],
  ['nested / has_child / has_parent (join)', 'corpus is flat, no nested/join mapping'],
  ['span_term / span_near / span_or / span_not / span_first / span_containing / span_within', 'needs a text field with positions'],
  ['significant_text', 'needs an analyzed text field (corpus fields are keyword)'],
  ['semantic / hybrid retriever', 'needs a dense_vector field + embed step (see --knn)'],
  ['knn (as query/top-level)', 'covered separately by --knn on a purpose-built dense_vector index'],
  ['percolate', 'parses but no-ops to match_none — not benchmarkable for correctness'],
];

// 5-op read set reused for --mixed (matches sustained_mixed_probe.sh).
const MIXED_OPS = [
  ['match_all', { query: { match_all: {} }, size: 10 }],
  ['bool', { query: { bool: { must: [{ match: { status: 'ok' } }], filter: [{ range: { latency_ms: { gte: 200 } } }] } }, size: 10 }],
  ['range', { query: { range: { cost_usd: { gte: 0.01 } } }, size: 10 }],
  ['terms', { size: 0, aggs: { m: { terms: { field: 'model', size: 10 } } } }],
  ['cardinality', { size: 0, aggs: { u: { cardinality: { field: 'top_doc' } } } }],
];

// ────────────────────────────── ingest (multi-client) ──────────────────────────────
// ES-style auto-id _bulk, 10k/batch, cycling the corpus. Pre-generates all batch
// files (so file-write cost is excluded from the timed window — same trick as
// ingest_measure.sh) then fans them out across `clients` truly-concurrent async
// curl workers, bracketed by one performance.now() pair. Returns {ms,docs,dps}.
async function ingest(base, N, clients) {
  await req(base, 'DELETE', '/perf');
  await req(base, 'PUT', '/perf', MAPPING);
  const pre = (await req(base, 'GET', '/perf/_count')).j?.count ?? 0; // fresh index → 0, measured for safety
  const raw = fs.readFileSync(CORPUS, 'utf8').trim().split('\n');
  const RN = raw.length;
  const nb = Math.ceil(N / BATCH);
  const files = [];
  let written = 0;
  for (let b = 0; b < nb; b++) {
    let nd = '';
    for (let i = 0; i < BATCH && written < N; i++, written++) nd += '{"index":{}}\n' + raw[written % RN] + '\n';
    const f = `${SCRATCH}/_mx_batch_${b}.ndjson`;
    fs.writeFileSync(f, nd);
    files.push(f);
  }
  // round-robin the batch files across N workers so slices are ~equal.
  const slices = Array.from({ length: clients }, () => []);
  files.forEach((f, i) => slices[i % clients].push(f));
  // Capture each _bulk response (NOT >/dev/null) so `"errors":true` / rejected
  // items are visible; a silent drop must not inflate docs/s.
  let bulkErrors = 0, bulkItemErrors = 0;
  const worker = async (slice) => {
    for (const f of slice) {
      const { stdout } = await execP(`curl -s -XPOST ${base}/perf/_bulk -H 'content-type: application/x-ndjson' --data-binary @${f}`, { maxBuffer: 1 << 28 });
      try {
        const j = JSON.parse(stdout);
        if (j.errors) {
          bulkErrors++;
          bulkItemErrors += (j.items || []).filter((it) => { const o = it && Object.values(it)[0]; return o && typeof o.status === 'number' && o.status >= 400; }).length;
        }
      } catch { bulkErrors++; } // unparsable/empty response counts as an error
    }
  };
  const t0 = performance.now();
  await Promise.all(slices.map(worker));
  const ms = performance.now() - t0;
  await req(base, 'POST', '/perf/_refresh');
  const c = await req(base, 'GET', '/perf/_count');
  for (const f of files) { try { fs.unlinkSync(f); } catch {} }
  // dps from the VERIFIED indexed delta (post-refresh count − pre count), never
  // the intended `written`; flag a shortfall so a silent drop can't win the row.
  const indexed = (c.j?.count ?? 0) - pre;
  const shortfall = indexed !== written;
  return { ms, docs: c.j?.count ?? 0, written, indexed, dps: Math.round(indexed / (ms / 1000)), bulkErrors, bulkItemErrors, shortfall };
}

// ────────────────────────────── mixed read-under-write ──────────────────────────────
// Folds in sustained_mixed_probe.sh: initial ~300k load + refresh, then a
// BACKGROUND continuous bulk loop keeps flush+merge hot while a foreground read
// burst measures latency. Returns per-op {p50,p99,max} under write load.
async function mixedBench(base, log) {
  const raw = fs.readFileSync(CORPUS, 'utf8').trim().split('\n');
  const RN = raw.length;
  await req(base, 'DELETE', '/perf');
  await req(base, 'PUT', '/perf', MAPPING);
  let nd = '';
  for (let i = 0; i < BATCH; i++) nd += '{"index":{}}\n' + raw[i % RN] + '\n';
  const tag = base.replace(/[^a-z0-9]+/gi, '_');
  const f = `${SCRATCH}/_mx_mixed_${tag}.ndjson`;
  fs.writeFileSync(f, nd);
  log('     initial load 300k...');
  for (let b = 0; b < 30; b++) await execP(`curl -s -XPOST ${base}/perf/_bulk -H 'content-type: application/x-ndjson' --data-binary @${f} >/dev/null`, { maxBuffer: 1 << 28 });
  await req(base, 'POST', '/perf/_refresh');
  // ── OUT-OF-PROCESS sustained writer ──
  // A DETACHED bash loop drives the background bulk writes so the write load does
  // NOT share this Node process's event loop; curl-spawn + I/O from the writer no
  // longer perturb the foreground performance.now() read timings (measures server
  // tail, not client contention). A flag file gates the loop; a counter file
  // exposes achieved background throughput.
  const flag = `${SCRATCH}/_mx_run_${tag}.flag`;
  const cntF = `${SCRATCH}/_mx_wcount_${tag}.txt`;
  fs.writeFileSync(flag, '1');
  fs.writeFileSync(cntF, '0');
  const script = `n=0; while [ -f '${flag}' ]; do curl -s -XPOST '${base}/perf/_bulk' -H 'content-type: application/x-ndjson' --data-binary @'${f}' >/dev/null 2>&1; n=$((n+1)); printf '%s' "$n" > '${cntF}'; done`;
  const child = spawn('bash', ['-c', script], { detached: true, stdio: 'ignore' });
  child.unref();
  const wStart = performance.now();
  await new Promise((r) => setTimeout(r, 2000)); // let ingest+merge get going
  const out = {};
  // Longer read window + a few thousand samples per op so (a) segment
  // flush/merge actually occurs during measurement and (b) p99 rests on enough
  // tail samples to be trustworthy (index floor(0.99*3000)=2970 → ~30 tail obs).
  for (const [label, body] of MIXED_OPS) {
    out[label] = await safeTimed(base, S, body, { iters: 3000, warmup: 20, rate: 300 });
    log(`     mixed ${label}: p99 ${f3(statVal(out[label], 'p99'))}ms max ${f3(statVal(out[label], 'max'))}ms (achieved ${out[label] && out[label].achievedRate != null ? out[label].achievedRate : '—'}/s, under write load)`);
  }
  // stop the writer and measure achieved background throughput
  try { fs.unlinkSync(flag); } catch {}            // loop exits after its current curl
  await new Promise((r) => setTimeout(r, 300));
  try { process.kill(-child.pid); } catch {}       // kill the detached process group (bash + curl)
  const wEnd = performance.now();
  let writes = 0;
  try { writes = parseInt(fs.readFileSync(cntF, 'utf8').trim(), 10) || 0; } catch {}
  try { fs.unlinkSync(cntF); } catch {}
  try { fs.unlinkSync(f); } catch {}
  out.__bg = { docs: writes * BATCH, dps: Math.round((writes * BATCH) / ((wEnd - wStart) / 1000)) };
  return out;
}

// ────────────────────────────── kNN latency + recall ──────────────────────────────
function mulberry32(a) {
  return function () {
    a |= 0; a = (a + 0x6D2B79F5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
function cosine(a, b) {
  let d = 0, na = 0, nb = 0;
  for (let i = 0; i < a.length; i++) { d += a[i] * b[i]; na += a[i] * a[i]; nb += b[i] * b[i]; }
  return d / (Math.sqrt(na) * Math.sqrt(nb) || 1);
}
// dims=128, ~50k random unit-ish vectors indexed with EXPLICIT ids so engine
// hit _ids map back to generated vectors — recall@10 vs a client-side exact
// brute-force cosine top-10. Same vectors sent to both engines.
async function knnBench(base, log, dims = 128, M = 50000) {
  await req(base, 'DELETE', '/perfvec');
  const put = await req(base, 'PUT', '/perfvec', { mappings: { properties: { v: { type: 'dense_vector', dims, index: true, similarity: 'cosine' } } } });
  if (put.status >= 400) return { unsupported: true, status: put.status };
  const rng = mulberry32(0xC0FFEE);
  const vecs = new Array(M);
  for (let i = 0; i < M; i++) { const v = new Array(dims); for (let k = 0; k < dims; k++) v[k] = rng() * 2 - 1; vecs[i] = v; }
  log(`     indexing ${M} × ${dims}-d vectors...`);
  let nd = '';
  const flush = async () => { const f = `${SCRATCH}/_mxvec.ndjson`; fs.writeFileSync(f, nd); await execP(`curl -s -XPOST ${base}/perfvec/_bulk -H 'content-type: application/x-ndjson' --data-binary @${f} >/dev/null`, { maxBuffer: 1 << 28 }); nd = ''; };
  for (let i = 0; i < M; i++) {
    nd += `{"index":{"_id":"${i}"}}\n` + JSON.stringify({ v: vecs[i] }) + '\n';
    if (nd.length > 4e6) await flush();
  }
  if (nd) await flush();
  await req(base, 'POST', '/perfvec/_refresh');
  const qv = new Array(dims); for (let k = 0; k < dims; k++) qv[k] = rng() * 2 - 1;
  // exact top-10 (client-side brute force)
  const scored = vecs.map((v, i) => [i, cosine(qv, v)]);
  scored.sort((a, b) => b[1] - a[1]);
  const exact = new Set(scored.slice(0, 10).map((x) => x[0]));
  const body = { knn: { field: 'v', query_vector: qv, k: 10, num_candidates: 100 }, size: 10 };
  const one = await req(base, 'POST', '/perfvec/_search', body);
  const ids = (one.j?.hits?.hits || []).map((h) => parseInt(h._id, 10));
  const recall = ids.length ? ids.filter((id) => exact.has(id)).length / 10 : null;
  // >=2000 samples so kNN p99/max aren't dominated by 1-2 tail observations.
  const t = await safeTimed(base, '/perfvec/_search', body, { iters: 2000, warmup: 15, rate: 200 });
  return { ...t, recall };
}

// ────────────────────────────── on-disk index size ──────────────────────────────
async function statsBytes(base) {
  try { const r = await req(base, 'GET', '/perf/_stats'); return r.j?._all?.total?.store?.size_in_bytes ?? null; }
  catch (e) { process.stderr.write(`   [disk] _stats error for ${base}: ${String(e && e.message || e)}\n`); return null; }
}
// Measure BOTH engines on the SAME, index-scoped basis: the /perf `_stats` store
// size. No `du` on shared/stale candidate data dirs (which can hold translog,
// system indices, or leftovers from unrelated benchmark runs), and never `du` for
// one engine while using `_stats` for the other — that compared different
// quantities as if equal and could hand a spurious disk WIN.
async function diskBytes(base) {
  return await statsBytes(base);
}

// ────────────────────────────── CLI ──────────────────────────────
function parseDocs(s) {
  const m = /^([\d.]+)\s*([kmKM]?)$/.exec(s.trim());
  if (!m) return parseInt(s, 10) || 0;
  let n = parseFloat(m[1]);
  const u = m[2].toLowerCase();
  if (u === 'k') n *= 1e3; else if (u === 'm') n *= 1e6;
  return Math.round(n);
}
function parseArgs(argv) {
  const a = {
    clients: '1', docs: '100k,1m',
    xerj: 'http://localhost:9200', es: 'http://localhost:9201',
    mixed: false, knn: false, out: DEFAULT_OUT, readsOnly: false, ingestOnly: false,
  };
  for (let i = 2; i < argv.length; i++) {
    let t = argv[i], inlineVal;
    const eq = t.indexOf('=');
    if (t.startsWith('--') && eq !== -1) { inlineVal = t.slice(eq + 1); t = t.slice(0, eq); }
    const val = () => (inlineVal !== undefined ? inlineVal : argv[++i]);
    switch (t) {
      case '--clients': a.clients = val(); break;
      case '--docs': a.docs = val(); break;
      case '--xerj': a.xerj = val(); break;
      case '--es': a.es = val(); break;
      case '--out': a.out = val(); break;
      case '--mixed': a.mixed = true; break;
      case '--knn': a.knn = true; break;
      case '--reads-only': a.readsOnly = true; break;
      case '--ingest-only': a.ingestOnly = true; break;
      default: process.stderr.write(`[warn] unknown flag: ${t}\n`);
    }
  }
  a.clientsList = a.clients.split(',').map((x) => parseInt(x.trim(), 10)).filter((x) => x > 0);
  a.docsList = a.docs.split(',').map((x) => parseDocs(x)).filter((x) => x > 0);
  if (!a.clientsList.length) a.clientsList = [1];
  if (!a.docsList.length) a.docsList = [100000];
  return a;
}
const humanDocs = (n) => (n >= 1e6 ? (n / 1e6) + 'm' : n >= 1e3 ? (n / 1e3) + 'k' : String(n));
const mb = (b) => (b / 1048576).toFixed(1) + ' MB';

// ────────────────────────────── scorecard ──────────────────────────────
// One row: XERJ value, ES value, ratio expressed so >1 == XERJ better, and a
// plain-ASCII WIN/LOSE verdict. Unsupported/missing on either side => N/A
// (does NOT fail CI). higherBetter=true for docs/s + recall; false for latency
// + disk bytes.
// Optional xraw/eraw are the raw timed()/knnBench() results: when present they
// let disp() distinguish a genuine 4xx feasibility rejection ("unsupported
// (400)") from a harness/transport error ("error") — so a dead engine mid-run
// no longer masquerades as a missing feature in the scorecard.
function scoreRow(dim, xv, ev, higherBetter, fmt, xraw, eraw) {
  const disp = (v, raw) => {
    if (v != null) return !isFinite(v) ? 'collapsed' : fmt(v);
    if (raw && raw.unsupported) return `unsupported (${raw.status ?? '4xx'})`;
    if (raw && raw.error) return 'error';
    return 'unsupported';
  };
  const xs = disp(xv, xraw);
  const es = disp(ev, eraw);
  let ratio = '—', verdict = 'N/A';
  if (xv != null && ev != null) {
    const better = higherBetter ? xv >= ev : xv <= ev;
    const r = higherBetter ? (ev ? xv / ev : 0) : (xv ? ev / xv : 0);
    ratio = isFinite(r) && r > 0 ? r.toFixed(2) + '×' : '—';
    verdict = better ? 'WIN' : 'LOSE';
  }
  return { dim, xs, es, ratio, verdict };
}

// ────────────────────────────── main ──────────────────────────────
async function main() {
  const A = parseArgs(process.argv);
  const log = (s) => process.stderr.write(s + '\n');
  const engines = [
    { name: 'XERJ', url: A.xerj },
    { name: 'ES', url: A.es },
  ];
  const READ_DOCS = Math.max(...A.docsList);

  log('══════════════════════════════════════════════════════════════');
  log(' XERJ vs Elasticsearch — benchmark matrix');
  log(`   docs:    ${A.docsList.map(humanDocs).join(', ')}`);
  log(`   clients: ${A.clientsList.join(', ')}`);
  log(`   xerj:    ${A.xerj}`);
  log(`   es:      ${A.es}`);
  log(`   modes:   reads=${!A.ingestOnly} ingest=${!A.readsOnly} mixed=${A.mixed} knn=${A.knn}`);
  log('══════════════════════════════════════════════════════════════');

  // health check (non-fatal)
  const alive = {};
  for (const e of engines) {
    try { const v = await req(e.url, 'GET', '/'); alive[e.name] = v.status < 500; log(`   ${e.name} (${e.url}) — ${v.j?.version?.number || 'reachable'}`); }
    catch (err) { alive[e.name] = false; log(`   ${e.name} (${e.url}) — UNREACHABLE: ${err.message}`); }
  }

  const R = { ingest: {}, reads: {}, mixed: {}, knn: {}, disk: {} };

  // ── ingest matrix ──
  if (!A.readsOnly) {
    for (const docs of A.docsList) {
      for (const clients of A.clientsList) {
        const key = `${humanDocs(docs)} × c${clients}`;
        R.ingest[key] = {};
        for (const e of engines) {
          if (!alive[e.name]) { R.ingest[key][e.name] = { error: 'unreachable' }; continue; }
          log(`   [ingest] ${e.name} ${key} ...`);
          try {
            const ing = await ingest(e.url, docs, clients);
            R.ingest[key][e.name] = ing;
            const warn = `${ing.bulkErrors ? ` [BULK ERRORS: ${ing.bulkErrors} resp, ${ing.bulkItemErrors} items]` : ''}${ing.shortfall ? ` [COUNT SHORTFALL: indexed ${ing.indexed} != written ${ing.written}]` : ''}`;
            log(`     ${e.name} ${key}: ${ing.indexed.toLocaleString()}/${ing.written.toLocaleString()} docs in ${(ing.ms / 1000).toFixed(1)}s = ${ing.dps.toLocaleString()} docs/s${warn}`);
          } catch (err) { R.ingest[key][e.name] = { error: String(err.message || err) }; log(`     ${e.name} ${key}: ERROR ${err.message}`); }
        }
      }
    }
  }

  // ── reads (+ mixed + knn) ──
  if (!A.ingestOnly) {
    // guarantee a consistent read corpus on each engine
    for (const e of engines) {
      if (!alive[e.name]) continue;
      log(`   [reads] ${e.name}: seeding ${humanDocs(READ_DOCS)} docs for read surface ...`);
      try { const s = await ingest(e.url, READ_DOCS, 1); if (s.shortfall || s.bulkErrors) log(`     seed WARN: indexed ${s.indexed}/${s.written}, bulkErrors ${s.bulkErrors}`); }
      catch (err) { log(`     seed ERROR: ${err.message}`); }
    }
    // Bring BOTH engines to a comparable quiescent state AFTER both seeds finish,
    // so neither gets extra background-merge time from seed ordering (segment
    // count materially affects read latency). Force-merge to 1 segment + equal
    // settle delay so reads are measured on equivalently-merged indexes.
    for (const e of engines) {
      if (!alive[e.name]) continue;
      log(`   [reads] ${e.name}: force-merge to 1 segment (quiesce) ...`);
      try { await req(e.url, 'POST', '/perf/_forcemerge?max_num_segments=1'); } catch (err) { log(`     forcemerge WARN: ${err.message}`); }
    }
    await new Promise((r) => setTimeout(r, 3000)); // equal fixed settle for both engines
    for (const fam of READ_FAMILIES) {
      R.reads[fam.label] = {};
      for (const e of engines) {
        if (!alive[e.name]) { R.reads[fam.label][e.name] = { error: 'unreachable' }; continue; }
        const path = fam.path || S;
        // families with per-engine setup (e.g. search_after sort-value capture)
        // build their body against THIS engine right before timing.
        const body = fam.makeBody ? await fam.makeBody(e.url) : fam.body;
        R.reads[fam.label][e.name] = await safeTimed(e.url, path, body, { ndjson: !!fam.ndjson });
      }
      const xrr = R.reads[fam.label].XERJ, err_ = R.reads[fam.label].ES;
      const xr = statVal(xrr), er = statVal(err_);
      const mm = signalMismatch(xrr && xrr.signal, err_ && err_.signal);
      log(`   [read] ${fam.label.padEnd(38)} XERJ p50 ${f3(xr)} (${sigStr(xrr && xrr.signal)})  ES p50 ${f3(er)} (${sigStr(err_ && err_.signal)})${mm ? `  [MISMATCH: ${mm}]` : ''}`);
    }
    for (const [fam, why] of SKIPPED_FAMILIES) log(`   [skip] ${fam} — ${why}`);

    if (A.mixed) {
      for (const e of engines) {
        if (!alive[e.name]) continue;
        log(`   [mixed] ${e.name}: read-under-write ...`);
        try {
          R.mixed[e.name] = await mixedBench(e.url, log);
          const bg = R.mixed[e.name].__bg;
          if (bg) log(`     ${e.name} background write load: ${bg.dps.toLocaleString()} docs/s (${bg.docs.toLocaleString()} docs during read window)`);
        } catch (err) {
          // Log WHY + whether the engine is still up: all-unsupported mixed cells
          // have previously meant "engine died between phases", not "no feature".
          log(`     mixed ERROR (${e.name}): ${err.message} — engine ${await engineAlive(e.url)}`);
          R.mixed[e.name] = {};
        }
      }
    }
    if (A.knn) {
      for (const e of engines) {
        if (!alive[e.name]) { R.knn[e.name] = { unsupported: true }; continue; }
        log(`   [knn] ${e.name}: latency + recall@10 ...`);
        try { R.knn[e.name] = await knnBench(e.url, log); log(`     ${e.name} kNN: p50 ${f3(statVal(R.knn[e.name]))}ms recall@10 ${f3(statVal(R.knn[e.name], 'recall'))}`); }
        catch (err) {
          R.knn[e.name] = { error: String(err.message || err) };
          log(`     kNN ERROR (${e.name}): ${err.message} — engine ${await engineAlive(e.url)}`);
        }
      }
    }
  }

  // ── disk ──
  for (const e of engines) {
    if (!alive[e.name]) { R.disk[e.name] = null; continue; }
    R.disk[e.name] = await diskBytes(e.url); // index-only _stats store, identical basis for both
    // null store size + dead engine == crash between phases, not a missing API.
    if (R.disk[e.name] == null) log(`   [disk] ${e.name}: _stats gave no store size — engine ${await engineAlive(e.url)}`);
    log(`   [disk] ${e.name}: ${R.disk[e.name] == null ? 'unknown' : mb(R.disk[e.name])} (/perf _stats store, apples-to-apples)`);
  }

  // ────────────────────────────── build scorecard ──────────────────────────────
  const rows = [];
  // ingest cells (higher docs/s = XERJ win). A bulk-error or count-shortfall on
  // either engine invalidates the comparison (silently-dropped docs inflate
  // docs/s) → mark the cell N/A with a note rather than scoring it.
  const fmtDps = (v) => (v == null ? 'unsupported' : v.toLocaleString());
  const ingestFlagged = (g) => g && (g.bulkErrors > 0 || g.shortfall);
  for (const key of Object.keys(R.ingest)) {
    const c = R.ingest[key];
    if (ingestFlagged(c.XERJ) || ingestFlagged(c.ES)) {
      const note = [];
      for (const nm of ['XERJ', 'ES']) {
        const g = c[nm];
        if (ingestFlagged(g)) note.push(`${nm}${g.bulkErrors ? ` ${g.bulkErrors}bulk-err` : ''}${g.shortfall ? ` ${g.indexed}/${g.written}` : ''}`);
      }
      rows.push({ dim: `ingest ${key} (docs/s) [${note.join('; ')}]`, xs: fmtDps(c.XERJ?.dps ?? null), es: fmtDps(c.ES?.dps ?? null), ratio: 'errors', verdict: 'N/A' });
    } else {
      rows.push(scoreRow(`ingest ${key} (docs/s)`, c.XERJ?.dps ?? null, c.ES?.dps ?? null, true, (v) => v.toLocaleString()));
    }
  }
  // read families (lower p50 = XERJ win) — but only when BOTH engines did
  // materially the same work. If results diverge (one no-ops to empty while the
  // other matches, or hit counts differ), mark N/A so an effectively-unsupported
  // feature can't win the row on latency.
  const fmtMs = (v) => (v == null ? 'unsupported' : v.toFixed(2));
  for (const fam of READ_FAMILIES) {
    if (!R.reads[fam.label]) continue;
    const xrr = R.reads[fam.label].XERJ, err_ = R.reads[fam.label].ES;
    const mm = signalMismatch(xrr && xrr.signal, err_ && err_.signal);
    if (mm) {
      rows.push({ dim: `read ${fam.label} (p50 ms) [result mismatch: ${mm}]`, xs: fmtMs(statVal(xrr)), es: fmtMs(statVal(err_)), ratio: 'mismatch', verdict: 'N/A' });
    } else {
      rows.push(scoreRow(`read ${fam.label} (p50 ms)`, statVal(xrr), statVal(err_), false, (v) => v.toFixed(2), xrr, err_));
    }
  }
  // mixed read-under-write (lower p99 = XERJ win)
  if (A.mixed) {
    for (const [label] of MIXED_OPS) {
      const xv = statVal(R.mixed.XERJ?.[label], 'p99'), ev = statVal(R.mixed.ES?.[label], 'p99');
      rows.push(scoreRow(`mixed ${label} (p99 ms, under write)`, xv, ev, false, (v) => v.toFixed(2), R.mixed.XERJ?.[label], R.mixed.ES?.[label]));
    }
  }
  // knn latency (lower p50 = win) + recall (higher = win)
  if (A.knn) {
    rows.push(scoreRow('kNN k=10 (p50 ms)', statVal(R.knn.XERJ), statVal(R.knn.ES), false, (v) => v.toFixed(2), R.knn.XERJ, R.knn.ES));
    rows.push(scoreRow('kNN recall@10', statVal(R.knn.XERJ, 'recall'), statVal(R.knn.ES, 'recall'), true, (v) => (v * 100).toFixed(1) + '%', R.knn.XERJ, R.knn.ES));
  }
  // disk (smaller = win)
  rows.push(scoreRow('index on-disk size', R.disk.XERJ ?? null, R.disk.ES ?? null, false, mb));

  const loses = rows.filter((r) => r.verdict === 'LOSE');

  let md = `# XERJ vs Elasticsearch — Scorecard\n\n`;
  md += `Generated by \`demo/playbooks/bench-matrix.mjs\`. Identical workload, same machine (localhost), single node, security off. `;
  md += `Corpus: real LLM-telemetry (${humanDocs(READ_DOCS)} docs for reads). `;
  md += `docs = {${A.docsList.map(humanDocs).join(', ')}}, clients = {${A.clientsList.join(', ')}}.\n\n`;
  md += `Verdict is from XERJ's POV: WIN = XERJ better (lower latency / higher docs·s / smaller disk / higher recall). `;
  md += `Ratio is normalized so **>1× means XERJ is better**. Any LOSE fails CI.\n\n`;
  md += `| dimension | XERJ | ES | ratio | verdict |\n|---|--:|--:|--:|:--:|\n`;
  for (const r of rows) md += `| ${r.dim} | ${r.xs} | ${r.es} | ${r.ratio} | ${r.verdict} |\n`;
  md += `\n`;
  if (SKIPPED_FAMILIES.length) {
    md += `## Skipped families (need a purpose-built index the flat corpus lacks)\n\n`;
    for (const [fam, why] of SKIPPED_FAMILIES) md += `- \`${fam}\` — ${why}\n`;
    md += `\n`;
  }
  md += `_Summary: ${rows.filter((r) => r.verdict === 'WIN').length} WIN, ${loses.length} LOSE, ${rows.filter((r) => r.verdict === 'N/A').length} N/A._\n`;

  fs.writeFileSync(A.out, md);
  // keep the head-to-head slice around for continuity with bench-vs-es.mjs
  try { fs.writeFileSync(HEADTOHEAD_OUT, md); } catch {}
  log(`\nwrote ${A.out}`);
  console.log(md);

  if (loses.length) { log(`\nFAIL: ${loses.length} LOSE cell(s) — ${loses.map((r) => r.dim).join('; ')}`); process.exit(1); }
}

main().catch((e) => { console.error(e); process.exit(1); });
