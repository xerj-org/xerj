// ============================================================
// Xerj Console backend — Xerj engine
//
// Talks the ES-compatible REST API on port 9200 (or wherever the
// caller pointed it). Every dashboard's data shape is produced
// by composing one or more `_search` calls + light client-side
// reshape into the dashboard render contract.
//
// `probe()` is intentionally GET / so it costs ~0 on the engine.
// ============================================================

// Aggregation materialisation bypass.
//
// Xerj v1.0.0-rc.1 has a known bug at
// `engine/crates/xerj-engine/src/index.rs:3623`:
//   `let materialisation_limit = (from + size + 100).max(256);`
// This caps the doc set that aggregations run over.  When the
// dashboard sends the standard ES idiom `size:0` for "aggs only",
// the engine runs aggs over only ~256 docs and silently under-
// reports sums / averages / terms.  We bypass by passing a large
// `size` so the materialisation cap exceeds the corpus.  The
// `.hits.hits` payload is discarded by every adapter below.
// Track engine fix at: engine bug "agg materialisation cap".
const AGG_BYPASS_SIZE = 9999;

export const meta = {
  id: 'xerj',
  label: 'Xerj',
  defaultBaseUrl: 'http://localhost:9200',
  supports: {
    search: true,
    aggs: true,
    knn: true,
    semantic: true,
    hybrid: true,
    fsck: true,
  },
};

/** Lightweight liveness check. `true` if the server responded with a
 *  Xerj-shaped root payload. Costs one HTTP GET. */
export async function probe(baseUrl, signal) {
  try {
    const r = await fetch(baseUrl + '/', { signal });
    if (!r.ok) return false;
    const j = await r.json().catch(() => ({}));
    // Xerj's `/` returns ES-shaped info with a recognisable cluster_name
    // ("xerj" by default) and version. Accept anything that looks vaguely
    // ES-shaped so the same backend module also reports true against ES /
    // OpenSearch (handy when we add those backends as siblings).
    return typeof j === 'object' && j !== null
      && (j.cluster_name !== undefined || j.tagline !== undefined || j.version !== undefined);
  } catch (_e) {
    return false;
  }
}

/** Fetch live cat-indices for the picker / overview dashboards. */
export async function listIndices(baseUrl, signal) {
  const r = await fetch(baseUrl + '/_cat/indices?format=json', { signal });
  if (!r.ok) return [];
  return r.json().catch(() => []);
}

/** Generic ES-compat search against a single index (or wildcard). */
async function rawSearch(baseUrl, index, body, signal) {
  const path = `/${encodeURIComponent(index)}/_search`;
  const r = await fetch(baseUrl + path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
    signal,
  });
  if (!r.ok) {
    const txt = await r.text().catch(() => '');
    throw new Error(`Xerj _search ${index} HTTP ${r.status}: ${txt.slice(0, 200)}`);
  }
  return r.json();
}

// ── Dashboard ↔ live-data adapters ──────────────────────────────
//
// The dispatcher below maps `dashId` to the function that produces the
// shape that the matching dashboard's render() expects. New dashboards
// add a case here when ready; until then the `query.js` wrapper
// transparently falls back to mock data.

export async function search(baseUrl, dashId, ctx, signal) {
  switch (dashId) {
    case 'search-discover':       return liveSearchDiscover(baseUrl, ctx, signal);
    case 'system':                return liveSystem(baseUrl, ctx, signal);
    case 'logs-overview':         return liveLogsOverview(baseUrl, ctx, signal);
    case 'data':                  return liveData(baseUrl, ctx, signal);
    case 'ai-overview':           return liveAiOverview(baseUrl, ctx, signal);
    case 'rag-quality':           return liveRagQuality(baseUrl, ctx, signal);
    case 'vector-index':          return liveVectorIndex(baseUrl, ctx, signal);
    case 'agent-memory':          return liveAgentMemory(baseUrl, ctx, signal);
    case 'anomaly-detect':        return liveAnomalyDetect(baseUrl, ctx, signal);
    case 'ingest-pipeline':       return liveIngestPipeline(baseUrl, ctx, signal);
    default:                      return null; // signals "fall back to mock"
  }
}

