// ============================================================
// XERJ.ai — Data source catalog (mock)
//
// Shape that mirrors the live XERJ API responses:
//   GET /v1/clusters                        → [{ id, name, url, status }]
//   GET /v1/clusters/{id}/indices           → [{ name, docs, bytes, shards }]
//   GET /v1/indices/{name}/_mapping         → { fields: [...] }
//   GET /v1/indices/{name}/encodings        → { per-field encoding/ratio }
//
// When the backend is ready, swap the three exported functions
// for real fetch() calls. Dashboards and the MANAGE view never
// touch HTTP — they go through this module.
// ============================================================

const MOCK_CLUSTERS = [
  { id: 'local',    name: 'LOCAL',    url: 'http://localhost:8080', status: 'green', version: '0.1.0', indices: 6, docs: 1_24_00_000 },
  { id: 'prod-us',  name: 'PROD-US',  url: 'https://xerj-us-east-1.internal', status: 'green', version: '0.1.0', indices: 14, docs: 840_00_00_000 },
  { id: 'prod-eu',  name: 'PROD-EU',  url: 'https://xerj-eu-central-1.internal', status: 'yellow', version: '0.1.0', indices: 14, docs: 512_00_00_000 },
  { id: 'staging',  name: 'STAGING',  url: 'https://xerj-staging.internal', status: 'green', version: '0.2.0-rc1', indices: 8, docs: 22_00_00_000 },
];

const MOCK_INDICES = {
  local: [
    { name: 'logs-prod',    docs: 42_000_000, bytes: 8_400_000_000, shards: 4, replicas: 0, retention_days: 30 },
    { name: 'logs-stage',   docs:  3_200_000, bytes: 640_000_000, shards: 2, replicas: 0, retention_days: 7 },
    { name: 'traces',       docs: 18_000_000, bytes: 3_600_000_000, shards: 4, replicas: 0, retention_days: 14 },
    { name: 'docs',         docs:     58_000, bytes: 120_000_000, shards: 1, replicas: 0, retention_days: null },
    { name: 'metrics',      docs: 22_000_000, bytes: 1_100_000_000, shards: 2, replicas: 0, retention_days: 90 },
    { name: 'events',       docs:  8_200_000, bytes: 820_000_000, shards: 2, replicas: 0, retention_days: 30 },
  ],
  'prod-us': [
    { name: 'logs-prod',       docs: 12_400_000_000, bytes: 2_480_000_000_000, shards: 32, replicas: 1, retention_days: 90 },
    { name: 'logs-edge',       docs:  4_200_000_000, bytes:   840_000_000_000, shards: 16, replicas: 1, retention_days: 30 },
    { name: 'traces',          docs:  6_800_000_000, bytes: 1_360_000_000_000, shards: 16, replicas: 1, retention_days: 14 },
    { name: 'embeddings',      docs:  1_200_000_000, bytes:   240_000_000_000, shards:  8, replicas: 1, retention_days: null },
    { name: 'agent-memory',    docs:     48_000_000, bytes:    9_600_000_000, shards:  4, replicas: 1, retention_days: null },
    { name: 'rag-chunks',      docs:    320_000_000, bytes:   64_000_000_000, shards:  8, replicas: 1, retention_days: null },
    { name: 'alerts',          docs:     12_000_000, bytes:    2_400_000_000, shards:  2, replicas: 1, retention_days: 180 },
    { name: 'audit',           docs:     18_000_000, bytes:    3_600_000_000, shards:  2, replicas: 1, retention_days: 365 },
    { name: 'metrics',         docs:  2_400_000_000, bytes:   480_000_000_000, shards:  8, replicas: 1, retention_days: 90 },
    { name: 'events',          docs:    860_000_000, bytes:  172_000_000_000, shards:  8, replicas: 1, retention_days: 30 },
  ],
};

