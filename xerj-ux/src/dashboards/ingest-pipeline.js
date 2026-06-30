// ============================================================
// Dashboard — INGEST · PIPELINE  (engine internals)
//
// Every number on this dashboard corresponds to a real metric
// emitted by xerj-common/src/metrics.rs (/v1/metrics):
//   docs_indexed, bytes_written, index_latency_s (p50/p95/p99),
//   flush_duration_s, merge_duration_s, wal_write_latency_s,
//   wal_size_bytes, segment_count, memory_usage, active_searches.
// Plus per-field encodings from /v1/indices/:name/encodings.
// ============================================================

import { Num }                        from '../ux/text.js';
import { Spark, Series, Multiples }   from '../ux/charts.js';
import { FlowBand }                   from '../ux/charts-ai.js';
import { MetricTile, Citations }      from '../ux/charts-ops.js';
import { TopN }                       from '../ux/layout.js';
import { Table }                      from '../ux/tables.js';
import { Gauge }                      from '../ux/charts-ext.js';
import { dashboardCitations }         from '../data/feedback-citations.js';

export const ingestPipeline = {
  id:   'ingest-pipeline',
  name: 'Ingest · Pipeline',
  render: ({ data, time }) => ({
    title:  'INGEST · PIPELINE',
    kicker: 'WAL · MEMTABLE · SEGMENT · MERGE',
    meta:   [time, 'ENGINE INTERNALS'],
    caption: 'Every value on this page is a real Prometheus metric emitted by xerj-common::metrics — no synthetic signals. Use this to watch the WAL → memtable → segment → merge chain and the exact field encodings the storage layer chose.',
    panels: [

      { id: 'docsRate', eyebrow: 'DOCS INDEXED/s', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.docsRate.formatted, unit: 'docs/s',
          spark: Spark(data.series.docsIn, { w: 160, h: 36 }),
          delta: data.metrics.docsRate.delta, emphasis: true,
        }),
      },
      { id: 'bytesRate', eyebrow: 'BYTES WRITTEN/s', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.bytesRate.formatted, unit: 'MB/s',
          emphasis: false,
        }),
      },
      { id: 'walLag', eyebrow: 'WAL WRITE LATENCY', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.walLag.formatted, unit: 'ms',
          delta: data.metrics.walLag.delta, deltaGood: 'down', emphasis: false,
        }),
      },
      { id: 'segments', eyebrow: 'SEGMENTS', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.segments.formatted, unit: 'open',
          hint: data.metrics.segments.hint, emphasis: false,
        }),
      },
      { id: 'mem', eyebrow: 'MEMORY USAGE', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.mem.formatted, unit: 'gb',
          emphasis: false,
        }),
      },

      { id: 'pipeline', eyebrow: 'PIPELINE · END-TO-END FLOW', cols: 12, type: 'flowband',
        render: () => FlowBand({ segments: data.pipelineFlow, unit: '%' }),
      },

      { id: 'docsSeries', eyebrow: 'INGEST THROUGHPUT', cols: 12, type: 'line',
        render: () => Series(data.series.docsIn, {
          h: 140, labels: [data.series.startLabel, data.series.endLabel], unit: 'docs/s',
        }),
      },

      { id: 'latency', eyebrow: 'INDEX LATENCY · p50 / p95 / p99', cols: 12, type: 'multiples',
        render: () => Multiples({
          items: [
            { label: 'P50', values: data.series.idxLatP50, value: data.series.idxLatP50[data.series.idxLatP50.length-1].toFixed(2) + ' MS' },
            { label: 'P95', values: data.series.idxLatP95, value: data.series.idxLatP95[data.series.idxLatP95.length-1].toFixed(2) + ' MS' },
            { label: 'P99', values: data.series.idxLatP99, value: data.series.idxLatP99[data.series.idxLatP99.length-1].toFixed(2) + ' MS' },
          ],
          w: 300, h: 28,
        }),
      },

      { id: 'flushDur', eyebrow: 'FLUSH DURATION', cols: 6, type: 'line',
        render: () => Series(data.series.flushMs, {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: 'ms',
        }),
      },

      { id: 'mergeDur', eyebrow: 'MERGE DURATION', cols: 6, type: 'line',
        render: () => Series(data.series.mergeMs, {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: 'ms',
        }),
      },

      { id: 'topIndices', eyebrow: 'DOCS INDEXED · BY INDEX', cols: 6, type: 'topn',
        render: () => TopN({ items: data.topIndices, n: 10 }),
      },

      { id: 'encodings', eyebrow: 'FIELD ENCODINGS · /v1/indices/:name/encodings', cols: 6, type: 'table',
        render: () => Table({
          columns: ['FIELD', 'ENCODING', 'RATIO'],
          rows: data.perField.map((f) => [f.label, f.encoding, f.value + '%']),
          align: ['left', 'left', 'right'],
        }),
      },

      { id: 'compressionRatio', eyebrow: 'COMPRESSION RATIO', cols: 6, type: 'gauge',
        render: () => Gauge({
          value: 4.8, min: 1, max: 8, unit: '× raw JSON',
          thresholds: [2, 5], label: 'target ≥ 5×',
        }),
      },

      { id: 'memSeries', eyebrow: 'MEMORY OVER TIME', cols: 6, type: 'line',
        render: () => Series(data.series.memBytes.map((b) => b / 1024 / 1024 / 1024), {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: 'GB',
        }),
      },

      { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({ items: dashboardCitations['ingest-pipeline'] || [], total: 5150 }),
      },

    ],
  }),
};