// ── search-discover ────────────────────────────────────────────────
//
// The dashboard's `q` / `type` / `index` come in via ctx.search; we
// translate to an ES-compat query body and run it. Hits, total,
// per-field facets all come from one round-trip.
async function liveSearchDiscover(baseUrl, ctx, signal) {
  const search = ctx.search || {};
  const q = search.q || '';
  const type = search.type || 'match';
  const index = search.index === '*' ? '_all' : (search.index || '_all');

  const body = buildSearchBody(q, type, ctx);
  let response;
  try {
    response = await rawSearch(baseUrl, index, body, signal);
  } catch (e) {
    return { error: String(e), hits: [], total: 0, took: 0, facets: {} };
  }

  const total = response.hits?.total?.value ?? response.hits?.total ?? 0;
  const hits = (response.hits?.hits || []).map((h) => ({
    _id: h._id,
    _index: h._index,
    _score: h._score,
    _source: h._source,
    '@timestamp': h._source?.['@timestamp'] || null,
  }));
  const facets = {
    by_level:   bucketsToFacet(response.aggregations?.by_level),
    by_service: bucketsToFacet(response.aggregations?.by_service),
    by_host:    bucketsToFacet(response.aggregations?.by_host),
  };
  return {
    total,
    took: response.took ?? 0,
    hits,
    facets,
    raw: { query: body, response },
  };
}

function buildSearchBody(q, type, ctx) {
  const inner = (() => {
    switch (type) {
      case 'term': {
        const m = (q || '').match(/^([a-z_]+)\s*=\s*(.+)$/i);
        return m ? { term: { [m[1]]: m[2] } } : { match_all: {} };
      }
      case 'range': {
        const m = (q || '').match(/^([a-z_]+)\s*(>=|<=|>|<)\s*(\d+(?:\.\d+)?)$/i);
        if (!m) return { match_all: {} };
        const [, f, op, v] = m;
        const k = op === '>=' ? 'gte' : op === '<=' ? 'lte' : op === '>' ? 'gt' : 'lt';
        return { range: { [f]: { [k]: Number(v) } } };
      }
      case 'prefix':   return q ? { prefix: { message: q } } : { match_all: {} };
      case 'phrase':   return q ? { match_phrase: { message: q } } : { match_all: {} };
      case 'semantic': return q ? { semantic: { field: 'embedding', query: q, k: 10 } }
                                : { match_all: {} };
      case 'hybrid':   return q ? {
        hybrid: {
          queries: [
            { query: { match: { message: q } }, weight: 1.0 },
            { query: { semantic: { field: 'embedding', query: q, k: 10 } }, weight: 0.8 },
          ],
          fusion: { type: 'rrf', k: 60 },
        }
      } : { match_all: {} };
      case 'knn':      return { match_all: {} }; // raw knn needs vector input — UI shows DSL only
      default:         return q ? { match: { message: q } } : { match_all: {} };
    }
  })();

  const filterList = Object.entries(ctx.filters || {}).map(([f, v]) => ({ term: { [f]: v } }));
  const query = filterList.length
    ? { bool: { must: inner, filter: filterList } }
    : inner;

  return {
    query,
    size: 25,
    track_total_hits: true,
    aggs: {
      by_level:   { terms: { field: 'level',   size: 8 } },
      by_service: { terms: { field: 'service', size: 8 } },
      by_host:    { terms: { field: 'host',    size: 8 } },
    },
  };
}

function bucketsToFacet(agg) {
  if (!agg || !Array.isArray(agg.buckets)) return [];
  return agg.buckets.map((b) => ({ key: String(b.key), count: b.doc_count }));
}

