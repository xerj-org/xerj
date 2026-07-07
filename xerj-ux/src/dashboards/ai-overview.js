// ============================================================
// Dashboard — AI · OVERVIEW  (flagship)
//
// Answers: am I serving AI at scale, what does it cost me,
// what would it cost on ES + Pinecone + Splunk, where is my
// money going?
// ============================================================

import { Num }                              from '../ux/text.js';
import { Spark, Series, Dist, Heatmap }     from '../ux/charts.js';
import { Ribbon3D, FlowBand }               from '../ux/charts-ai.js';
import { Citations }                        from '../ux/charts-ops.js';
import { TopN }                             from '../ux/layout.js';
import { dashboardCitations }               from '../data/feedback-citations.js';

export const aiOverview = {
  id:   'ai-overview',
  name: 'AI · Overview',
  render: ({ data, time }) => ({
    title:  'AI · OVERVIEW',
    kicker: 'XERJ INTELLIGENCE',
    meta:   [time, 'AI DATA PLANE'],
    panels: [

      { id: 'queries', eyebrow: 'LLM QUERIES', cols: 4, type: 'metric',
        render: () => Num({
          value: data.metrics.queries.formatted, unit: 'queries',
          spark: Spark(data.series.queries, { w: 220, h: 44 }),
          delta: data.metrics.queries.delta, emphasis: true,
        }),
      },
      { id: 'tokens', eyebrow: 'TOKENS · IN + OUT', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.tokens.formatted, unit: 'T',
          delta: data.metrics.tokens.delta, emphasis: false,
        }),
      },
      { id: 'cost', eyebrow: 'SPEND · USD', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.cost.formatted, unit: 'usd',
          delta: data.metrics.cost.delta, deltaGood: 'down', emphasis: false,
        }),
      },
      { id: 'savings', eyebrow: 'vs. ES + PINECONE + SPLUNK', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.savings.formatted, unit: 'saved',
          hint:  data.metrics.savings.note,
          emphasis: false,
        }),
      },
      { id: 'cacheHit', eyebrow: 'CACHE HIT', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.cacheHit.formatted, unit: '%',
          delta: data.metrics.cacheHit.delta, emphasis: false,
        }),
      },

      { id: 'queriesSeries', eyebrow: 'QUERIES OVER TIME', cols: 12, type: 'line',
        render: () => Series(data.series.queries, {
          h: 160, labels: [data.series.startLabel, data.series.endLabel], unit: 'q/s',
        }),
      },

      { id: 'latencyRibbons', eyebrow: 'LATENCY · PER MODEL · AXONOMETRIC', cols: 12, type: 'ribbon3d',
        render: () => Ribbon3D({ series: data.latencyRibbons, h: 280, depth: 14 }),
      },

      { id: 'tokenFlow', eyebrow: 'TOKEN BUDGET', cols: 12, type: 'flowband',
        render: () => FlowBand({ segments: data.flowSegments, unit: 'T' }),
      },

      { id: 'models', eyebrow: 'BY MODEL', cols: 12, type: 'dist',
        render: () => Dist({ segments: data.models, width: 1200 }),
      },

      { id: 'topIntents', eyebrow: 'TOP INTENTS · CLICK TO DRILL', cols: 6, type: 'topn',
        drilldown: { to: 'search-discover' },
        render: () => TopN({ items: data.intents, total: data.metrics.queries.value, n: 10, filterField: 'intent' }),
      },

      { id: 'topDocs', eyebrow: 'TOP DOCUMENTS · CLICK TO FILTER', cols: 6, type: 'topn',
        render: () => TopN({ items: data.topDocs, total: data.metrics.queries.value, n: 10, filterField: 'doc' }),
      },

      { id: 'costHeatmap', eyebrow: 'SPEND · WEEKDAY × 2H', cols: 12, type: 'heatmap',
        render: () => Heatmap({
          rows: ['MON','TUE','WED','THU','FRI','SAT','SUN'],
          cols: data.costHeatmap.cols,
          matrix: data.costHeatmap.matrix,
          cellFmt: (v) => '$' + v.toFixed(0),
        }),
      },

      { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({ items: dashboardCitations['ai-overview'] || [], total: 5150 }),
      },

    ],
  }),
};
