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

/** List all configured clusters. */
export async function listClusters() {
  return MOCK_CLUSTERS;
}

/** Synchronous snapshot of the cluster list — used by the header
 *  cluster selector where we don't want to await on every render. */
export function listClustersSync() {
  return MOCK_CLUSTERS;
}

/** List indices in a cluster. */
export async function listIndices(clusterId) {
  return MOCK_INDICES[clusterId] || [];
}

/** List fields in an index (from its mapping). */
export async function listFields(indexName) {
  return MOCK_FIELDS[indexName] || [];
}

/** Return the default cluster id the app should talk to. */
export function defaultClusterId() {
  return localStorage.getItem('xerj.cluster') || 'local';
}
export function setDefaultCluster(id) {
  localStorage.setItem('xerj.cluster', id);
}