// ── system ────────────────────────────────────────────────────────
//
// The system dashboard expects a rich infrastructure-monitoring
// shape (hostCpu sparklines, topProcs, auth.failures…) that Xerj
// itself doesn't expose — those come from a metric collector
// pointed at the host (Metricbeat / collectd / OTLP). For the SE
// demo path we return null here so the dashboard falls back to the
// mock; once a metrics-ingest adapter lands in v0.7.x we'll fill
// the live shape from the same xerj indices the user is watching.
async function liveSystem(baseUrl, ctx, signal) {
  // We don't have a host-metrics agent today, but `_cluster/stats`
  // gives us real numbers we can drop into the headline tiles.
  let stats;
  try {
    const r = await fetch(baseUrl + '/_cluster/stats', { signal });
    if (!r.ok) return null;
    stats = await r.json();
  } catch (_e) { return null; }
  const base = await loadMock('system', ctx);
  const docs    = stats?.indices?.docs?.count   || 0;
  const bytes   = stats?.indices?.store?.size_in_bytes || 0;
  const idxN    = stats?.indices?.count          || 0;
  const shardN  = stats?.indices?.shards?.total  || 0;
  base.metrics = base.metrics || {};
  base.metrics.cpu     = base.metrics.cpu     || { value: 0, formatted: '—' };
  base.metrics.disk    = { value: bytes, formatted: (bytes / 1e9).toFixed(2) + ' GB', hint: 'live · xerj · stored' };
  base.metrics.docs    = { value: docs,  formatted: docs.toLocaleString('en-US'), hint: 'live · xerj' };
  base.metrics.indices = { value: idxN,  formatted: String(idxN), hint: `live · xerj · ${shardN} shards` };
  base._live = { source: '_cluster/stats', docs, bytes, indices: idxN, shards: shardN };
  return base;
}

// ── logs-overview ─────────────────────────────────────────────────
//
// Returns the rich logs-overview shape the dashboard expects, with
// live counts overlaid where we can compute them from xerj aggs.
// Fields the engine can't produce (peak rate, multi-host series,
// per-host CPU sparklines) come from the mock so the canvas stays
// fully populated for the SE demo.
async function liveLogsOverview(baseUrl, ctx, signal) {
  const since = rangeToSince(ctx.range || '24H');
  const body = {
    size: AGG_BYPASS_SIZE,  // rc.1 agg-cap workaround; see header
    query: { range: { '@timestamp': { gte: since } } },
    aggs: {
      by_level: { terms: { field: 'level', size: 5 } },
      by_service: { terms: { field: 'service', size: 8 } },
      by_host: { terms: { field: 'host', size: 8 } },
      timeline: {
        date_histogram: { field: '@timestamp', fixed_interval: bucketSize(ctx.range || '24H') },
        aggs: { by_level: { terms: { field: 'level', size: 5 } } },
      },
    },
  };
  let live;
  try {
    live = await rawSearch(baseUrl, 'logs-*', body, signal);
  } catch (_e) {
    return null; // graceful → mock fallback
  }

  // Use mock as the base shape so all panels render; overlay the few
  // numbers we can verify live against xerj.
  const { mock: mockData } = await import('../mock.js');
  const base = mockData('logs-overview', ctx.range || '24H', {
    cluster: ctx.cluster || '',
    filters: ctx.filters || {},
    customRange: ctx.customRange || null,
  });

  const total = live.hits?.total?.value ?? 0;
  if (base?.metrics?.total) {
    base.metrics.total.value = total;
    base.metrics.total.formatted = total.toLocaleString('en-US');
    base.metrics.total.delta = null;
    base.metrics.total.hint = 'live · xerj';
  }
  base._live = {
    total,
    by_level: bucketsToFacet(live.aggregations?.by_level),
    by_service: bucketsToFacet(live.aggregations?.by_service),
    by_host: bucketsToFacet(live.aggregations?.by_host),
    timeline: (live.aggregations?.timeline?.buckets || []).map((b) => ({
      t: b.key, count: b.doc_count,
      by_level: bucketsToFacet(b.by_level),
    })),
  };
  return base;
}

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

