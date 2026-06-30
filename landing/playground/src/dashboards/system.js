// ============================================================
// Dashboard — [SYSTEM] OVERVIEW + AUTH
// Replaces Metricbeat System Overview + auth logs.
// ============================================================

import { Num }                             from '../ux/text.js';
import { Spark, Series, Multiples }        from '../ux/charts.js';
import { Citations }                       from '../ux/charts-ops.js';
import { TopN }                            from '../ux/layout.js';
import { dashboardCitations }              from '../data/feedback-citations.js';

export const system = {
  id:   'system',
  name: 'System',
  render: ({ data, time }) => ({
    title:  'SYSTEM · OVERVIEW',
    kicker: 'HOSTS · LOAD · AUTH',
    meta:   [time, 'METRICBEAT + AUTH'],
    panels: [
      { id: 'hosts', eyebrow: 'HOSTS', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.hosts.formatted, unit: 'online',
          hint: data.metrics.hosts.hint, emphasis: true,
        }),
      },
      { id: 'alerts', eyebrow: 'ACTIVE ALERTS', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.alerts.formatted, unit: 'open',
          delta: data.metrics.alerts.delta, deltaGood: 'down',
          hint: data.metrics.alerts.hint, emphasis: false,
        }),
      },
      { id: 'cpuMean', eyebrow: 'MEAN CPU', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.cpuMean.formatted, unit: '%',
          spark: Spark(data.series.cpu, { w: 140, h: 32 }),
          delta: data.metrics.cpuMean.delta, deltaGood: 'down', emphasis: false,
        }),
      },
      { id: 'memMean', eyebrow: 'MEAN MEM', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.memMean.formatted, unit: '%',
          spark: Spark(data.series.mem, { w: 140, h: 32 }),
          delta: data.metrics.memMean.delta, deltaGood: 'down', emphasis: false,
        }),
      },
      { id: 'cpu', eyebrow: 'CPU', cols: 6, type: 'line',
        render: () => Series(data.series.cpu, {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: '%',
        }),
      },
      { id: 'mem', eyebrow: 'MEMORY', cols: 6, type: 'line',
        render: () => Series(data.series.mem, {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: '%',
        }),
      },
      { id: 'disk', eyebrow: 'DISK I/O', cols: 6, type: 'line',
        render: () => Series(data.series.disk, {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: 'MB/s',
        }),
      },
      { id: 'net', eyebrow: 'NETWORK I/O', cols: 6, type: 'line',
        render: () => Series(data.series.net, {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: 'MB/s',
        }),
      },
      { id: 'hostCpu', eyebrow: 'PER-HOST CPU · SMALL MULTIPLES', cols: 12, type: 'multiples',
        render: () => Multiples({ items: data.hostCpu, w: 180, h: 24 }),
      },
      { id: 'topProcs', eyebrow: 'TOP PROCESSES', cols: 6, type: 'topn',
        render: () => TopN({ items: data.topProcs, n: 10, valueFmt: (v) => v.toFixed(1) + ' %' }),
      },
      { id: 'topHosts', eyebrow: 'HOSTS BY LOAD', cols: 6, type: 'topn',
        render: () => TopN({ items: data.topHosts, n: 10, valueFmt: (v) => v + ' %' }),
      },
      { id: 'authSeries', eyebrow: 'AUTH · FAILED LOGINS', cols: 12, type: 'line',
        render: () => Series(data.auth.series, {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: '/min',
        }),
      },
      { id: 'topFailUsers', eyebrow: 'TOP FAILED USERS', cols: 6, type: 'topn',
        render: () => TopN({ items: data.auth.topFailUsers, total: data.auth.failures, n: 8 }),
      },
      { id: 'topFailIPs', eyebrow: 'TOP ATTACKING IPS', cols: 6, type: 'topn',
        render: () => TopN({ items: data.auth.topFailIPs, total: data.auth.failures, n: 8 }),
      },
      { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({ items: dashboardCitations['system'] || [], total: 5150 }),
      },
    ],
  }),
};
