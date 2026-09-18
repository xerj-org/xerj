// ============================================================
// XERJ.ai — Synthetic data source
//
// Deterministic per (dashboardId, timeRange). Feeds every
// dashboard today; the shape is what a real XERJ.ai server will
// return through /xerj/dashboards/{id}.
//
// Swap in data/query.js.
// ============================================================

// ---------- seeded PRNG (mulberry32) ----------------------
const rng = (seed) => {
  let t = seed >>> 0;
  return () => {
    t = (t + 0x6D2B79F5) >>> 0;
    let r = t;
    r = Math.imul(r ^ (r >>> 15), r | 1);
    r ^= r + Math.imul(r ^ (r >>> 7), r | 61);
    return ((r ^ (r >>> 14)) >>> 0) / 4294967296;
  };
};
const hashStr = (s) => {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); }
  return h >>> 0;
};

// ---------- shape helpers ---------------------------------
const POINTS = { '1H': 60, '24H': 96, '7D': 168, '30D': 180, '90D': 180 };
const points = (r) => POINTS[r] ?? 96;

const HOURS = { '1H': 1, '24H': 24, '7D': 168, '30D': 720, '90D': 2160 };
const hours = (r) => HOURS[r] ?? 24;

const diurnal = (n, base, { peakHour = 14, amplitude = 0.75, noise = 0.12, rand } = {}) => {
  const out = new Array(n);
  const span = hours(rand.range ?? '24H');
  for (let i = 0; i < n; i++) {
    const hour = (i / (n - 1)) * span;
    const phase = Math.cos(((hour - peakHour) / 24) * 2 * Math.PI);
    const n1 = (rand() - 0.5) * 2 * noise;
    out[i] = Math.max(0, base * (1 + amplitude * phase + n1));
  }
  return out;
};

const pareto = (labels, total, { alpha = 1.05, rand }) => {
  const raw = labels.map((_, i) => (1 / Math.pow(i + 1, alpha)) * (0.85 + rand() * 0.3));
  const sum = raw.reduce((a, b) => a + b, 0);
  return labels.map((label, i) => ({ label, value: Math.round((raw[i] / sum) * total) }));
};

const sumOf = (arr) => arr.reduce((a, b) => a + b, 0);

const peakLabel = (values) => {
  let idx = 0, max = -Infinity;
  for (let i = 0; i < values.length; i++) if (values[i] > max) { max = values[i]; idx = i; }
  const frac = idx / (values.length - 1);
  const h = Math.floor(frac * 24);
  const m = Math.floor(((frac * 24) - h) * 60);
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
};

const compact = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 2 });

const rangeLabels = (range) => {
  switch (range) {
    case '1H':  return ['−60 MIN', 'NOW'];
    case '24H': return ['00:00', '24:00'];
    case '7D':  return ['MON', 'SUN'];
    case '30D': return ['D−30', 'TODAY'];
    case '90D': return ['Q START', 'TODAY'];
    default:    return ['', ''];
  }
};