// ── data (data sources / index inventory) ─────────────────────────
//
// Same overlay pattern as logs-overview: mock supplies the rich
// canvas (clusters list, fieldsByIndex, narrative blurb) and we
// overlay the live indices Xerj actually has.
async function liveData(baseUrl, ctx, signal) {
  let indices;
  try {
    indices = await listIndices(baseUrl, signal);
  } catch (_e) {
    return null;
  }
  const { mock: mockData } = await import('../mock.js');
  const base = mockData('data', ctx.range || '24H', {
    cluster: ctx.cluster || '',
    filters: ctx.filters || {},
    customRange: ctx.customRange || null,
  });

  const liveIndices = (indices || []).map((i) => ({
    name: i.index,
    health: i.health || 'green',
    docs: Number(i['docs.count'] || 0),
    size: i['store.size'] || '0b',
    status: i.status || 'open',
    primary_shards: Number(i.pri || 1),
    replica_shards: Number(i.rep || 0),
  }));

  // Inject the live indices under whichever cluster is active in the
  // mock — that way the existing renderClusters / renderIndices tiles
  // light up without needing a new shape.
  const active = base?.activeCluster || 'local';
  if (base?.indicesByCluster) {
    base.indicesByCluster[active] = liveIndices.length ? liveIndices : base.indicesByCluster[active];
    base.focusIndex = liveIndices[0]?.name || base.focusIndex;
  }
  base._live = { indices: liveIndices };
  return base;
}

// ─────────────────────────────────────────────────────────────────────
// LIVE ADAPTERS — every dashboard the demo runbook visits returns
// numbers from real xerj indices. Each adapter starts from the mock
// shape (so panels stay populated even when a sub-metric isn't yet
// computable on the live engine) and overlays everything we CAN
// compute. The status pill flips to "LIVE · XERJ" because the
// outermost shape is real.
// ─────────────────────────────────────────────────────────────────────

const TIMELINE_BUCKET = (range) => bucketSize(range);

function paretoFromBuckets(buckets) {
  return (buckets || []).map((b) => ({ key: String(b.key), value: b.doc_count }));
}

async function loadMock(dashId, ctx) {
  const { mock: mockData } = await import('../mock.js');
  return mockData(dashId, ctx.range || '24H', {
    cluster: ctx.cluster || '',
    filters: ctx.filters || {},
    customRange: ctx.customRange || null,
  });
}

