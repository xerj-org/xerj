// ============================================================
// XERJ.ai — Per-panel query path (declarative user panels)
//
// A user-built panel carries a declarative `query` block instead of a
// closure. This module turns that block into a single ES-compat
// `_search` body, runs it same-origin against the engine, and
// normalises the response into a small shape the declarative renderer
// (ux/panel-render.js) consumes:
//
//   { total, value, series:[{t,count}], buckets:[{key,count}], hits:[...] }
//
// Results are cached by (index + body) so re-renders are synchronous
// cache hits; a subscriber (`onPanelData`) lets the app re-render when
// an in-flight query settles — the same optimistic pattern the search
// box uses. This closes the "custom dashboards have no live data" gap
// without touching the 8-dashId dispatch in backends/xerj.js.
//
// query block shape (all optional except index+kind):
//   { index, kind, q, field, metric, agg, interval, size, time }
//     kind   : 'count' | 'metric' | 'timeseries' | 'terms' | 'search'
//     metric : 'count' | 'sum' | 'avg' | 'max' | 'min'
//     time   : true → constrain to the global range; default off so
//              sparse/seed data still shows for count/terms/metric.
// ============================================================

// rc.1 aggregation materialisation cap workaround — mirrors
// backends/xerj.js. A large `size` lifts the doc set aggs run over
// past the corpus; the `.hits` payload is discarded for agg kinds.
const AGG_BYPASS_SIZE = 9999;
const TS_FIELD = '@timestamp';

function rangeToSince(range) {
  switch (range) {
    case '1H':  return 'now-1h';
    case '24H': return 'now-24h';
    case '7D':  return 'now-7d';
    case '30D': return 'now-30d';
    case '90D': return 'now-90d';
    default:    return 'now-24h';
  }
}
function bucketSize(range) {
  switch (range) {
    case '1H':  return '1m';
    case '24H': return '1h';
    case '7D':  return '6h';
    case '30D': return '1d';
    case '90D': return '1d';
    default:    return '1h';
  }
}

const METRICS = new Set(['sum', 'avg', 'max', 'min']);

/** WHERE clause from the panel's free-text `q` + optional time range. */
function buildWhere(q, useTime, range) {
  const clauses = [];
  const text = (q || '').trim();
  if (text) {
    // query_string handles both `field:value` and loose text and is
    // supported by the engine's ES-compat parser.
    clauses.push({ query_string: { query: text } });
  }
  if (useTime) {
    clauses.push({ range: { [TS_FIELD]: { gte: rangeToSince(range) } } });
  }
  if (!clauses.length) return { match_all: {} };
  if (clauses.length === 1) return clauses[0];
  return { bool: { must: clauses } };
}

/** Translate a declarative panel → { index, body }. Throws on nonsense. */
export function buildBody(panel, ctx = {}) {
  const q = panel.query || {};
  const kind = q.kind || 'count';
  const range = ctx.range || '24H';
  const rawIndex = (q.index || '').trim();
  const index = !rawIndex || rawIndex === '*' ? '_all' : rawIndex;
  const useTime = q.time === true || kind === 'timeseries';
  const where = buildWhere(q.q, useTime, range);
  const metric = q.metric && METRICS.has(q.metric) ? q.metric : null;

  let body;
  if (kind === 'count') {
    body = { query: where, size: 0, track_total_hits: true };
  } else if (kind === 'metric') {
    if (!q.field) throw new Error('metric needs a field');
    const m = metric || 'avg';
    body = {
      query: where,
      size: AGG_BYPASS_SIZE,
      track_total_hits: true,
      aggs: { m: { [m]: { field: q.field } } },
    };
  } else if (kind === 'timeseries') {
    const inner = metric && q.field ? { m: { [metric]: { field: q.field } } } : undefined;
    body = {
      query: where,
      size: AGG_BYPASS_SIZE,
      track_total_hits: true,
      aggs: {
        tl: {
          date_histogram: { field: q.timeField || TS_FIELD, fixed_interval: q.interval || bucketSize(range) },
          ...(inner ? { aggs: inner } : {}),
        },
      },
    };
  } else if (kind === 'terms') {
    if (!q.field) throw new Error('terms needs a field');
    const inner = metric && q.metricField ? { m: { [metric]: { field: q.metricField } } } : undefined;
    body = {
      query: where,
      size: AGG_BYPASS_SIZE,
      track_total_hits: true,
      aggs: {
        t: {
          terms: { field: q.field, size: q.size || 8 },
          ...(inner ? { aggs: inner } : {}),
        },
      },
    };
  } else if (kind === 'search') {
    body = { query: where, size: q.size || 25, track_total_hits: true };
  } else {
    throw new Error(`unknown query kind '${kind}'`);
  }
  return { index, body, kind, metric };
}