// ==========================================================
// AI OVERVIEW  — the flagship
// ==========================================================
const buildAiOverview = (rand, range) => {
  const n = points(range);
  const base = 780 + rand() * 220;
  const queries = diurnal(n, base, { rand, peakHour: 13 });
  const totalQueries = Math.round(sumOf(queries));

  // Token counts — prompt grows with context, completion is steadier.
  const promptTok = queries.map((q) => q * (1800 + rand() * 600));
  const ctxTok    = queries.map((q) => q * (9000 + rand() * 4500));
  const outTok    = queries.map((q) => q * (320 + rand() * 180));
  const cacheHit  = queries.map(() => 0.38 + rand() * 0.18);

  const totalPromptT = Math.round(sumOf(promptTok));
  const totalCtxT    = Math.round(sumOf(ctxTok));
  const totalOutT    = Math.round(sumOf(outTok));
  const totalT       = totalPromptT + totalCtxT + totalOutT;

  // Cost: $/million tokens blended
  const cost = (totalPromptT / 1e6) * 2.5 + (totalCtxT / 1e6) * 0.2 + (totalOutT / 1e6) * 10;
  const costES = cost * 5.2; // what this would cost on the ES + proprietary stack

  const models = [
    { label: 'OPUS 4.6',    value: Math.round(totalQueries * 0.22) },
    { label: 'SONNET 4.6',  value: Math.round(totalQueries * 0.35) },
    { label: 'HAIKU 4.5',   value: Math.round(totalQueries * 0.28) },
    { label: 'GPT-5',       value: Math.round(totalQueries * 0.09) },
    { label: 'GEMINI 3',    value: Math.round(totalQueries * 0.04) },
    { label: 'LLAMA 4',     value: Math.round(totalQueries * 0.02) },
  ];

  const intents = pareto(
    ['semantic search','code-assist','doc-q&a','summarize','translate','classify',
     'extract-json','agent-tool','rerank','rewrite','chat-freeform','safety-check'],
    totalQueries, { alpha: 0.85, rand }
  );

  const topDocs = pareto(
    ['runbook/oncall.md','rfc/042-retention.md','arch/cluster-design.md',
     'rfc/039-hybrid-search.md','runbook/incident-1411.md','policy/pii.md',
     'arch/hnsw-internals.md','rfc/051-agent-memory.md','docs/query-dsl.md',
     'rfc/048-embed-proxy.md','runbook/billing-sync.md','docs/ingest-api.md'],
    Math.round(totalQueries * 0.7), { alpha: 0.75, rand }
  );

  // Latency percentiles per model as ribbon-3d series
  const latencyRibbons = [
    { label: 'OPUS 4.6',   values: queries.map(() => 1450 + rand() * 220) },
    { label: 'SONNET 4.6', values: queries.map(() => 820  + rand() * 140) },
    { label: 'HAIKU 4.5',  values: queries.map(() => 310  + rand() * 80) },
    { label: 'GPT-5',      values: queries.map(() => 990  + rand() * 190) },
    { label: 'GEMINI 3',   values: queries.map(() => 1120 + rand() * 240) },
  ];

  const costHeatmap = {
    cols: Array.from({ length: 12 }, (_, i) => String(i * 2).padStart(2, '0')),
    matrix: ['MON','TUE','WED','THU','FRI','SAT','SUN'].map((_, di) =>
      Array.from({ length: 12 }, (_, hi) => {
        const h = hi * 2;
        const phase = Math.cos(((h - 13) / 24) * 2 * Math.PI);
        const weekend = di >= 5 ? 0.48 : 1;
        return Math.max(0.2, cost / 150 * weekend * (1 + 0.8 * phase) * (0.85 + rand() * 0.3));
      })
    ),
  };

  const flowSegments = [
    { label: 'SYS PROMPT', value: totalPromptT },
    { label: 'CONTEXT',    value: totalCtxT },
    { label: 'COMPLETION', value: totalOutT },
  ];

  return {
    metrics: {
      queries:   { value: totalQueries, formatted: compact.format(totalQueries), delta: (rand() - 0.3) * 12 },
      tokens:    { value: totalT, formatted: compact.format(totalT), delta: (rand() - 0.35) * 8 },
      cost:      { value: cost, formatted: '$' + cost.toFixed(0), delta: (rand() - 0.65) * 18 },
      savings:   { value: costES - cost, formatted: '$' + (costES - cost).toFixed(0), note: 'vs. ES+Pinecone+Splunk' },
      latency:   { value: 820, formatted: '820', delta: (rand() - 0.55) * 8 },
      cacheHit:  { value: 42, formatted: '42', delta: (rand() - 0.35) * 6 },
    },
    series: {
      queries,
      prompt:  promptTok.map((v) => v / (1e3)),
      context: ctxTok.map((v) => v / (1e3)),
      output:  outTok.map((v) => v / (1e3)),
      startLabel: rangeLabels(range)[0],
      endLabel:   rangeLabels(range)[1],
    },
    models,
    intents,
    topDocs,
    latencyRibbons,
    costHeatmap,
    flowSegments,
  };
};