// Per-index field catalog (mock). For each index a list of
// { name, type, docValues, indexed, cardinality, encoding, ratio }.
const MOCK_FIELDS = {
  'logs-prod': [
    { name: '@timestamp',  type: 'date',    indexed: true, cardinality: 42_000_000, encoding: 'Δ-of-Δ',   ratio: 0.02 },
    { name: 'service',     type: 'keyword', indexed: true, cardinality:         12, encoding: 'DICT',      ratio: 0.05 },
    { name: 'level',       type: 'keyword', indexed: true, cardinality:          5, encoding: 'DICT',      ratio: 0.04 },
    { name: 'host',        type: 'keyword', indexed: true, cardinality:         48, encoding: 'DICT',      ratio: 0.07 },
    { name: 'message',     type: 'text',    indexed: true, cardinality: 38_200_000, encoding: 'ZSTD+TMPL', ratio: 0.31 },
    { name: 'trace_id',    type: 'keyword', indexed: true, cardinality: 12_400_000, encoding: 'UVARINT',   ratio: 0.44 },
    { name: 'span_id',     type: 'keyword', indexed: true, cardinality: 12_400_000, encoding: 'UVARINT',   ratio: 0.44 },
    { name: 'latency_ms',  type: 'integer', indexed: true, cardinality:      2_400, encoding: 'FOR+RLE',   ratio: 0.18 },
    { name: 'status',      type: 'integer', indexed: true, cardinality:         48, encoding: 'DICT',      ratio: 0.06 },
    { name: 'bytes_out',   type: 'integer', indexed: true, cardinality:     84_000, encoding: 'FOR',       ratio: 0.22 },
    { name: 'client_ip',   type: 'ip',      indexed: true, cardinality:  3_400_000, encoding: 'RAW',       ratio: 0.62 },
    { name: 'user_agent',  type: 'text',    indexed: true, cardinality:     18_000, encoding: 'DICT+ZSTD', ratio: 0.28 },
  ],
  traces: [
    { name: '@timestamp',       type: 'date',    indexed: true, cardinality: 18_000_000, encoding: 'Δ-of-Δ', ratio: 0.02 },
    { name: 'service.name',     type: 'keyword', indexed: true, cardinality:        42, encoding: 'DICT',    ratio: 0.05 },
    { name: 'operation.name',   type: 'keyword', indexed: true, cardinality:       820, encoding: 'DICT',    ratio: 0.08 },
    { name: 'duration_us',      type: 'long',    indexed: true, cardinality:    42_000, encoding: 'FOR+RLE', ratio: 0.14 },
    { name: 'trace_id',         type: 'keyword', indexed: true, cardinality: 12_400_000, encoding: 'UVARINT', ratio: 0.44 },
    { name: 'span_id',          type: 'keyword', indexed: true, cardinality: 18_000_000, encoding: 'UVARINT', ratio: 0.44 },
    { name: 'parent_span_id',   type: 'keyword', indexed: true, cardinality: 14_000_000, encoding: 'UVARINT', ratio: 0.44 },
    { name: 'http.status_code', type: 'integer', indexed: true, cardinality:        48, encoding: 'DICT',    ratio: 0.06 },
  ],
  embeddings: [
    { name: '@timestamp',  type: 'date',         indexed: true,  cardinality: 1_200_000_000, encoding: 'Δ-of-Δ',  ratio: 0.02 },
    { name: 'doc_id',      type: 'keyword',      indexed: true,  cardinality: 1_200_000_000, encoding: 'UVARINT', ratio: 0.44 },
    { name: 'chunk_id',    type: 'keyword',      indexed: true,  cardinality: 1_200_000_000, encoding: 'UVARINT', ratio: 0.44 },
    { name: 'text',        type: 'text',         indexed: true,  cardinality: 1_100_000_000, encoding: 'ZSTD',    ratio: 0.18 },
    { name: 'embedding',   type: 'dense_vector', indexed: true,  cardinality: null,          encoding: 'HNSW+SQ8', ratio: 0.22 },
    { name: 'model',       type: 'keyword',      indexed: true,  cardinality:            6,  encoding: 'DICT',    ratio: 0.04 },
    { name: 'dim',         type: 'integer',      indexed: false, cardinality:            4,  encoding: 'DICT',    ratio: 0.04 },
  ],
  'rag-chunks': [
    { name: '@timestamp',  type: 'date',    indexed: true, cardinality: 320_000_000, encoding: 'Δ-of-Δ', ratio: 0.02 },
    { name: 'source_uri',  type: 'keyword', indexed: true, cardinality:  2_400_000, encoding: 'DICT',    ratio: 0.14 },
    { name: 'chunk_idx',   type: 'integer', indexed: true, cardinality:     12_000, encoding: 'FOR',     ratio: 0.12 },
    { name: 'text',        type: 'text',    indexed: true, cardinality: 280_000_000, encoding: 'ZSTD',    ratio: 0.20 },
    { name: 'title',       type: 'text',    indexed: true, cardinality:  2_400_000, encoding: 'DICT+ZSTD', ratio: 0.18 },
    { name: 'parent_id',   type: 'keyword', indexed: true, cardinality:  2_400_000, encoding: 'UVARINT', ratio: 0.44 },
    { name: 'tags',        type: 'keyword', indexed: true, cardinality:     12_000, encoding: 'DICT',    ratio: 0.10 },
  ],
  'agent-memory': [
    { name: '@timestamp',   type: 'date',    indexed: true, cardinality: 48_000_000, encoding: 'Δ-of-Δ', ratio: 0.02 },
    { name: 'agent',        type: 'keyword', indexed: true, cardinality:          8, encoding: 'DICT',    ratio: 0.04 },
    { name: 'op',           type: 'keyword', indexed: true, cardinality:          5, encoding: 'DICT',    ratio: 0.04 },
    { name: 'key',          type: 'keyword', indexed: true, cardinality:  2_400_000, encoding: 'UVARINT', ratio: 0.44 },
    { name: 'embedding',    type: 'dense_vector', indexed: true, cardinality: null, encoding: 'HNSW+SQ8', ratio: 0.22 },
    { name: 'score',        type: 'float',   indexed: true, cardinality:      1_000, encoding: 'FOR+RLE', ratio: 0.18 },
    { name: 'dedup_of',     type: 'keyword', indexed: true, cardinality:    860_000, encoding: 'UVARINT', ratio: 0.44 },
  ],
};

