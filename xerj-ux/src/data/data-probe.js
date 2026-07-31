// ============================================================
// XERJ Console — "which data classes does this engine hold?" probe
//
// Sibling to brains-probe.js. The AI / logs / vector / memory dashboards each
// fall back to FULL MOCK data when their backing index has zero rows (see
// data/backends/xerj.js: `if (!total) return null` → query.js → mock.js). So a
// fresh or brain-only engine that advertised those dashboards would be showing
// fabricated numbers behind a real-looking UI. This probe answers, per data
// class, "does the engine actually hold this?" so app.js can hide the nav entry
// until it does — exactly the pattern secondBrain already uses via brains-probe.
//
// One cheap `_cat/indices` call, classified client-side by the index-name
// literals the dashboards query (data/backends/xerj.js). Fail-CLOSED: absent on
// any transport failure or non-OK status, so we never CLAIM data we can't prove.
//
// Cached per baseUrl with a short TTL (same as brains-probe) so the boot probe
// and periodic re-probes don't hammer `_cat`, and a backend switch re-probes.
// ============================================================

const TTL_MS = 30_000;

/** baseUrl → { at: epoch-ms, value: features } */
const cache = new Map();

/** The data-class feature keys, all false. Also the shape app.js merges into
 *  state.liveFeatures, and the safe answer on any failure. */
export function emptyDataFeatures() {
  return {
    'chat-events': false,
    'vector-ops': false,
    'agent-memory': false,
    'anomalies': false,
    'logs-ingest-events': false,
    'logs': false,
  };
}

/** Classify a list of index names into data-class presence flags. The literals
 *  mirror the hardcoded `/<index>/_search` targets in data/backends/xerj.js. */
export function classifyIndices(names) {
  const has = (n) => names.includes(n);
  const anyPrefix = (p) => names.some((n) => n.startsWith(p));
  return {
    'chat-events':        has('chat-events'),        // ai-overview, rag-quality
    'vector-ops':         has('vector-ops'),          // vector-index
    'agent-memory':       has('agent-memory'),        // agent-memory
    'anomalies':          has('anomalies'),           // anomaly-detect
    'logs-ingest-events': has('logs-ingest-events'),  // ingest-pipeline
    'logs':               anyPrefix('logs-'),         // logs-overview (logs-*)
  };
}

/**
 * Return which data classes the engine at `baseUrl` holds. Never throws;
 * returns all-false on transport failure, non-OK status, or empty baseUrl.
 */
export async function dataFeaturesPresent(baseUrl, signal) {
  const base = (baseUrl || '').replace(/\/+$/, '');
  if (!base) return emptyDataFeatures();

  const hit = cache.get(base);
  if (hit && Date.now() - hit.at < TTL_MS) return hit.value;

  let value = emptyDataFeatures();
  try {
    // The data-class indices (chat-events, vector-ops, …) are non-dot, so the
    // default `_cat/indices` lists them; no expand_wildcards needed.
    const r = await fetch(`${base}/_cat/indices`, {
      signal,
      headers: { accept: 'text/plain, application/json' },
    });
    if (r.ok) {
      value = classifyIndices(parseIndexNames(await r.text()));
    }
  } catch {
    value = emptyDataFeatures(); // engine down / CORS / abort — no claim
  }

  cache.set(base, { at: Date.now(), value });
  return value;
}

/**
 * Parse a `_cat/indices` body into index names. This engine emits plain text
 * lines (`health status NAME uuid …`); tolerate a JSON array in case a later
 * build adds `?format=json`. Exported for the node self-test.
 */
export function parseIndexNames(text) {
  const trimmed = (text || '').trim();
  if (!trimmed) return [];
  if (trimmed.startsWith('[')) {
    try { return JSON.parse(trimmed).map((i) => i.index).filter(Boolean); } catch { return []; }
  }
  return trimmed.split('\n')
    .map((l) => l.trim().split(/\s+/)[2])
    .filter(Boolean);
}