// ==========================================================
// RAG QUALITY  — the unsolved observability problem
// ==========================================================
const buildRagQuality = (rand, range) => {
  const n = points(range);
  const grounding = diurnal(n, 88, { rand, amplitude: 0.06, noise: 0.03, peakHour: 13 });
  const hallucination = grounding.map((g) => Math.max(0, 4.2 - (g - 84) * 0.8 + (rand() - 0.5) * 0.8));
  const retrievalHit  = grounding.map((g) => Math.min(99, g + 8 + (rand() - 0.5) * 4));

  // Query → Chunk → Citation flow (chord arcs)
  const queries = [
    { id: 'q1', label: 'how do I reset cluster' },
    { id: 'q2', label: 'retention vs storage cost' },
    { id: 'q3', label: 'agent memory dedup rules' },
    { id: 'q4', label: 'hybrid search score fusion' },
    { id: 'q5', label: 'mmap segment format' },
    { id: 'q6', label: 'WAL recovery procedure' },
    { id: 'q7', label: 'what is HNSW recall' },
    { id: 'q8', label: 'pricing large context' },
  ];
  const chunks = [
    { id: 'c1', label: 'arch/cluster-design.md#reset' },
    { id: 'c2', label: 'rfc/042-retention.md#cost' },
    { id: 'c3', label: 'rfc/051-agent-memory.md#dedup' },
    { id: 'c4', label: 'rfc/039-hybrid-search.md#rrf' },
    { id: 'c5', label: 'arch/hnsw-internals.md' },
    { id: 'c6', label: 'runbook/wal-recovery.md' },
    { id: 'c7', label: 'docs/query-dsl.md#knn' },
    { id: 'c8', label: 'rfc/048-embed-proxy.md' },
    { id: 'c9', label: 'docs/pricing.md#ctx' },
    { id: 'c10', label: 'runbook/oncall.md' },
  ];
  const flows = [];
  for (const q of queries) {
    // each query pulls 3..5 chunks
    const k = 3 + Math.floor(rand() * 3);
    const picked = new Set();
    while (picked.size < k) picked.add('c' + (1 + Math.floor(rand() * chunks.length)));
    for (const cid of picked) {
      flows.push({ from: q.id, to: cid, weight: 0.4 + rand() * 0.6 });
    }
  }

  // Sample answer with token attention weights
  const sampleText = 'The XERJ.ai cluster reset procedure requires draining the WAL, flushing all memtables to disk, and then restarting with the recovery flag set. Do not skip the WAL drain step — lost writes in the fsync tail are unrecoverable.';
  const tokens = sampleText.split(' ').map((t) => {
    const hot = /(cluster|reset|drain|WAL|flush|memtables|recovery|unrecoverable)/i.test(t);
    return { text: t, weight: hot ? 0.65 + rand() * 0.35 : 0.05 + rand() * 0.25 };
  });

  const chunkHitDensity = {
    rows: ['Q&A', 'SEARCH', 'SUMMARIZE', 'CODE', 'EXTRACT', 'AGENT'],
    cols: ['c1','c2','c3','c4','c5','c6','c7','c8','c9','c10'],
    matrix: Array.from({ length: 6 }, () =>
      Array.from({ length: 10 }, () => Math.round(100 + rand() * 900))
    ),
  };

  const retrievalSource = [
    { label: 'HYBRID (RRF)', value: 6120 },
    { label: 'VECTOR KNN',   value: 2340 },
    { label: 'BM25 ONLY',    value:  820 },
    { label: 'MEMORY RECALL',value:  410 },
  ];

  const lowGroundingPrompts = pareto(
    [
      'what changed in release 0.2',
      'why is latency up today',
      'who owns the billing pipeline',
      'current on-call for Europe',
      'is the fix deployed yet',
      'any anomaly in the last hour',
      'what does the new quota mean',
      'regression vs last week',
    ],
    4200, { alpha: 0.7, rand }
  ).map((it) => ({ ...it, value: Math.round(it.value / 10) + '%' }));

  return {
    metrics: {
      grounding: { value: grounding[grounding.length - 1], formatted: grounding[grounding.length - 1].toFixed(1), delta: (rand() - 0.35) * 4 },
      hallucination: { value: hallucination[hallucination.length - 1], formatted: hallucination[hallucination.length - 1].toFixed(2), delta: (rand() - 0.6) * 1.8 },
      hitRate: { value: retrievalHit[retrievalHit.length - 1], formatted: retrievalHit[retrievalHit.length - 1].toFixed(1), delta: (rand() - 0.4) * 4 },
      avgCitations: { value: 3.4, formatted: '3.4', delta: (rand() - 0.5) * 0.8 },
    },
    series: {
      grounding,
      hallucination,
      retrievalHit,
      startLabel: rangeLabels(range)[0],
      endLabel:   rangeLabels(range)[1],
    },
    flow: { queries, chunks, flows },
    attention: { tokens, text: sampleText },
    chunkHitDensity,
    retrievalSource,
    lowGroundingPrompts,
  };
};

// ==========================================================
// VECTOR INDEX  — HNSW + embedding space
// ==========================================================
const buildVectorIndex = (rand, range) => {
  const n = points(range);
  const qps = diurnal(n, 1800, { rand, peakHour: 14 });
  const p50 = qps.map(() => 2.8 + rand() * 0.7);
  const p95 = qps.map(() => 9.4 + rand() * 2.2);
  const p99 = qps.map(() => 22 + rand() * 6);
  const recall = qps.map(() => 96.2 + rand() * 2);

  // 2D projected embedding space with 6 clusters
  const clusters = [
    { label: 'code',      center: [20, 70], spread: 9,  count: 180 },
    { label: 'docs',      center: [60, 40], spread: 12, count: 220 },
    { label: 'runbook',   center: [82, 68], spread: 7,  count: 110 },
    { label: 'tickets',   center: [42, 82], spread: 10, count: 160 },
    { label: 'chat',      center: [30, 20], spread: 14, count: 240 },
    { label: 'email',     center: [72, 18], spread: 8,  count: 120 },
  ].map((c) => {
    const points = Array.from({ length: c.count }, () => {
      const a = rand() * Math.PI * 2;
      const r = (rand() ** 0.5) * c.spread;
      return [c.center[0] + Math.cos(a) * r, c.center[1] + Math.sin(a) * r];
    });
    return { label: c.label, points, centroid: c.center };
  });

  const vectors = clusters.reduce((a, c) => a + c.points.length, 0);

  const models = [
    { label: 'text-embed-3 (1536)', value: 720 },
    { label: 'cohere-v3 (1024)',    value: 340 },
    { label: 'e5-large (1024)',     value: 190 },
    { label: 'local-bge-m3 (1024)', value: 80 },
  ];

  // Parallel coordinates of 40 recent queries across 5 dims
  const dims = [
    { name: 'LATENCY' }, { name: 'TOKENS' }, { name: 'COST' },
    { name: 'RECALL' }, { name: 'GROUND'  },
  ];
  const rows = Array.from({ length: 80 }, () => [
    300 + rand() * 1400,       // latency ms
    800 + rand() * 18000,      // tokens
    0.01 + rand() * 0.18,      // cost usd
    82 + rand() * 16,          // recall %
    70 + rand() * 28,          // grounding %
  ]);
  const highlight = [1420, 14200, 0.16, 94, 86];

  return {
    metrics: {
      vectors: { value: vectors, formatted: compact.format(vectors * 6200), hint: '6.2M total · shard 14/32' },
      dim:     { value: 1536, formatted: '1536', hint: '3 models' },
      disk:    { value: 38,   formatted: '38', hint: 'GB quantized' },
      qps:     { value: qps[qps.length - 1], formatted: compact.format(qps[qps.length - 1]), delta: (rand() - 0.3) * 6 },
      recall:  { value: 96.8, formatted: '96.8', delta: (rand() - 0.45) * 0.6 },
      p95:     { value: p95[p95.length - 1], formatted: p95[p95.length - 1].toFixed(1), delta: (rand() - 0.6) * 8 },
    },
    series: {
      qps, p50, p95, p99, recall,
      startLabel: rangeLabels(range)[0],
      endLabel:   rangeLabels(range)[1],
    },
    clusters,
    models,
    pcoords: { dims, rows, highlight },
  };
};

