// ============================================================
// XERJ.ai — Chart type registry
//
// The master catalog of visualization types, used by:
//   1. The edit-mode ADD picker (lists every type).
//   2. User-added panels (render demo data via this registry).
//
// Each entry: { id, name, cols, render(data) }. `render` may
// consume the current dashboard's data object OR fall back to
// canned demo data so an added panel always previews nicely
// regardless of the active dashboard.
// ============================================================

import { Num, fmt }               from './text.js';
import { Spark, Series, Dist, Heatmap, Multiples } from './charts.js';
import { VBar, Hist, Gauge, Scatter, Stacked, Treemap } from './charts-ext.js';
import { EmbedSpace, Ribbon3D, ChordArcs, ParallelCoords, AttentionMap, FlowBand } from './charts-ai.js';
import { Table, Markdown }        from './tables.js';
import { TopN, Events }           from './layout.js';

// ---------- demo data (deterministic) ---------------------
const demoSeries = Array.from({ length: 48 }, (_, i) =>
  50 + Math.sin(i / 5.5) * 22 + Math.cos(i / 3.2) * 9 + ((i * 37) % 7)
);
const demoItems = [
  { label: 'alpha',   value: 4230 },
  { label: 'beta',    value: 3180 },
  { label: 'gamma',   value: 2564 },
  { label: 'delta',   value: 1923 },
  { label: 'epsilon', value: 1340 },
  { label: 'zeta',    value:  912 },
  { label: 'eta',     value:  680 },
  { label: 'theta',   value:  445 },
];
const demoSegments = [
  { label: '2XX', value: 9620 },
  { label: '3XX', value:  410 },
  { label: '4XX', value:  190 },
  { label: '5XX', value:   48 },
];
const demoTree = [
  { label: 'root', value: 13000, children: [
    { label: 'api-gateway',  value: 5100, children: [
      { label: '/v2/search',  value: 2400 },
      { label: '/v2/cart',    value: 1800 },
      { label: '/v2/catalog', value:  900 },
    ]},
    { label: 'auth-service', value: 2400 },
    { label: 'billing',      value: 1600 },
    { label: 'checkout',     value: 1400 },
  ]},
];

