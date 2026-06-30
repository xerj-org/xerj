// ============================================================
// Dashboard — [LOGS] OVERVIEW
// Replaces Kibana [Logs] Overview. Typography-first.
//
// Panel schema:
//   { id, eyebrow, cols, render: () => innerHtmlString }
// ============================================================

import { Num }                             from '../ux/text.js';
import { Spark, Series, Dist, Heatmap }    from '../ux/charts.js';
import { Citations }                       from '../ux/charts-ops.js';
import { TopN }                            from '../ux/layout.js';
import { dashboardCitations }              from '../data/feedback-citations.js';

export const logsOverview = {
  id:   'logs-overview',
  name: 'Logs',
  render: ({ data, time }) => ({
    title:  'LOGS · OVERVIEW',
    kicker: 'OBSERVE',
    meta:   [time, 'ALL INDICES'],
    panels: [
      { id: 'total', eyebrow: 'TOTAL EVENTS', cols: 4, type: 'metric',
        render: () => Num({
          value: data.metrics.total.formatted, unit: 'events',
          spark: Spark(data.series.total, { w: 200, h: 40 }),
          delta: data.metrics.total.delta, deltaGood: 'up', emphasis: true,
        }),
      },
      { id: 'peak', eyebrow: 'PEAK RATE', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.peak.formatted, unit: 'e/s',
          hint: 'at ' + data.metrics.peak.at, emphasis: false,
        }),
      },
      { id: 'errRate', eyebrow: 'ERROR RATE', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.errorRate.formatted, unit: '%',
          delta: data.metrics.errorRate.delta, deltaGood: 'down', emphasis: false,
        }),
      },
      { id: 'sources', eyebrow: 'SOURCES', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.sources.formatted, unit: 'hosts',
          hint: data.metrics.sources.active + ' active', emphasis: false,
        }),
      },
      { id: 'series', eyebrow: 'EVENTS OVER TIME', cols: 12, type: 'line',
        render: () => Series(data.series.total, {
          h: 160, labels: [data.series.startLabel, data.series.endLabel], unit: '/bucket',
        }),
      },
      { id: 'levels', eyebrow: 'BY LEVEL', cols: 12, type: 'dist',
        render: () => Dist({ segments: data.byLevel, width: 1200 }),
      },
      { id: 'topServices', eyebrow: 'TOP SERVICES · CLICK TO FILTER', cols: 6, type: 'topn',
        render: () => TopN({ items: data.topServices, total: data.metrics.total.value, n: 10, filterField: 'service' }),
      },
      { id: 'topHosts', eyebrow: 'TOP HOSTS · CLICK TO DRILL', cols: 6, type: 'topn',
        drilldown: { to: 'system' },
        render: () => TopN({ items: data.topHosts, total: data.metrics.total.value, n: 10, filterField: 'host' }),
      },
      { id: 'heatmap', eyebrow: 'INTENSITY · WEEKDAY × 2H', cols: 12, type: 'heatmap',
        render: () => Heatmap({
          rows: ['MON','TUE','WED','THU','FRI','SAT','SUN'],
          cols: data.heatmap.cols,
          matrix: data.heatmap.matrix,
          cellFmt: (v) => (v >= 1000 ? Math.round(v / 1000) + 'K' : String(v)),
        }),
      },
      { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({ items: dashboardCitations['logs-overview'] || [], total: 5150 }),
      },
    ],
  }),
};