// ==========================================================
// AGENT MEMORY  — XERJ.ai's unique primitive
// ==========================================================
const buildAgentMemory = (rand, range) => {
  const n = points(range);
  const size = diurnal(n, 2_200_000, { rand, amplitude: 0.25, noise: 0.04 });
  const dedup = diurnal(n, 34, { rand, amplitude: 0.08, noise: 0.02 });
  const recallP95 = diurnal(n, 14, { rand, amplitude: 0.12, noise: 0.04 });

  const agents = [
    { label: 'oncall-triage',     value: 48210 },
    { label: 'doc-writer',        value: 31450 },
    { label: 'incident-postmort', value: 22100 },
    { label: 'customer-qa',       value: 18600 },
    { label: 'eng-copilot',       value: 14200 },
    { label: 'billing-agent',     value:  8900 },
    { label: 'sales-prospect',    value:  6400 },
    { label: 'ops-baby-sitter',   value:  3800 },
  ];

  // Semantic memory clusters for one specific agent (oncall-triage)
  const clusters = [
    { label: 'network', center: [18, 70], spread: 10, count: 90 },
    { label: 'storage', center: [52, 78], spread: 8,  count: 110 },
    { label: 'billing', center: [78, 50], spread: 12, count: 80 },
    { label: 'auth',    center: [30, 30], spread: 9,  count: 70 },
    { label: 'query',   center: [65, 22], spread: 11, count: 120 },
  ].map((c) => {
    const points = Array.from({ length: c.count }, () => {
      const a = rand() * Math.PI * 2;
      const r = (rand() ** 0.5) * c.spread;
      return [c.center[0] + Math.cos(a) * r, c.center[1] + Math.sin(a) * r];
    });
    return { label: c.label, points, centroid: c.center };
  });

  const topMemories = [
    { label: 'cluster reset procedure (verified)',            value: 2804 },
    { label: 'pricing context for large enterprise',          value: 1920 },
    { label: 'hybrid search fusion score explanation',        value: 1612 },
    { label: 'WAL recovery tail loss (0.0014%)',              value: 1402 },
    { label: 'HNSW recall vs build time tradeoff',            value: 1180 },
    { label: 'agent memory dedup rules (semantic)',           value:  980 },
    { label: 'mmap segment format roadmap',                   value:  770 },
    { label: 'flush policy triggers (investigate)',           value:  612 },
  ];

  const recentOps = [
    ['23:14:02', 'INSERT',  'oncall-triage',   'new memory · cluster reset'],
    ['23:13:51', 'DEDUP',   'doc-writer',      'merged with 2 prior entries'],
    ['23:13:22', 'RECALL',  'oncall-triage',   'k=5 · top score 0.86'],
    ['23:12:48', 'FORGET',  'eng-copilot',     'decay below 0.12'],
    ['23:12:09', 'REWRITE', 'incident-pm',     'compacted from 4 → 1 entry'],
    ['23:11:44', 'INSERT',  'billing-agent',   'new memory · refund workflow'],
    ['23:11:03', 'RECALL',  'customer-qa',     'k=3 · top score 0.79'],
    ['23:10:41', 'DEDUP',   'sales-prospect',  'merged with 1 prior entry'],
    ['23:10:12', 'RECALL',  'eng-copilot',     'k=8 · top score 0.91'],
    ['23:09:58', 'INSERT',  'oncall-triage',   'new memory · billing-sync'],
  ];

  return {
    metrics: {
      entries: { value: size[size.length - 1], formatted: compact.format(size[size.length - 1]), delta: (rand() - 0.4) * 5 },
      dedup:   { value: dedup[dedup.length - 1], formatted: dedup[dedup.length - 1].toFixed(1), delta: (rand() - 0.4) * 3 },
      recall:  { value: recallP95[recallP95.length - 1], formatted: recallP95[recallP95.length - 1].toFixed(1), delta: (rand() - 0.55) * 4 },
      growth:  { value: 84000, formatted: '84K', hint: 'per day' },
      agents:  { value: agents.length, formatted: String(agents.length), hint: agents.length + ' active' },
    },
    series: {
      size, dedup, recallP95,
      startLabel: rangeLabels(range)[0],
      endLabel:   rangeLabels(range)[1],
    },
    agents,
    clusters,
    topMemories,
    recentOps,
  };
};