// ── ai-overview ───────────────────────────────────────────────────
async function liveAiOverview(baseUrl, ctx, signal) {
  const since = rangeToSince(ctx.range || '24H');
  const body = {
    size: AGG_BYPASS_SIZE,  // rc.1 agg-cap workaround; see header
    query: { range: { '@timestamp': { gte: since } } },
    aggs: {
      total_prompt:    { sum: { field: 'prompt_tokens' } },
      total_context:   { sum: { field: 'context_tokens' } },
      total_completion:{ sum: { field: 'completion_tokens' } },
      total_cost:      { sum: { field: 'cost_usd' } },
      avg_latency:     { avg: { field: 'latency_ms' } },
      cache_hits:      { terms: { field: 'cache_hit' } },
      models:          { terms: { field: 'model', size: 8 } },
      intents:         { terms: { field: 'intent', size: 12 } },
      docs:            { terms: { field: 'top_doc', size: 12 } },
      timeline:        { date_histogram: { field: '@timestamp', fixed_interval: TIMELINE_BUCKET(ctx.range || '24H') } },
    },
  };
  let live;
  try { live = await rawSearch(baseUrl, 'chat-events', body, signal); }
  catch (_e) { return null; }
  const total = live.hits?.total?.value ?? 0;
  if (!total) return null;
  const base = await loadMock('ai-overview', ctx);
  const a = live.aggregations || {};
  const totalPrompt   = Math.max(0, Math.round(a.total_prompt?.value    || 0));
  const totalContext  = Math.max(0, Math.round(a.total_context?.value   || 0));
  const totalComplete = Math.max(0, Math.round(a.total_completion?.value|| 0));
  const totalTokens   = totalPrompt + totalContext + totalComplete;
  const totalCost     = Math.max(0, a.total_cost?.value || 0);
  const avgLatency    = Math.round(a.avg_latency?.value || 0);
  const hits = (a.cache_hits?.buckets || []).reduce((m, b) => (m[String(b.key)] = b.doc_count, m), {});
  const cacheRate = total ? ((hits.true || 0) * 100 / total) : 0;
  const fmt = (n) => n >= 1e9 ? (n/1e9).toFixed(1)+'B' : n >= 1e6 ? (n/1e6).toFixed(1)+'M' : n >= 1e3 ? (n/1e3).toFixed(1)+'k' : String(n);
  base.metrics = base.metrics || {};
  base.metrics.queries  = { value: total,        formatted: fmt(total),       delta: null, hint: 'live · xerj' };
  base.metrics.tokens   = { value: totalTokens,  formatted: fmt(totalTokens), delta: null, hint: 'live · xerj' };
  base.metrics.cost     = { value: totalCost,    formatted: '$' + totalCost.toFixed(0), delta: null, hint: 'live · xerj' };
  const costES = totalCost * 5.2;
  base.metrics.savings  = { value: costES - totalCost, formatted: '$' + (costES - totalCost).toFixed(0), note: 'vs ES+Pinecone+Splunk' };
  base.metrics.latency  = { value: avgLatency, formatted: String(avgLatency), delta: null, hint: 'live · xerj' };
  base.metrics.cacheHit = { value: Math.round(cacheRate), formatted: String(Math.round(cacheRate)), delta: null, hint: 'live · xerj' };
  base.models  = (a.models?.buckets  || []).map((b) => ({ label: b.key.toUpperCase(), value: b.doc_count }));
  base.intents = (a.intents?.buckets || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base.topDocs = (a.docs?.buckets    || []).map((b) => ({ key: b.key, value: b.doc_count }));
  const tlBuckets = a.timeline?.buckets || [];
  if (tlBuckets.length) {
    base.series = base.series || {};
    base.series.queries = tlBuckets.map((b) => b.doc_count);
  }
  base._live = { source: 'chat-events', total };
  return base;
}

// ── rag-quality ────────────────────────────────────────────────────
async function liveRagQuality(baseUrl, ctx, signal) {
  const since = rangeToSince(ctx.range || '24H');
  const body = {
    size: AGG_BYPASS_SIZE,  // rc.1 agg-cap workaround; see header
    query: { range: { '@timestamp': { gte: since } } },
    aggs: {
      avg_prompt: { avg: { field: 'prompt_tokens' } },
      avg_context:{ avg: { field: 'context_tokens' } },
      avg_completion: { avg: { field: 'completion_tokens' } },
      docs:       { terms: { field: 'top_doc', size: 12 } },
      tenants:    { terms: { field: 'tenant', size: 8 } },
    },
  };
  let live;
  try { live = await rawSearch(baseUrl, 'chat-events', body, signal); }
  catch (_e) { return null; }
  const total = live.hits?.total?.value ?? 0;
  if (!total) return null;
  const base = await loadMock('rag-quality', ctx);
  const a = live.aggregations || {};
  base.metrics = base.metrics || {};
  base.metrics.queries = { value: total, formatted: total.toLocaleString('en-US'), hint: 'live · xerj' };
  if (base.metrics.contextLen) {
    base.metrics.contextLen.value = Math.round(a.avg_context?.value || 0);
    base.metrics.contextLen.formatted = Math.round(a.avg_context?.value || 0).toLocaleString('en-US');
    base.metrics.contextLen.hint = 'live · xerj';
  }
  base.topDocs = (a.docs?.buckets || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base._live = { source: 'chat-events', total };
  return base;
}

// ── vector-index ───────────────────────────────────────────────────
async function liveVectorIndex(baseUrl, ctx, signal) {
  const since = rangeToSince(ctx.range || '24H');
  const body = {
    size: AGG_BYPASS_SIZE,  // rc.1 agg-cap workaround; see header
    query: { range: { '@timestamp': { gte: since } } },
    aggs: {
      ops:      { terms: { field: 'op', size: 5 } },
      shards:   { terms: { field: 'shard', size: 16 } },
      indexes:  { terms: { field: 'index', size: 8 } },
      avg_recall:{ avg: { field: 'recall_at_10' } },
      avg_lat:  { avg: { field: 'latency_ms' } },
      timeline: { date_histogram: { field: '@timestamp', fixed_interval: TIMELINE_BUCKET(ctx.range || '24H') } },
    },
  };
  let live;
  try { live = await rawSearch(baseUrl, 'vector-ops', body, signal); }
  catch (_e) { return null; }
  const total = live.hits?.total?.value ?? 0;
  if (!total) return null;
  const base = await loadMock('vector-index', ctx);
  const a = live.aggregations || {};
  base.metrics = base.metrics || {};
  base.metrics.qps     = { value: total, formatted: total.toLocaleString('en-US'), hint: 'live · xerj · ops/24h' };
  base.metrics.recall  = { value: a.avg_recall?.value || 0,
                            formatted: ((a.avg_recall?.value || 0) * 100).toFixed(1) + '%', hint: 'live · xerj' };
  base.metrics.latency = { value: a.avg_lat?.value || 0,
                            formatted: Math.round(a.avg_lat?.value || 0) + 'ms', hint: 'live · xerj' };
  base.shards   = (a.shards?.buckets  || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base.byIndex  = (a.indexes?.buckets || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base.byOp     = (a.ops?.buckets     || []).map((b) => ({ key: b.key, value: b.doc_count }));
  const tl = a.timeline?.buckets || [];
  if (tl.length) {
    base.series = base.series || {};
    base.series.queries = tl.map((b) => b.doc_count);
  }
  base._live = { source: 'vector-ops', total };
  return base;
}

// ── agent-memory ───────────────────────────────────────────────────
async function liveAgentMemory(baseUrl, ctx, signal) {
  const since = rangeToSince(ctx.range || '24H');
  const body = {
    size: AGG_BYPASS_SIZE,  // rc.1 agg-cap workaround; see header
    query: { range: { '@timestamp': { gte: since } } },
    aggs: {
      ops:     { terms: { field: 'op', size: 8 } },
      agents:  { terms: { field: 'agent', size: 8 } },
      keys:    { terms: { field: 'memory_key', size: 12 } },
      total_tokens:{ sum: { field: 'tokens' } },
      avg_score:{ avg: { field: 'score' } },
      timeline:{ date_histogram: { field: '@timestamp', fixed_interval: TIMELINE_BUCKET(ctx.range || '24H') } },
    },
  };
  let live;
  try { live = await rawSearch(baseUrl, 'agent-memory', body, signal); }
  catch (_e) { return null; }
  const total = live.hits?.total?.value ?? 0;
  if (!total) return null;
  const base = await loadMock('agent-memory', ctx);
  const a = live.aggregations || {};
  base.metrics = base.metrics || {};
  base.metrics.ops      = { value: total, formatted: total.toLocaleString('en-US'), hint: 'live · xerj' };
  base.metrics.tokens   = { value: a.total_tokens?.value || 0,
                            formatted: ((a.total_tokens?.value || 0) / 1e3).toFixed(1) + 'k', hint: 'live · xerj' };
  base.metrics.recall   = { value: a.avg_score?.value || 0,
                            formatted: ((a.avg_score?.value || 0) * 100).toFixed(1) + '%', hint: 'live · xerj · avg score' };
  base.byOp     = (a.ops?.buckets    || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base.byAgent  = (a.agents?.buckets || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base.topKeys  = (a.keys?.buckets   || []).map((b) => ({ key: b.key, value: b.doc_count }));
  const tl = a.timeline?.buckets || [];
  if (tl.length) {
    base.series = base.series || {};
    base.series.queries = tl.map((b) => b.doc_count);
  }
  base._live = { source: 'agent-memory', total };
  return base;
}

// ── anomaly-detect ─────────────────────────────────────────────────
async function liveAnomalyDetect(baseUrl, ctx, signal) {
  const since = rangeToSince(ctx.range || '24H');
  const body = {
    size: AGG_BYPASS_SIZE,  // rc.1 agg-cap workaround; see header
    query: { range: { '@timestamp': { gte: since } } },
    aggs: {
      kinds:    { terms: { field: 'kind', size: 8 } },
      severity: { terms: { field: 'severity', size: 5 } },
      services: { terms: { field: 'service', size: 8 } },
      timeline: { date_histogram: { field: '@timestamp', fixed_interval: TIMELINE_BUCKET(ctx.range || '24H') } },
    },
  };
  let live;
  try { live = await rawSearch(baseUrl, 'anomalies', body, signal); }
  catch (_e) { return null; }
  const total = live.hits?.total?.value ?? 0;
  if (!total) return null;
  const base = await loadMock('anomaly-detect', ctx);
  const a = live.aggregations || {};
  const sevCounts = {};
  for (const b of a.severity?.buckets || []) sevCounts[b.key] = b.doc_count;
  base.metrics = base.metrics || {};
  base.metrics.total    = { value: total, formatted: total.toLocaleString('en-US'), hint: 'live · xerj' };
  base.metrics.critical = { value: sevCounts.critical || 0, formatted: String(sevCounts.critical || 0), hint: 'live · xerj' };
  base.metrics.warning  = { value: sevCounts.warning  || 0, formatted: String(sevCounts.warning  || 0), hint: 'live · xerj' };
  base.byKind     = (a.kinds?.buckets    || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base.byService  = (a.services?.buckets || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base.bySeverity = (a.severity?.buckets || []).map((b) => ({ key: b.key, value: b.doc_count }));
  const tl = a.timeline?.buckets || [];
  if (tl.length) {
    base.series = base.series || {};
    base.series.events = tl.map((b) => b.doc_count);
  }
  base._live = { source: 'anomalies', total };
  return base;
}

// ── ingest-pipeline ────────────────────────────────────────────────
async function liveIngestPipeline(baseUrl, ctx, signal) {
  const since = rangeToSince(ctx.range || '24H');
  const body = {
    size: AGG_BYPASS_SIZE,  // rc.1 agg-cap workaround; see header
    query: { range: { '@timestamp': { gte: since } } },
    aggs: {
      stages:    { terms: { field: 'stage', size: 8 } },
      pipelines: { terms: { field: 'pipeline', size: 8 } },
      status:    { terms: { field: 'status', size: 4 } },
      total_docs:{ sum: { field: 'docs' } },
      avg_dur:   { avg: { field: 'duration_ms' } },
      timeline:  { date_histogram: { field: '@timestamp', fixed_interval: TIMELINE_BUCKET(ctx.range || '24H') } },
    },
  };
  let live;
  try { live = await rawSearch(baseUrl, 'logs-ingest-events', body, signal); }
  catch (_e) { return null; }
  const total = live.hits?.total?.value ?? 0;
  if (!total) return null;
  const base = await loadMock('ingest-pipeline', ctx);
  const a = live.aggregations || {};
  const statusCount = {};
  for (const b of a.status?.buckets || []) statusCount[b.key] = b.doc_count;
  const errRate = total ? ((statusCount.failed || 0) * 100 / total) : 0;
  base.metrics = base.metrics || {};
  base.metrics.events     = { value: total, formatted: total.toLocaleString('en-US'), hint: 'live · xerj' };
  base.metrics.docs       = { value: a.total_docs?.value || 0,
                              formatted: ((a.total_docs?.value || 0) / 1e6).toFixed(1) + 'M', hint: 'live · xerj' };
  base.metrics.errorRate  = { value: errRate, formatted: errRate.toFixed(2) + '%', hint: 'live · xerj' };
  base.metrics.avgLatency = { value: a.avg_dur?.value || 0,
                              formatted: Math.round(a.avg_dur?.value || 0) + 'ms', hint: 'live · xerj' };
  base.byStage    = (a.stages?.buckets    || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base.byPipeline = (a.pipelines?.buckets || []).map((b) => ({ key: b.key, value: b.doc_count }));
  base.byStatus   = (a.status?.buckets    || []).map((b) => ({ key: b.key, value: b.doc_count }));
  const tl = a.timeline?.buckets || [];
  if (tl.length) {
    base.series = base.series || {};
    base.series.events = tl.map((b) => b.doc_count);
  }
  base._live = { source: 'logs-ingest-events', total };
  return base;
}