// ---------- registry --------------------------------------
export const chartTypes = {

  metric: {
    name: 'METRIC',
    cols: 4,
    describe: 'A single headline number with optional spark and delta.',
    render: () => Num({
      value: '42.0K', unit: 'demo',
      spark: Spark(demoSeries, { w: 160, h: 30 }),
      delta: 3.1, emphasis: true,
    }),
  },

  gauge: {
    name: 'GAUGE',
    cols: 4,
    describe: 'Single value on a bounded 1px track.',
    render: () => Gauge({ value: 73, min: 0, max: 100, unit: '%', label: 'demo' }),
  },

  line: {
    name: 'LINE',
    cols: 12,
    describe: 'Full-width time series. Replaces both Line and Area.',
    render: () => Series(demoSeries, { h: 140, labels: ['START', 'NOW'], unit: 'u/s' }),
  },

  spark: {
    name: 'SPARK',
    cols: 3,
    describe: 'Inline 1px sparkline with latest value.',
    render: () => Num({
      value: fmt(demoSeries[demoSeries.length - 1]),
      unit: 'u/s',
      spark: Spark(demoSeries, { w: 200, h: 36 }),
      emphasis: false,
    }),
  },

  bar: {
    name: 'BAR',
    cols: 6,
    describe: 'Vertical 1px-line bar chart.',
    render: () => VBar({
      items: demoSeries.slice(0, 24).map((v, i) => ({ label: String(i).padStart(2, '0'), value: Math.round(v) })),
      h: 140,
      unit: 'u',
    }),
  },

  histogram: {
    name: 'HISTOGRAM',
    cols: 12,
    describe: 'Column histogram of counts across buckets.',
    render: () => Hist({
      items: Array.from({ length: 32 }, (_, i) => ({
        label: String(i),
        value: Math.round(Math.exp(-Math.pow((i - 16) / 5, 2)) * 1000 + (i % 3) * 30),
      })),
      h: 160, unit: 'count',
    }),
  },

  dist: {
    name: 'DIST',
    cols: 12,
    describe: 'Distribution bar. Replaces pie/donut.',
    render: () => Dist({ segments: demoSegments, width: 1200 }),
  },

  stacked: {
    name: 'STACKED',
    cols: 12,
    describe: 'Horizontal stacked bars, one row per series.',
    render: () => Stacked({
      rows: [
        { label: 'US',  segments: demoSegments },
        { label: 'DE',  segments: demoSegments.map((s) => ({ ...s, value: s.value * 0.6 })) },
        { label: 'JP',  segments: demoSegments.map((s) => ({ ...s, value: s.value * 0.45 })) },
        { label: 'BR',  segments: demoSegments.map((s) => ({ ...s, value: s.value * 0.3 })) },
      ],
    }),
  },

  topn: {
    name: 'TOP-N',
    cols: 6,
    describe: 'Ranked list with aligned 1px bars. Replaces horizontal bar.',
    render: () => TopN({ items: demoItems, total: demoItems.reduce((a, i) => a + i.value, 0), n: 8 }),
  },

  treemap: {
    name: 'TREEMAP',
    cols: 6,
    describe: 'Nested hierarchy of ranked items.',
    render: () => Treemap({ items: demoTree }),
  },

  heatmap: {
    name: 'HEATMAP',
    cols: 12,
    describe: 'Character-intensity grid. Rows × columns of numbers whose opacity encodes magnitude.',
    render: () => Heatmap({
      rows: ['A','B','C','D','E','F','G'],
      cols: ['00','02','04','06','08','10','12','14','16','18','20','22'],
      matrix: Array.from({ length: 7 }, (_, r) =>
        Array.from({ length: 12 }, (_, c) => {
          const phase = Math.cos(((c * 2 - 14) / 24) * 2 * Math.PI);
          return Math.round(80 + 60 * phase + r * 5 + ((r + c * 7) % 9));
        })),
      cellFmt: (v) => String(v),
    }),
  },

  multiples: {
    name: 'MULTIPLES',
    cols: 12,
    describe: 'Grid of small sparklines, one per dimension.',
    render: () => Multiples({
      items: Array.from({ length: 8 }, (_, i) => ({
        label: 'SERIES ' + String.fromCharCode(65 + i),
        values: demoSeries.map((v) => v + (i * 3) + Math.sin(i) * 5),
        value: String(Math.round(50 + i * 2)),
      })),
      w: 180, h: 22,
    }),
  },

  scatter: {
    name: 'SCATTER',
    cols: 6,
    describe: 'X/Y point cloud using `·` characters as data points.',
    render: () => Scatter({
      points: Array.from({ length: 80 }, () => {
        const t = Math.random();
        return [20 + t * 80 + (Math.random() - 0.5) * 20, 10 + t * 70 + (Math.random() - 0.5) * 25];
      }),
      xLabel: 'LATENCY', yLabel: 'THROUGHPUT', h: 220,
    }),
  },

  table: {
    name: 'TABLE',
    cols: 6,
    describe: 'Text data table, no borders, tabular numbers.',
    render: () => Table({
      columns: ['NAME', 'COUNT', 'PCT', 'TREND'],
      rows: demoItems.map((it, i) => [
        it.label,
        fmt(it.value),
        ((it.value / demoItems.reduce((a, x) => a + x.value, 0)) * 100).toFixed(1) + '%',
        i % 2 ? '▲ 3.1' : '▼ 1.4',
      ]),
      align: ['left', 'right', 'right', 'right'],
    }),
  },

  events: {
    name: 'EVENTS',
    cols: 12,
    describe: 'Recent event tail — time, severity, message.',
    render: () => Events({ items: [
      { at: '23:14:02.081', sev: 'err',  msg: 'demo · upstream timed out (connect) while connecting to upstream' },
      { at: '23:13:51.602', sev: 'warn', msg: 'demo · slow query 842 ms  SELECT * FROM orders WHERE ...' },
      { at: '23:13:04.114', sev: 'info', msg: 'demo · batch 238 committed · 14,402 docs · 112 ms' },
      { at: '23:12:48.771', sev: 'warn', msg: 'demo · rate limit approaching 85% for tenant=acme' },
      { at: '23:12:09.033', sev: 'info', msg: 'demo · segment merged · 3→1 · 412 MB → 386 MB' },
    ]}),
  },

  markdown: {
    name: 'TEXT',
    cols: 6,
    describe: 'Free-form prose panel with very light markdown.',
    render: () => Markdown(
`## NOTES

This is a **text** panel. Use it for *annotations*, playbooks,
SLO definitions, on-call runbooks — anything that the reader
needs to see next to the data.

Inline \`code\` renders in accent, which is the one place prose
borrows the accent color.`
    ),
  },

  // ---- AI / RAG primitives -------------------------------

  embedspace: {
    name: 'EMBED-SPACE',
    cols: 12,
    describe: 'Embedding projection with 1px cluster hulls.',
    render: () => {
      const mk = (cx, cy, r, label, n) => {
        const pts = Array.from({ length: n }, () => {
          const a = Math.random() * Math.PI * 2;
          const rr = Math.sqrt(Math.random()) * r;
          return [cx + Math.cos(a) * rr, cy + Math.sin(a) * rr];
        });
        return { label, points: pts, centroid: [cx, cy] };
      };
      return EmbedSpace({
        clusters: [
          mk(22, 70, 10, 'code',    120),
          mk(58, 38, 12, 'docs',    180),
          mk(82, 66,  8, 'runbook', 80),
          mk(40, 80,  9, 'tickets', 110),
          mk(28, 22, 12, 'chat',    160),
        ],
        h: 360,
      });
    },
  },

  ribbon3d: {
    name: '3D RIBBON',
    cols: 12,
    describe: 'Axonometric stacked 1px time series. The "3D" chart.',
    render: () => {
      const series = Array.from({ length: 5 }, (_, i) => ({
        label: 'SERIES ' + String.fromCharCode(65 + i),
        values: Array.from({ length: 48 }, (_, j) =>
          40 + i * 9 + Math.sin(j / 5 + i) * 15 + Math.cos(j / 8) * 6
        ),
      }));
      return Ribbon3D({ series, h: 260, depth: 14 });
    },
  },

  chord: {
    name: 'CHORD FLOW',
    cols: 12,
    describe: '1px Bézier arcs from sources to targets.',
    render: () => {
      const sources = Array.from({ length: 6 }, (_, i) => ({ id: 's' + i, label: 'query ' + (i + 1) }));
      const targets = Array.from({ length: 8 }, (_, i) => ({ id: 't' + i, label: 'doc/chunk ' + (i + 1) }));
      const flows = [];
      for (const s of sources) {
        const k = 2 + Math.floor(Math.random() * 3);
        for (let i = 0; i < k; i++) {
          flows.push({ from: s.id, to: 't' + Math.floor(Math.random() * 8), weight: Math.random() });
        }
      }
      return ChordArcs({ sources, targets, flows, h: 360 });
    },
  },

  pcoords: {
    name: 'PARALLEL-COORDS',
    cols: 12,
    describe: 'Multi-dimensional rows as 1px polylines across N axes.',
    render: () => ParallelCoords({
      dims: [{ name: 'LAT' }, { name: 'TOK' }, { name: 'COST' }, { name: 'RECALL' }, { name: 'GROUND' }],
      rows: Array.from({ length: 60 }, () => [
        200 + Math.random() * 1500,
        800 + Math.random() * 20000,
        0.01 + Math.random() * 0.2,
        80 + Math.random() * 18,
        70 + Math.random() * 28,
      ]),
      h: 260,
    }),
  },

  attention: {
    name: 'ATTENTION-MAP',
    cols: 12,
    describe: 'Inline text with per-token opacity = attention weight.',
    render: () => AttentionMap({
      tokens: ('The XERJ.ai cluster reset requires draining the WAL and flushing all memtables before restart . Lost writes in the fsync tail are unrecoverable .').split(' ').map((t) => ({
        text: t,
        weight: /(cluster|reset|drain|WAL|flush|memtables|restart|unrecoverable)/i.test(t)
          ? 0.7 + Math.random() * 0.3
          : 0.08 + Math.random() * 0.22,
      })),
    }),
  },

  flowband: {
    name: 'FLOW BAND',
    cols: 12,
    describe: 'Single-row stacked flow allocation with labeled ticks.',
    render: () => FlowBand({
      segments: [
        { label: 'SYS PROMPT',  value: 1200 },
        { label: 'CONTEXT',     value: 8400 },
        { label: 'COMPLETION',  value: 640 },
      ],
      unit: 'tok',
    }),
  },

};

/** Ordered list for the ADD picker. Reads left to right. AI first. */
export const chartTypeList = [
  // AI / RAG
  'embedspace','ribbon3d','chord','pcoords','attention','flowband',
  // Classic
  'metric','gauge','spark','line','bar','histogram','dist','stacked',
  'topn','treemap','heatmap','multiples','scatter','table','events','markdown',
].map((id) => ({ id, ...chartTypes[id] }));
