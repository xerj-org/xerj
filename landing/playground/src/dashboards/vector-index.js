// ============================================================
// Dashboard — VECTOR INDEX
//
// Vector-index health + embedding space. Answers: where do my
// embeddings live, how is recall holding up, what is the
// shape of my corpus in vector space?
// ============================================================

import { Num }                             from '../ux/text.js';
import { Spark, Series, Dist, Multiples }  from '../ux/charts.js';
import { EmbedSpace, ParallelCoords, Ribbon3D } from '../ux/charts-ai.js';
import { Citations }                       from '../ux/charts-ops.js';
import { TopN }                            from '../ux/layout.js';
import { Gauge }                           from '../ux/charts-ext.js';
import { dashboardCitations }              from '../data/feedback-citations.js';

export const vectorIndex = {
  id:   'vector-index',
  name: 'Vector · Index',
  render: ({ data, time }) => ({
    title:  'VECTOR · INDEX',
    kicker: 'EXACT kNN · EMBEDDINGS · HYBRID',
    meta:   [time, 'XERJ-VECTOR'],
    panels: [

      { id: 'vectors', eyebrow: 'VECTORS', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.vectors.formatted, unit: 'total',
          hint: data.metrics.vectors.hint, emphasis: true,
        }),
      },
      { id: 'dim', eyebrow: 'DIMENSIONS', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.dim.formatted, unit: 'd',
          hint: data.metrics.dim.hint, emphasis: false,
        }),
      },
      { id: 'disk', eyebrow: 'ON DISK', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.disk.formatted, unit: 'gb',
          hint: data.metrics.disk.hint, emphasis: false,
        }),
      },
      { id: 'qps', eyebrow: 'QUERIES/s', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.qps.formatted, unit: 'qps',
          spark: Spark(data.series.qps, { w: 140, h: 32 }),
          delta: data.metrics.qps.delta, emphasis: false,
        }),
      },
      { id: 'recall', eyebrow: 'RECALL @ 10', cols: 3, type: 'gauge',
        render: () => Gauge({
          value: data.metrics.recall.value, min: 80, max: 100, unit: '%',
          thresholds: [90, 95], emphasis: false,
        }),
      },

      { id: 'embedSpace', eyebrow: 'EMBEDDING SPACE · UMAP PROJECTION', cols: 12, type: 'embedspace',
        render: () => EmbedSpace({
          clusters: data.clusters, h: 420, highlight: [48, 48],
        }),
      },

      { id: 'annLatency', eyebrow: 'ANN LATENCY · P50 / P95 / P99', cols: 12, type: 'ribbon3d',
        render: () => Ribbon3D({
          series: [
            { label: 'P50', values: data.series.p50 },
            { label: 'P95', values: data.series.p95 },
            { label: 'P99', values: data.series.p99 },
          ],
          h: 220, depth: 14,
        }),
      },

      { id: 'pcoords', eyebrow: 'QUERY PROFILE · PARALLEL COORDINATES', cols: 12, type: 'pcoords',
        render: () => ParallelCoords({
          dims:      data.pcoords.dims,
          rows:      data.pcoords.rows,
          highlight: data.pcoords.highlight,
          h: 260,
        }),
      },

      { id: 'models', eyebrow: 'EMBEDDING MODELS', cols: 6, type: 'topn',
        render: () => TopN({ items: data.models, n: 6, valueFmt: (v) => v + 'M' }),
      },

      { id: 'p95Spark', eyebrow: 'p95 LATENCY', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.p95.formatted, unit: 'ms',
          spark: Spark(data.series.p95, { w: 160, h: 32 }),
          delta: data.metrics.p95.delta, deltaGood: 'down', emphasis: false,
        }),
      },

      { id: 'recallTimeline', eyebrow: 'RECALL OVER TIME', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.recall.formatted, unit: '%',
          spark: Spark(data.series.recall, { w: 160, h: 32 }),
          emphasis: false,
        }),
      },

      { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({ items: dashboardCitations['vector-index'] || [], total: 5150 }),
      },

    ],
  }),
};