// ==========================================================
// LOGS OVERVIEW  — kept from earlier iteration
// ==========================================================
const buildLogsOverview = (rand, range) => {
  const n = points(range);
  const base = 42000 + rand() * 8000;
  const totalSeries = diurnal(n, base, { rand });
  const total = Math.round(sumOf(totalSeries));
  const peakVal = Math.max(...totalSeries);
  const peakAt  = peakLabel(totalSeries);
  const errRate = 0.6 + rand() * 0.6;

  const sourcesTotal  = 84 + Math.floor(rand() * 20);
  const sourcesActive = sourcesTotal - Math.floor(rand() * 6);

  const byLevel = [
    { label: 'INFO',  value: Math.round(total * (0.86 + rand() * 0.04)) },
    { label: 'WARN',  value: Math.round(total * (0.08 + rand() * 0.02)) },
    { label: 'ERROR', value: Math.round(total * (errRate / 100)) },
    { label: 'DEBUG', value: Math.round(total * 0.003) },
    { label: 'FATAL', value: Math.round(total * 0.0001) + 2 },
  ];

  const services = [
    'api-gateway','auth-service','billing','checkout','search',
    'catalog','inventory','shipping','notifications','webhook-worker',
    'recommendation','pricing',
  ];
  const topServices = pareto(services, Math.round(total * 0.92), { alpha: 0.95, rand });

  const hosts = [
    'ip-10-0-1-17','ip-10-0-1-42','ip-10-0-2-88','ip-10-0-3-11',
    'ip-10-0-3-54','ip-10-0-4-09','ip-10-0-4-73','ip-10-0-5-22',
    'ip-10-0-5-91','ip-10-0-6-18','ip-10-0-6-60','ip-10-0-7-04',
  ];
  const topHosts = pareto(hosts, Math.round(total * 0.82), { alpha: 0.7, rand });

  const cols = Array.from({ length: 12 }, (_, i) => String(i * 2).padStart(2, '0'));
  const matrix = ['MON','TUE','WED','THU','FRI','SAT','SUN'].map((_, di) =>
    cols.map((_, hi) => {
      const h = hi * 2;
      const phase = Math.cos(((h - 14) / 24) * 2 * Math.PI);
      const weekend = di >= 5 ? 0.55 : 1;
      return Math.round(base * weekend * (1 + 0.8 * phase) * (0.85 + rand() * 0.3));
    })
  );

  return {
    metrics: {
      total: { value: total, formatted: compact.format(total), delta: (rand() - 0.45) * 6 },
      peak:  { value: peakVal, formatted: compact.format(peakVal), at: peakAt },
      errorRate: { value: errRate, formatted: errRate.toFixed(2), delta: (rand() - 0.6) * 0.8 },
      sources: { value: sourcesTotal, formatted: String(sourcesTotal), active: sourcesActive },
    },
    series: {
      total: totalSeries,
      startLabel: rangeLabels(range)[0],
      endLabel:   rangeLabels(range)[1],
    },
    byLevel,
    topServices,
    topHosts,
    heatmap: { cols, matrix },
  };
};

// ==========================================================
// SYSTEM  — kept from earlier iteration
// ==========================================================
const buildSystem = (rand, range) => {
  const n = points(range);
  const cpuSeries  = diurnal(n, 48,  { rand, amplitude: 0.35, noise: 0.08, peakHour: 14 });
  const memSeries  = diurnal(n, 61,  { rand, amplitude: 0.12, noise: 0.04, peakHour: 18 });
  const diskSeries = diurnal(n, 180, { rand, amplitude: 0.55, noise: 0.18, peakHour: 13 });
  const netSeries  = diurnal(n, 420, { rand, amplitude: 0.6,  noise: 0.2,  peakHour: 15 });

  const hosts = [
    'ip-10-0-1-17','ip-10-0-1-42','ip-10-0-2-88','ip-10-0-3-11',
    'ip-10-0-3-54','ip-10-0-4-09','ip-10-0-4-73','ip-10-0-5-22',
    'ip-10-0-5-91','ip-10-0-6-18','ip-10-0-6-60','ip-10-0-7-04',
  ];
  const hostCpu = hosts.map((h) => {
    const baseline = 30 + rand() * 45;
    const vals = Array.from({ length: 40 }, (_, i) => {
      const phase = Math.cos(((i - 20) / 20) * Math.PI);
      return Math.max(0, Math.min(100, baseline + phase * 14 + (rand() - 0.5) * 10));
    });
    return { label: h.toUpperCase(), values: vals, value: Math.round(vals[vals.length - 1]) + '%' };
  });

  const procs = [
    'java -jar checkout-svc','postgres: walwriter','node search-proxy',
    'java -jar auth-svc','python3 metrics-exporter','redis-server *:6379',
    'envoy -c /etc/envoy.yaml','containerd-shim-runc','systemd-journald',
    'kubelet --config /etc/kubelet','otelcol --config otel.yaml',
  ];
  const topProcs = pareto(procs, 1000, { alpha: 0.9, rand }).map((p) => ({
    label: p.label, value: p.value / 10,
  }));

  const topHosts = hosts.map((h, i) => ({
    label: h,
    value: Math.round(30 + (rand() * 60) + (i === 0 ? 15 : 0)),
  })).sort((a, b) => b.value - a.value);

  const authSeries = diurnal(n, 28, { rand, amplitude: 0.5 });
  const authTotal = Math.round(sumOf(authSeries));
  const failures = Math.round(authTotal * (0.03 + rand() * 0.02));
  const topFailUsers = pareto(
    ['root','admin','ubuntu','deploy','postgres','jenkins','test','oracle'],
    failures, { alpha: 0.65, rand }
  );
  const topFailIPs = pareto(
    ['45.137.21.4','185.234.218.19','193.32.162.157','91.240.118.99',
     '162.247.74.217','141.98.10.55','89.248.165.74','185.142.236.35'],
    failures, { alpha: 0.7, rand }
  );

  return {
    metrics: {
      hosts:   { value: hosts.length, formatted: String(hosts.length), hint: `${hosts.length - 1} healthy` },
      alerts:  { value: 3, formatted: '3', delta: -1.0, deltaGood: 'down', hint: '1 warn · 2 info' },
      cpuMean: { value: cpuSeries[cpuSeries.length - 1], formatted: cpuSeries[cpuSeries.length - 1].toFixed(0), delta: 4.1 },
      memMean: { value: memSeries[memSeries.length - 1], formatted: memSeries[memSeries.length - 1].toFixed(0), delta: 0.8 },
    },
    series: {
      cpu: cpuSeries, mem: memSeries, disk: diskSeries, net: netSeries,
      startLabel: rangeLabels(range)[0], endLabel: rangeLabels(range)[1],
    },
    hostCpu,
    topProcs,
    topHosts,
    auth: { total: authTotal, failures, series: authSeries, topFailUsers, topFailIPs },
  };
};