// ── live wiring ─────────────────────────────────────────────────────
// `local` cluster maps to the running xerj on the same origin. We
// keep MOCK_CLUSTERS for the prod-us / prod-eu / staging entries so
// the selector still has multi-cluster shape, but the LOCAL cluster
// flips to live `_cat/indices` + `_mapping` calls so the demo shows
// "the engine actually has these indices" instead of seeded names.

function liveBaseUrl() {
  if (typeof window !== 'undefined' && window.location && window.location.origin) {
    return window.location.origin;
  }
  return 'http://localhost:9200';
}

// ── Live wiring via Xerj Console backend ──────────────────────────────────────────
// The SPA fetches connections, indices, and fields through Xerj Console's typed
// surface (`/_xerj-console/api/v1/data-sources/...`) which carries the session
// cookie automatically.  Falls back to the engine's ES-compat endpoints
// (`/_cluster/stats`, `/_cat/indices`, `/{idx}/_mapping`) when Xerj Console is
// unreachable so the SPA still shows numbers if the user lands here
// before logging in (or in dev when xerj-console-api isn't mounted).

const XAPI = '/_xerj-console/api/v1';

async function liveConnections() {
  try {
    const r = await fetch(XAPI + '/data-sources/connections', { credentials: 'same-origin' });
    if (!r.ok) return null;
    const body = await r.json();
    return body?.data?.connections || null;
  } catch (_e) { return null; }
}

