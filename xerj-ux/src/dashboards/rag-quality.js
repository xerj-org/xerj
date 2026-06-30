// ============================================================
// Dashboard — RAG · QUALITY
//
// The unsolved observability problem of 2026: are my RAG
// answers actually grounded? Which retrievals mattered? Which
// prompts hallucinate?
// ============================================================

import { Num }                         from '../ux/text.js';
import { Spark, Series, Dist, Heatmap } from '../ux/charts.js';
import { ChordArcs, AttentionMap }     from '../ux/charts-ai.js';
import { Citations }                   from '../ux/charts-ops.js';
import { TopN }                        from '../ux/layout.js';
import { dashboardCitations }          from '../data/feedback-citations.js';

export const ragQuality = {
  id:   'rag-quality',
  name: 'RAG · Quality',
  render: ({ data, time }) => ({
    title:  'RAG · QUALITY',
    kicker: 'GROUNDING · HALLUCINATION · CITATIONS',
    meta:   [time, 'RETRIEVAL PIPELINE'],
    panels: [

      { id: 'grounding', eyebrow: 'GROUNDING SCORE', cols: 4, type: 'metric',
        render: () => Num({
          value: data.metrics.grounding.formatted, unit: '%',
          spark: Spark(data.series.grounding, { w: 220, h: 44 }),
          delta: data.metrics.grounding.delta, deltaGood: 'up', emphasis: true,
        }),
      },
      { id: 'halluc', eyebrow: 'HALLUCINATION RATE', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.hallucination.formatted, unit: '%',
          delta: data.metrics.hallucination.delta, deltaGood: 'down', emphasis: false,
        }),
      },
      { id: 'hitRate', eyebrow: 'RETRIEVAL HIT RATE', cols: 3, type: 'metric',
        render: () => Num({
          value: data.metrics.hitRate.formatted, unit: '%',
          delta: data.metrics.hitRate.delta, emphasis: false,
        }),
      },
      { id: 'citations', eyebrow: 'AVG CITATIONS', cols: 2, type: 'metric',
        render: () => Num({
          value: data.metrics.avgCitations.formatted, unit: 'per answer',
          delta: data.metrics.avgCitations.delta, emphasis: false,
        }),
      },

      { id: 'groundingSeries', eyebrow: 'GROUNDING OVER TIME', cols: 12, type: 'line',
        render: () => Series(data.series.grounding, {
          h: 140, labels: [data.series.startLabel, data.series.endLabel], unit: '%',
        }),
      },

      { id: 'flow', eyebrow: 'RETRIEVAL FLOW · QUERY → CHUNK', cols: 12, type: 'chord',
        render: () => ChordArcs({
          sources: data.flow.queries,
          targets: data.flow.chunks,
          flows:   data.flow.flows,
          h: 440,
        }),
      },

      { id: 'attention', eyebrow: 'SAMPLE ANSWER · TOKEN ATTENTION', cols: 12, type: 'attention',
        render: () => AttentionMap({ tokens: data.attention.tokens }),
      },

      { id: 'retrievalSource', eyebrow: 'BY RETRIEVAL SOURCE', cols: 12, type: 'dist',
        render: () => Dist({ segments: data.retrievalSource, width: 1200 }),
      },

      { id: 'lowGrounding', eyebrow: 'LOWEST GROUNDING PROMPTS', cols: 6, type: 'topn',
        render: () => TopN({
          items: data.lowGroundingPrompts,
          n: 8,
          valueFmt: (v) => v,
        }),
      },

      { id: 'chunkDensity', eyebrow: 'CHUNK HIT DENSITY · QUERY TYPE × CHUNK', cols: 6, type: 'heatmap',
        render: () => Heatmap({
          rows: data.chunkHitDensity.rows,
          cols: data.chunkHitDensity.cols,
          matrix: data.chunkHitDensity.matrix,
          cellFmt: (v) => (v >= 1000 ? Math.round(v / 1000) + 'K' : String(v)),
        }),
      },

      { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({ items: dashboardCitations['rag-quality'] || [], total: 5150 }),
      },

    ],
  }),
};