// (The client-side fake search corpus and `mockSearch()` that lived here are
// gone. They fabricated log-shaped hits for the Search · Discover table, and
// the shell kept them on screen whenever the engine call failed — invented
// documents under a LIVE pill. Discover, the Corpus home and the Reader show
// the user's own documents, so they are engine-or-error only: see
// data/query.js#NEVER_MOCK and test/guest-graph.test.mjs.)

// ==========================================================
// ANOMALY DETECTION  — client-side z-score over a mock stream
// ==========================================================
/**
 * Rolling-window anomaly detection. Pure client-side z-score
 * because the engine's native anomaly pipeline is stubbed.
 * Returns value series + upper/lower bands + outlier indices
 * with attribution over N features.
 */
const buildAnomalyDetect = (rand, range) => {
  const n = points(range);
  // Primary signal: query latency with injected spikes
  const base = 420 + rand() * 90;
  const values = Array.from({ length: n }, (_, i) => {
    const diurnal = Math.cos(((i / n) * 24 - 14) / 24 * 2 * Math.PI);
    return Math.max(10, base * (1 + 0.22 * diurnal) + (rand() - 0.5) * 28);
  });
  // Inject anomalies
  const injected = [];
  const numAnom = 5 + Math.floor(rand() * 3);
  for (let k = 0; k < numAnom; k++) {
    const idx = Math.floor(rand() * n);
    const mag = 2.8 + rand() * 3;
    values[idx] = values[idx] * mag;
    injected.push(idx);
  }

  // Rolling window stats for bands
  const W = Math.max(6, Math.floor(n / 12));
  const upper = new Array(n);
  const lower = new Array(n);
  for (let i = 0; i < n; i++) {
    const lo = Math.max(0, i - W), hi = Math.min(n - 1, i + W);
    let s = 0, s2 = 0, c = 0;
    for (let j = lo; j <= hi; j++) { s += values[j]; s2 += values[j] * values[j]; c++; }
    const mean = s / c;
    const std  = Math.sqrt(Math.max(0, s2 / c - mean * mean));
    upper[i] = mean + 2.5 * std;
    lower[i] = Math.max(0, mean - 2.5 * std);
  }

  // Compute z-scores — anomalies are indices exceeding 2.5σ
  const anomalies = [];
  for (let i = 0; i < n; i++) {
    if (values[i] > upper[i] || values[i] < lower[i]) {
      const score = (values[i] - (upper[i] + lower[i]) / 2) / ((upper[i] - lower[i]) / 2 || 1);
      anomalies.push({ idx: i, score: Math.abs(score), value: values[i] });
    }
  }
  anomalies.sort((a, b) => b.score - a.score);

  // Feature attribution for top anomaly
  const topFeatures = [
    { label: 'query_latency_p95', value: 3.8 },
    { label: 'upstream_timeout',  value: 2.9 },
    { label: 'cache_miss_rate',   value: 2.1 },
    { label: 'flush_duration',    value: 1.8 },
    { label: 'gc_pause',          value: 0.7 },
    { label: 'cpu_saturation',    value: 0.5 },
  ];

  // Top anomalous signals over the period
  const topSignals = [
    { label: 'api-gateway /checkout p95',   value: anomalies[0]?.score || 0 },
    { label: 'billing-svc query latency',    value: 4.1 },
    { label: 'auth-svc failed logins',       value: 3.6 },
    { label: 'search-svc wal_lag_ms',        value: 2.9 },
    { label: 'embed-proxy cost surge',       value: 2.4 },
    { label: 'vector-index p99 latency',     value: 2.0 },
    { label: 'agent-memory dedup dip',       value: 1.7 },
  ];

  return {
    metrics: {
      detected: { value: anomalies.length, formatted: String(anomalies.length), delta: (rand() - 0.6) * 18 },
      covered:  { value: 14,   formatted: '14', hint: 'signals scored · z·score' },
      falsePos: { value: 1.2,  formatted: '1.2', delta: (rand() - 0.5) * 0.6 },
      recall:   { value: 92,   formatted: '92', hint: 'vs. hand labels' },
    },
    series: {
      values, upper, lower, anomalies: anomalies.slice(0, 12),
      startLabel: rangeLabels(range)[0], endLabel: rangeLabels(range)[1],
    },
    topFeatures,
    topSignals,
    injected,
  };
};