async function liveClusterStats() {
  // Prefer the Xerj Console connection list (one row per data source) — every
  // row already carries the engine's status flag from xerj-console-api.
  const conns = await liveConnections();
  if (conns && conns.length) {
    return { __via_xerj_console: true, connections: conns };
  }
  try {
    const r = await fetch(liveBaseUrl() + '/_cluster/stats', { credentials: 'same-origin' });
    if (!r.ok) return null;
    return await r.json();
  } catch (_e) { return null; }
}

async function liveIndices() {
  // 1) Xerj Console facade — the auth-gated path that is what the binary
  //    actually documents.  Returns clean { indices: [...] }.
  try {
    const r = await fetch(XAPI + '/data-sources/connections/built-in/indices', {
      credentials: 'same-origin',
    });
    if (r.ok) {
      const body = await r.json();
      const list = body?.data?.indices || [];
      return list.map((it) => ({
        name: it.name,
        docs: Number(it.docs || 0),
        bytes: 0,                 // bytes not surfaced by phase-3 facade yet
        shards: Number(it.shards || 1),
        replicas: Number(it.replicas || 0),
        segments: Number(it.segments || 0),
        fields: Number(it.fields || 0),
        health: 'green',
        status: 'open',
        retention_days: null,
      }));
    }
  } catch (_e) { /* fall through to ES-compat */ }

  // 2) ES-compat fallback for unauthenticated dev / pre-bootstrap.
  try {
    const r = await fetch(liveBaseUrl() + '/_cat/indices', { credentials: 'same-origin' });
    if (!r.ok) return null;
    const txt = await r.text();
    const out = [];
    for (const line of txt.split('\n')) {
      // ES `_cat/indices` columns:
      //   health  status  index  uuid  pri  rep  docs.count  docs.deleted  store.size  pri.store.size
      const cols = line.trim().split(/\s+/);
      if (cols.length < 8) continue;
      const [health, status, name, , pri, rep, docsCount, , storeSize] = cols;
      if (!name || name === 'index') continue;
      // Skip system indices (.xerj_*) — they're plumbing, not user data.
      if (name.startsWith('.xerj_')) continue;
      out.push({
        name,
        docs: Number(docsCount || 0),
        bytes: parseSize(storeSize || '0b'),
        shards: Number(pri || 1),
        replicas: Number(rep || 0),
        health: health || 'green',
        status: status || 'open',
        retention_days: null,
      });
    }
    return out;
  } catch (_e) { return null; }
}

async function liveFields(indexName) {
  // Xerj Console facade first.
  try {
    const r = await fetch(
      XAPI + '/data-sources/connections/built-in/indices/'
        + encodeURIComponent(indexName) + '/fields',
      { credentials: 'same-origin' },
    );
    if (r.ok) {
      const body = await r.json();
      const list = body?.data?.fields || [];
      return list.map((f) => ({
        name: f.name,
        type: f.type || 'object',
        indexed: f.indexed !== false,
        doc_values: f.doc_values !== false,
        cardinality: null,
        encoding: encodingFor(f.type),
        ratio: null,
      }));
    }
  } catch (_e) { /* fall through */ }

  // ES-compat mapping fallback.
  try {
    const r = await fetch(liveBaseUrl() + '/' + encodeURIComponent(indexName) + '/_mapping', {
      credentials: 'same-origin',
    });
    if (!r.ok) return null;
    const j = await r.json();
    const props = j?.[indexName]?.mappings?.properties || {};
    return Object.entries(props).map(([name, cfg]) => ({
      name,
      type: (cfg && cfg.type) || 'object',
      indexed: !(cfg && cfg.index === false),
      cardinality: null,
      encoding: encodingFor(cfg && cfg.type),
      ratio: null,
    }));
  } catch (_e) { return null; }
}

