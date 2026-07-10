// ============================================================
// Section — ALERTS
//
// Live alert rules + recent fires + noisy rules + rule authoring.
//
// Corpus evidence: 349 items in categories/07-alerting-and-watcher,
// 49% negative. Recurring complaints:
//   • "Watcher → Alerting → Rules" migration confusion (GH issues)
//   • noisy alerts, silent alerts, rules fire with no trail
//   • separate Watcher app you have to remember exists
//   • authoring YAML for simple threshold rules
//
// XERJ response: rules as code (stored as JSON in xerj.rules.*),
// NL→rule authoring via the embed-proxy (Phase 3), connector
// status visible inline, no separate "app".
// ============================================================

import { Num }                    from '../ux/text.js';
import { Spark, Series, Dist }    from '../ux/charts.js';
import { TopN, Events }           from '../ux/layout.js';
import { Table, Markdown }        from '../ux/tables.js';
import { FlowBand }               from '../ux/charts-ai.js';
import { Citations }              from '../ux/charts-ops.js';

export const alerts = {
  id: 'alerts',
  name: 'Alerts',
  section: 'alerts',
  render: ({ data, time }) => ({
    title: 'ALERTS',
    kicker: 'RULES · FIRES · CONNECTORS',
    meta: [time, 'AS CODE · NO WATCHER APP'],
    caption: 'Alert rules as code, not YAML in a hidden app. Every rule is a JSON file under `xerj.rules.*` you can diff, review, and version. This view is the operator surface — active fires, rule health, connector status, and the corpus evidence that says Kibana got this wrong.',
    panels: [

      { id: 'active', eyebrow: 'ACTIVE FIRES', cols: 3, type: 'metric',
        render: () => Num({
          value: '7', unit: 'open',
          delta: -12.5, deltaGood: 'down',
          spark: Spark([3,5,8,12,14,11,9,7,6,7,5,7], { w: 160, h: 32 }),
          emphasis: true,
        }),
      },
      { id: 'silenced', eyebrow: 'SILENCED', cols: 2, type: 'metric',
        render: () => Num({ value: '3', unit: 'rules', hint: '2h avg ttl', emphasis: false }),
      },
      { id: 'rules', eyebrow: 'RULES DEFINED', cols: 2, type: 'metric',
        render: () => Num({ value: '48', unit: 'total', hint: '12 recently modified', emphasis: false }),
      },
      { id: 'fires', eyebrow: 'FIRES · 24H', cols: 2, type: 'metric',
        render: () => Num({ value: '132', unit: 'events', delta: 4.2, deltaGood: 'down', emphasis: false }),
      },
      { id: 'connectors', eyebrow: 'CONNECTORS', cols: 3, type: 'metric',
        render: () => Num({ value: '4', unit: 'healthy', hint: 'slack · pagerduty · webhook · email', emphasis: false }),
      },

      { id: 'firesOverTime', eyebrow: 'FIRES OVER TIME', cols: 12, type: 'line',
        render: () => Series(
          Array.from({ length: 48 }, (_, i) => 4 + Math.sin(i / 6) * 3 + Math.cos(i / 3) * 2 + Math.random() * 2),
          { h: 140, labels: ['00:00', '24:00'], unit: '/bucket' }
        ),
      },

      { id: 'bySev', eyebrow: 'BY SEVERITY', cols: 12, type: 'dist',
        render: () => Dist({
          segments: [
            { label: 'CRITICAL', value: 8 },
            { label: 'ERROR',    value: 24 },
            { label: 'WARN',     value: 62 },
            { label: 'INFO',     value: 38 },
          ],
          width: 1200,
        }),
      },

      { id: 'topNoisy', eyebrow: 'TOP NOISY RULES · FIRES / 24H', cols: 6, type: 'topn',
        render: () => TopN({
          items: [
            { label: 'checkout-svc p95 latency > 2s',  value: 28 },
            { label: 'auth failures > 10/min',          value: 24 },
            { label: 'disk utilization > 90%',          value: 18 },
            { label: 'memtable ratio > 0.8',            value: 14 },
            { label: 'upstream 5xx > 1%',               value: 12 },
            { label: 'wal lag > 50ms',                  value: 11 },
            { label: 'cache hit rate < 50%',            value:  9 },
            { label: 'sq8 recall < 95%',               value:  7 },
          ],
          total: 132,
          n: 8,
        }),
      },

      { id: 'recent', eyebrow: 'RECENT · LAST 30 EVENTS', cols: 6, type: 'events',
        render: () => Events({ items: [
          { at: '23:14:02', sev: 'err',  msg: 'checkout-svc p95 latency > 2s · 2842ms · critical' },
          { at: '23:13:51', sev: 'warn', msg: 'auth failures > 10/min · 14 failed logins · src=45.137.21.4' },
          { at: '23:13:22', sev: 'warn', msg: 'upstream 5xx > 1% · 1.8% on /api/v2/checkout' },
          { at: '23:12:48', sev: 'info', msg: 'disk utilization > 90% · ip-10-0-4-73 · 92%' },
          { at: '23:12:09', sev: 'err',  msg: 'sq8 recall < 95% · embeddings index · 93.7%' },
          { at: '23:11:44', sev: 'warn', msg: 'memtable ratio > 0.8 · logs-prod · 0.84' },
          { at: '23:11:03', sev: 'info', msg: 'wal lag > 50ms · ingest-worker · 62ms' },
          { at: '23:10:41', sev: 'warn', msg: 'cache hit rate < 50% · /api/v2/search · 42%' },
        ]}),
      },

      { id: 'ruleAsCode', eyebrow: 'RULES AS CODE · EXAMPLE', cols: 12, type: 'markdown',
        render: () => Markdown(
`## This is what a rule looks like

A XERJ.ai alert rule is a JSON object. It lives in git, not in a database.
You diff it, review it, roll it back. No separate app, no YAML, no "Watcher"
vs "Alerting" vs "Rules" migration confusion.

\`\`\`
{
  "id":        "checkout-p95-latency",
  "name":      "Checkout p95 latency > 2s",
  "severity":  "critical",
  "query": {
    "index":   "metrics",
    "metric":  "query_latency_s{quantile=\\"0.95\\",service=\\"checkout-svc\\"}",
    "window":  "5m",
    "op":      ">",
    "value":   2.0
  },
  "dedupe":    "30m",
  "notify":    ["pagerduty/oncall", "slack/#oncall"],
  "runbook":   "https://runbook.internal/checkout-latency"
}
\`\`\`

That's all of it. Put it in \`rules/checkout-p95-latency.json\`. Commit.
The engine watches the directory and starts evaluating. If you delete the
file, the rule stops — no orphaned state.`
        ),
      },

      { id: 'citations', eyebrow: 'WHY THIS SECTION EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({
          items: [
            { id: 'kibana-alerting', source: 'github', score: 143,
              title: 'Allow Authors to Limit Interactivity',
              url: 'https://github.com/elastic/kibana/issues/9575' },
            { id: 'watcher-1',       source: 'discourse', score: 47,
              title: 'Watcher that counts the documents that arrive to an index in kibana',
              url: 'https://discuss.elastic.co/t/watcher-that-counts-the-documents-that-arrive-to-an-index-in-kibana/270609/23' },
            { id: 'alerting-noise',  source: 'reddit',    score: 22,
              title: 'Kibana alerting: noisy, silent, or broken — pick one',
              url: 'https://www.reddit.com/r/elasticsearch/' },
          ],
          total: 349,
        }),
      },

    ],
  }),
};
