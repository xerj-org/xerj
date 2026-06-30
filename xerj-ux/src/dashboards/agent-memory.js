// ============================================================
// Dashboard — AGENT · MEMORY
//
// XERJ.ai's unique primitive: append-only index with semantic
// dedup and recency-weighted recall for long-running agents.
// This is the only dashboard in the category — nobody else
// ships this at the data-layer level.
// ============================================================

import { Num }                     from '../ux/text.js';
import { Spark, Series, Dist }     from '../ux/charts.js';
import { EmbedSpace }              from '../ux/charts-ai.js';
import { Citations }               from '../ux/charts-ops.js';
import { TopN }                    from '../ux/layout.js';
import { Table }                   from '../ux/tables.js';
import { dashboardCitations }      from '../data/feedback-citations.js';

export const agentMemory = {
  id:   'agent-memory',
  name: 'Agent · Memory',
  render: ({ data, time }) => ({
    title:  'AGENT · MEMORY',
    kicker: 'APPEND-ONLY · DEDUP · RECENCY',
    meta:   [time, 'AGENTIC LOOPS'],
    panels: [

      { id: 'entries', eyebrow: 'MEMORY ENTRIES', cols: 4, type: 'metric',
        render: () => Num({
          value: data.metrics.entries.formatted, unit: 'stored',
          spark: Spark(data.series.size, { w: 220, h: 44 }),
          delta: data.metrics.entries.delta, emphasis: true,
        }),
      },
      { id: 'dedup', eyebrow: 'DEDUP RATE', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.dedup.formatted, unit: '%',
          delta: data.metrics.dedup.delta, deltaGood: 'up', emphasis: false,
        }),
      },
      { id: 'recall', eyebrow: 'RECALL P95', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.recall.formatted, unit: 'ms',
          delta: data.metrics.recall.delta, deltaGood: 'down', emphasis: false,
        }),
      },
      { id: 'growth', eyebrow: 'GROWTH', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.growth.formatted, unit: 'new',
          hint: data.metrics.growth.hint, emphasis: false,
        }),
      },
      { id: 'agents', eyebrow: 'AGENTS', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.agents.formatted, unit: 'online',
          hint: data.metrics.agents.hint, emphasis: false,
        }),
      },

      { id: 'sizeSeries', eyebrow: 'MEMORY SIZE OVER TIME', cols: 12, type: 'line',
        render: () => Series(data.series.size, {
          h: 140, labels: [data.series.startLabel, data.series.endLabel], unit: 'entries',
        }),
      },

      { id: 'embedSpace', eyebrow: 'SEMANTIC MEMORY · ONCALL-TRIAGE · UMAP', cols: 12, type: 'embedspace',
        render: () => EmbedSpace({ clusters: data.clusters, h: 360 }),
      },

      { id: 'byAgent', eyebrow: 'BY AGENT', cols: 6, type: 'topn',
        render: () => TopN({
          items: data.agents,
          total: data.agents.reduce((a, b) => a + b.value, 0),
          n: 10,
        }),
      },

      { id: 'topMemories', eyebrow: 'MOST-REFERENCED MEMORIES', cols: 6, type: 'topn',
        render: () => TopN({
          items: data.topMemories,
          n: 10,
          valueFmt: (v) => v + '× ',
        }),
      },

      { id: 'dedupSeries', eyebrow: 'DEDUP RATE OVER TIME', cols: 6, type: 'line',
        render: () => Series(data.series.dedup, {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: '%',
        }),
      },

      { id: 'recallSeries', eyebrow: 'RECALL P95 OVER TIME', cols: 6, type: 'line',
        render: () => Series(data.series.recallP95, {
          h: 100, labels: [data.series.startLabel, data.series.endLabel], unit: 'ms',
        }),
      },

      { id: 'recentOps', eyebrow: 'RECENT OPERATIONS', cols: 12, type: 'table',
        render: () => Table({
          columns: ['TIME', 'OP', 'AGENT', 'DETAIL'],
          rows: data.recentOps,
          align: ['left', 'left', 'left', 'left'],
        }),
      },

      { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({ items: dashboardCitations['agent-memory'] || [], total: 5150 }),
      },

    ],
  }),
};