/** Normalise an ES response into the render shape. */
function normalise(resp, built) {
  const total = resp?.hits?.total?.value ?? resp?.hits?.total ?? 0;
  const out = { total, took: resp?.took ?? 0 };
  const a = resp?.aggregations || {};
  if (built.kind === 'count') {
    out.value = total;
  } else if (built.kind === 'metric') {
    out.value = a.m?.value ?? null;
  } else if (built.kind === 'timeseries') {
    out.series = (a.tl?.buckets || []).map((b) => ({
      t: b.key_as_string || b.key,
      count: b.m ? (b.m.value ?? 0) : b.doc_count,
    }));
  } else if (built.kind === 'terms') {
    out.buckets = (a.t?.buckets || []).map((b) => ({
      key: String(b.key),
      count: b.m ? (b.m.value ?? 0) : b.doc_count,
    }));
  } else if (built.kind === 'search') {
    out.hits = (resp?.hits?.hits || []).map((h) => ({
      _id: h._id, _index: h._index, _score: h._score, _source: h._source,
    }));
  }
  return out;
}

async function runQuery(built, signal) {
  const path = `/${encodeURIComponent(built.index)}/_search`;
  const r = await fetch(path, {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'content-type': 'application/json', accept: 'application/json' },
    body: JSON.stringify(built.body),
    signal,
  });
  if (!r.ok) {
    const txt = await r.text().catch(() => '');
    throw new Error(`_search ${built.index} HTTP ${r.status}: ${txt.slice(0, 160)}`);
  }
  const json = await r.json();
  if (json && json.error) {
    throw new Error(typeof json.error === 'string' ? json.error : (json.error.reason || 'search error'));
  }
  return normalise(json, built);
}

// ── cache + subscription ─────────────────────────────────────────────
const cache = new Map();     // key -> normalised result | { __error }
const inflight = new Map();  // key -> Promise
const subs = new Set();
let notifyTimer = null;

function notify() {
  if (notifyTimer) return;
  notifyTimer = setTimeout(() => {
    notifyTimer = null;
    for (const cb of subs) { try { cb(); } catch { /* ignore */ } }
  }, 30);
}

/** Subscribe to "a panel query settled". Returns an unsubscribe fn. */
export function onPanelData(cb) {
  subs.add(cb);
  return () => subs.delete(cb);
}

function keyFor(built) {
  return built.index + '|' + JSON.stringify(built.body);
}

/**
 * Synchronous cache read. Fires the query (once) if not cached and
 * returns `{status:'loading'}`; when it settles, subscribers are
 * notified so the app re-renders into a cache hit.
 *
 * status ∈ 'ready' | 'loading' | 'error' | 'unconfigured'
 */
export function panelResult(panel, ctx = {}) {
  if (!panel || !panel.query || !panel.query.index || !panel.query.kind) {
    return { status: 'unconfigured' };
  }
  let built;
  try { built = buildBody(panel, ctx); }
  catch (e) { return { status: 'unconfigured', error: String(e.message || e) }; }
  const key = keyFor(built);
  if (cache.has(key)) {
    const v = cache.get(key);
    if (v && v.__error) return { status: 'error', error: v.__error };
    return { status: 'ready', data: v };
  }
  if (!inflight.has(key)) {
    const p = runQuery(built)
      .then((res) => { cache.set(key, res); inflight.delete(key); notify(); return res; })
      .catch((err) => { cache.set(key, { __error: String(err.message || err) }); inflight.delete(key); notify(); });
    inflight.set(key, p);
  }
  return { status: 'loading' };
}

/**
 * Promise form for the builder's live preview — always returns a fresh
 * result (also seeds the shared cache so the panel paints instantly once
 * saved).
 */
export async function fetchPanel(panel, ctx = {}) {
  const built = buildBody(panel, ctx);
  const key = keyFor(built);
  const res = await runQuery(built);
  cache.set(key, res);
  return res;
}