function parseSize(s) {
  if (!s) return 0;
  const m = String(s).match(/^([0-9.]+)\s*([kmgt]?b)$/i);
  if (!m) return Number(s) || 0;
  const n = parseFloat(m[1]);
  const u = m[2].toLowerCase();
  return u === 'b' ? n
       : u === 'kb' ? n * 1024
       : u === 'mb' ? n * 1024 * 1024
       : u === 'gb' ? n * 1024 * 1024 * 1024
       : u === 'tb' ? n * 1024 * 1024 * 1024 * 1024
       : n;
}
function encodingFor(t) {
  switch (t) {
    case 'date':         return 'Δ-of-Δ';
    case 'date_nanos':   return 'Δ-of-Δ';
    case 'keyword':      return 'DICT';
    case 'text':         return 'ZSTD+TMPL';
    case 'integer':
    case 'short':
    case 'byte':
    case 'long':         return 'FOR';
    case 'float':
    case 'double':
    case 'half_float':
    case 'scaled_float': return 'FOR+RLE';
    case 'dense_vector': return 'HNSW+SQ8';
    case 'ip':           return 'RAW';
    case 'boolean':      return 'BIT';
    default:             return '—';
  }
}

/** List all configured clusters. After login, the LOCAL row reflects
 *  the real `built-in` connection shape from
 *  /_xerj-console/api/v1/data-sources/connections. Other rows stay seeded so
 *  the multi-cluster picker has shape (those external connections are
 *  not implemented yet — they'll come from /data-sources/connections
 *  too once the HTTP-shaped adapters land). */
export async function listClusters() {
  const stats = await liveClusterStats();
  if (!stats) return MOCK_CLUSTERS;

  // Xerj Console connections path — replace every mock row whose id matches
  // a real connection's id, and keep illustrative mock rows for the
  // ones we don't have adapters for yet.
  if (stats.__via_xerj_console) {
    const conns = stats.connections || [];
    const out = MOCK_CLUSTERS.map((c) => ({ ...c }));
    // Map `built-in` → the LOCAL row.
    const builtIn = conns.find((c) => c.id === 'built-in');
    if (builtIn) {
      const local = out.find((c) => c.id === 'local');
      if (local) {
        local.url     = liveBaseUrl();
        local.status  = builtIn.status || 'green';
        local.name    = (builtIn.name || 'LOCAL').toUpperCase();
        local.kind    = builtIn.kind;
      }
    }
    // Future: append additional connection rows from `conns` once the
    // HTTP-shaped adapters return non-501 from the indices endpoint.
    return out;
  }

  // ES-compat /_cluster/stats fallback (pre-login).
  const out = MOCK_CLUSTERS.map((c) => ({ ...c }));
  const local = out.find((c) => c.id === 'local');
  if (local) {
    local.indices = stats.indices?.count || local.indices;
    local.docs    = stats.indices?.docs?.count || local.docs;
    local.url     = liveBaseUrl();
    local.status  = stats.status || 'green';
    local.version = stats.nodes?.versions?.[0] || local.version;
  }
  return out;
}

export function listClustersSync() {
  return MOCK_CLUSTERS;
}

/** List indices in a cluster. LOCAL → live `_cat/indices`. */
export async function listIndices(clusterId) {
  if (clusterId === 'local') {
    const live = await liveIndices();
    if (live && live.length) return live;
  }
  return MOCK_INDICES[clusterId] || [];
}

/** List fields in an index. For live indices, fetch the mapping. */
export async function listFields(indexName) {
  if (MOCK_FIELDS[indexName]) return MOCK_FIELDS[indexName];
  const live = await liveFields(indexName);
  return live || [];
}

/** Return the default cluster id the app should talk to. */
export function defaultClusterId() {
  return localStorage.getItem('xerj.cluster') || 'local';
}
export function setDefaultCluster(id) {
  localStorage.setItem('xerj.cluster', id);
}