// ==========================================================
// INGEST PIPELINE  — engine internals, matches Prometheus metrics
// ==========================================================
const buildIngest = (rand, range) => {
  const n = points(range);
  const docsIn    = diurnal(n, 68000, { rand, amplitude: 0.6, peakHour: 15 });
  const bytesIn   = docsIn.map((d) => d * (1.1 + rand() * 0.6) * 1024);
  const walLag    = docsIn.map(() => 2 + rand() * 22);
  const flushMs   = docsIn.map(() => 180 + rand() * 220);
  const mergeMs   = docsIn.map(() => 420 + rand() * 900);
  const memBytes  = docsIn.map(() => 1.8 * 1024 * 1024 * 1024 + rand() * 800 * 1024 * 1024);

  const idxLatP50 = docsIn.map(() => 0.6 + rand() * 0.3);
  const idxLatP95 = docsIn.map(() => 2.1 + rand() * 0.9);
  const idxLatP99 = docsIn.map(() => 6.4 + rand() * 2.8);

  const topIndices = pareto(
    ['logs-prod','logs-stage','traces','docs','metrics','events',
     'agent-memory','embeddings','alerts','audit'],
    Math.round(sumOf(docsIn)), { alpha: 0.85, rand }
  );

  const perField = [
    { label: '@timestamp',  value: 98,  encoding: 'Δ-of-Δ' },
    { label: 'service',     value: 91,  encoding: 'DICT' },
    { label: 'level',       value: 96,  encoding: 'DICT' },
    { label: 'host',        value: 88,  encoding: 'DICT' },
    { label: 'message',     value: 61,  encoding: 'ZSTD+TMPL' },
    { label: 'trace_id',    value: 28,  encoding: 'UVARINT' },
    { label: 'latency_ms',  value: 72,  encoding: 'FOR+RLE' },
    { label: 'status',      value: 94,  encoding: 'DICT' },
    { label: 'bytes_out',   value: 58,  encoding: 'FOR' },
  ];

  const pipelineFlow = [
    { label: 'HTTP PARSE',        value: 100 },
    { label: 'FIELD MAP',         value: 98 },
    { label: 'PIPELINE · redact', value: 97 },
    { label: 'WAL APPEND',        value: 97 },
    { label: 'MEMTABLE',          value: 97 },
    { label: 'FLUSH → SEGMENT',   value: 92 },
    { label: 'MERGE',             value: 61 },
  ];

  return {
    metrics: {
      docsRate:    { value: docsIn[docsIn.length - 1], formatted: compact.format(docsIn[docsIn.length - 1]), delta: (rand() - 0.3) * 8, unit: 'docs/s' },
      bytesRate:   { value: bytesIn[bytesIn.length - 1], formatted: (bytesIn[bytesIn.length - 1] / 1024 / 1024).toFixed(1), unit: 'MB/s' },
      walLag:      { value: walLag[walLag.length - 1], formatted: walLag[walLag.length - 1].toFixed(1), unit: 'ms', delta: (rand() - 0.55) * 12 },
      segments:    { value: 184, formatted: '184', hint: '3 indices · 12 shards' },
      mem:         { value: (memBytes[memBytes.length - 1] / 1024 / 1024 / 1024).toFixed(2), formatted: (memBytes[memBytes.length - 1] / 1024 / 1024 / 1024).toFixed(2), unit: 'GB' },
      ratio:       { value: 4.8, formatted: '4.8', unit: '×', hint: 'vs. raw JSON' },
    },
    series: {
      docsIn, bytesIn, walLag, flushMs, mergeMs, memBytes,
      idxLatP50, idxLatP95, idxLatP99,
      startLabel: rangeLabels(range)[0], endLabel: rangeLabels(range)[1],
    },
    topIndices,
    perField,
    pipelineFlow,
  };
};

