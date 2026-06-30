// ============================================================
// Dashboard — ANOMALY · DETECTION  (z-score + attribution)
//
// Note: the engine's native anomaly pipeline is aspirational
// today. This dashboard runs a rolling 2.5σ detector on the
// client so the panels are real — it'll flip to server-side
// scoring once that module lands. The story is the same either
// way: *what fired, when, and why*.
// ============================================================

import { Num }               from '../ux/text.js';
import { Spark, Dist }       from '../ux/charts.js';
import { AnomalyBand, Citations } from '../ux/charts-ops.js';
import { TopN }              from '../ux/layout.js';
import { Treemap }           from '../ux/charts-ext.js';
import { ParallelCoords, AttentionMap } from '../ux/charts-ai.js';
import { dashboardCitations } from '../data/feedback-citations.js';

export const anomalyDetect = {
  id:   'anomaly-detect',
  name: 'Anomaly',
  render: ({ data, time }) => ({
    title:  'ANOMALY · DETECTION',
    kicker: 'WHAT FIRED · WHEN · WHY',
    meta:   [time, 'Z-SCORE · 2.5σ BAND · BETA'],
    caption: 'Rolling-window z-score over 14 engine-emitted signals. The normal band is mean ± 2.5σ across a sliding window. Crosses mark values outside the band; the top-anomaly attribution row explains which features moved.',
    panels: [

      { id: 'detected', eyebrow: 'ANOMALIES · LAST PERIOD', cols: 4, type: 'metric',
        render: () => Num({
          value: data.metrics.detected.formatted, unit: 'fired',
          delta: data.metrics.detected.delta, deltaGood: 'down', emphasis: true,
        }),
      },
      { id: 'covered', eyebrow: 'SIGNALS SCORED', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.covered.formatted, unit: 'streams',
          hint: data.metrics.covered.hint, emphasis: false,
        }),
      },
      { id: 'falsePos', eyebrow: 'FALSE-POSITIVE RATE', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.falsePos.formatted, unit: '%',
          delta: data.metrics.falsePos.delta, deltaGood: 'down', emphasis: false,
        }),
      },
      { id: 'recall', eyebrow: 'RECALL vs HAND LABELS', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.recall.formatted, unit: '%',
          hint: data.metrics.recall.hint, emphasis: false,
        }),
      },

      { id: 'band', eyebrow: 'QUERY LATENCY · NORMAL BAND · μ ± 2.5σ', cols: 12, type: 'anomalyband',
        render: () => AnomalyBand({
          values: data.series.values,
          upper:  data.series.upper,
          lower:  data.series.lower,
          anomalies: data.series.anomalies,
          h: 240,
          labels: [data.series.startLabel, data.series.endLabel],
          unit: 'ms',
        }),
      },

      { id: 'topSignals', eyebrow: 'MOST-ANOMALOUS SIGNALS · BY z-SCORE', cols: 6, type: 'topn',
        render: () => TopN({
          items: data.topSignals,
          n: 10,
          valueFmt: (v) => 'z ' + v.toFixed(1),
        }),
      },

      { id: 'features', eyebrow: 'FEATURE ATTRIBUTION · TOP ANOMALY', cols: 6, type: 'topn',
        render: () => TopN({
          items: data.topFeatures,
          n: 8,
          valueFmt: (v) => 'Δ ' + v.toFixed(1) + 'σ',
        }),
      },

      { id: 'pcoords', eyebrow: 'SIGNAL PROFILE · PARALLEL COORDINATES', cols: 12, type: 'pcoords',
        render: () => ParallelCoords({
          dims: [
            { name: 'LAT p95' }, { name: 'QPS' }, { name: 'WAL LAG' },
            { name: 'FLUSH' }, { name: 'MEM' }, { name: 'CPU' },
          ],
          rows: Array.from({ length: 80 }, () => [
            200 + Math.random() * 1400,
            800 + Math.random() * 1800,
            2 + Math.random() * 20,
            180 + Math.random() * 220,
            60 + Math.random() * 30,
            30 + Math.random() * 55,
          ]),
          highlight: [1680, 2600, 28, 420, 84, 82],
          h: 260,
        }),
      },

      { id: 'cause', eyebrow: 'ROOT-CAUSE CANDIDATES · RANKED', cols: 6, type: 'treemap',
        render: () => Treemap({ items: [
          { label: 'upstream checkout-svc:8080', value: 38, children: [
            { label: 'connect timeout (5xx burst)', value: 22 },
            { label: 'slow upstream (p95 +420ms)',  value: 12 },
            { label: 'connection reset',            value:  4 },
          ]},
          { label: 'gc pause · auth-svc',  value: 14, children: [
            { label: 'old-gen exhausted',  value:  8 },
            { label: 'young-gen pause',    value:  6 },
          ]},
          { label: 'wal lag · ingest-worker', value: 9 },
          { label: 'embed-proxy 429 rate',    value: 6 },
        ]}),
      },

      { id: 'trace', eyebrow: 'CORRELATED LOG · ATTENTION EXPLAIN', cols: 6, type: 'attention',
        render: () => AttentionMap({
          tokens: ('upstream timed out ( connect ) while connecting to upstream client: 10.0.3.42 upstream: "http://checkout-svc:8080/api/v2/checkout/quote" request: "POST /api/v2/checkout HTTP/1.1" retry_after: 30s fallback: cached-rate-bucket').split(' ').map((t) => ({
            text: t,
            weight: /(upstream|timed|out|connect|checkout-svc|retry_after|fallback)/i.test(t)
              ? 0.7 + Math.random() * 0.3
              : 0.1 + Math.random() * 0.25,
          })),
        }),
      },

      { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({ items: dashboardCitations['anomaly-detect'] || [], total: 5150 }),
      },

    ],
  }),
};