// ---------- filter application ----------------------------
// The real backend will compile `filters` into an ES filter clause.
// Here we approximate the behaviour so click-to-filter actually
// changes the shape: if a filter matches a label inside a top-N or
// Dist, we keep only buckets whose labels case-insensitively include
// the filter value. Series and metrics get a proportional shrink.
//
// A filter value can be a scalar OR an array — array = OR semantics,
// matching GH#3693 ("Multi-select (OR) dashboard filtering").
function filterCount(filters) {
  let n = 0;
  for (const v of Object.values(filters || {})) {
    n += Array.isArray(v) ? v.length : 1;
  }
  return n;
}
function filterRatio(filters) {
  const n = filterCount(filters);
  if (!n) return 1;
  // Each active filter shrinks things ~35%, clamped at 10%.
  // Multi-value (OR) filters shrink less because they match more.
  const fields = Object.keys(filters || {}).length;
  return Math.max(0.1, Math.pow(0.65, fields));
}

function applyFilters(data, filters) {
  if (!filters || !Object.keys(filters).length) return data;
  const r = filterRatio(filters);
  const values = [];
  for (const v of Object.values(filters)) {
    if (Array.isArray(v)) for (const x of v) values.push(String(x).toLowerCase());
    else values.push(String(v).toLowerCase());
  }
  const keep = (label) => {
    const s = String(label || '').toLowerCase();
    return values.some((v) => s.includes(v));
  };

  const walk = (node) => {
    if (Array.isArray(node)) {
      // Array of {label,value} buckets → narrow to matches when any
      // label matches a filter value. Otherwise rescale proportionally.
      if (node.length && typeof node[0] === 'object' && node[0] && 'label' in node[0] && 'value' in node[0]) {
        const hits = node.filter((b) => keep(b.label));
        if (hits.length) return hits.map((b) => ({ ...b, value: Math.max(1, Math.round(b.value * r * 1.2)) }));
        return node.map((b) => ({ ...b, value: Math.max(0, Math.round(b.value * r)) }));
      }
      // Numeric series → scale
      if (node.length && typeof node[0] === 'number') return node.map((v) => v * r);
      return node.map(walk);
    }
    if (node && typeof node === 'object') {
      const out = {};
      for (const k of Object.keys(node)) {
        const v = node[k];
        // Scale well-known metric shapes
        if (v && typeof v === 'object' && 'value' in v && 'formatted' in v && typeof v.value === 'number') {
          const scaled = Math.round(v.value * r);
          out[k] = { ...v, value: scaled, formatted: formatLike(v.formatted, scaled) };
        } else {
          out[k] = walk(v);
        }
      }
      return out;
    }
    return node;
  };
  return walk(data);
}

// Best-effort reformat of a metric's display string after scaling
// the numeric value. Handles compact (1.2K), percent (42), and
// currency ($1,230) forms.
function formatLike(prev, n) {
  const s = String(prev);
  if (s.startsWith('$')) return '$' + Math.round(n).toLocaleString('en');
  if (/^[\d.]+[KMB]$/.test(s)) return compact.format(n);
  if (/^\d+$/.test(s)) return String(Math.round(n));
  return compact.format(n);
}

// ---------- public entry point ----------------------------
// CUSTOM ranges resolve to the nearest bucket shape the mock knows
// so charts still render. The real backend will respect from/to as
// absolute ISO timestamps. We pick a bucket based on the span:
//   < 2h    → 1H shape
//   < 36h   → 24H shape
//   < 10d   → 7D shape
//   < 45d   → 30D shape
//   else    → 90D shape
function bucketForCustom(customRange) {
  if (!customRange || !customRange.from || !customRange.to) return '24H';
  const a = new Date(customRange.from).getTime();
  const b = new Date(customRange.to).getTime();
  if (!isFinite(a) || !isFinite(b) || b <= a) return '24H';
  const hrs = (b - a) / 3_600_000;
  if (hrs < 2)   return '1H';
  if (hrs < 36)  return '24H';
  if (hrs < 240) return '7D';
  if (hrs < 1080) return '30D';
  return '90D';
}

export function mock(dashId, range = '24H', ctx = {}) {
  const { cluster = '', filters = {}, customRange = null } = ctx;
  const effectiveRange = range === 'CUSTOM' ? bucketForCustom(customRange) : range;
  // Include cluster + filter keys + custom-range bounds in the seed so
  // switching filters visibly re-shuffles the numbers.
  const customKey = customRange ? (customRange.from || '') + '/' + (customRange.to || '') : '';
  const seedKey = dashId + '|' + effectiveRange + '|' + cluster + '|' + Object.keys(filters).sort().join(',') + '|' + customKey;
  const seedRand = rng(hashStr(seedKey));
  seedRand.range = effectiveRange;
  let out;
  switch (dashId) {
    case 'ai-overview':     out = buildAiOverview(seedRand, effectiveRange); break;
    case 'rag-quality':     out = buildRagQuality(seedRand, effectiveRange); break;
    case 'vector-index':    out = buildVectorIndex(seedRand, effectiveRange); break;
    case 'agent-memory':    out = buildAgentMemory(seedRand, effectiveRange); break;
    case 'anomaly-detect':  out = buildAnomalyDetect(seedRand, effectiveRange); break;
    case 'ingest-pipeline': out = buildIngest(seedRand, effectiveRange); break;
    case 'logs-overview':   out = buildLogsOverview(seedRand, effectiveRange); break;
    case 'system':          out = buildSystem(seedRand, effectiveRange); break;
    default:                out = buildAiOverview(seedRand, effectiveRange);
  }
  return applyFilters(out, filters);
}
