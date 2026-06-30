// ============================================================
// Section — DATA
//
// Cluster / index / field browser. This is the product surface
// for "what data does the engine have, where does it live,
// what fields are indexed, and how are they encoded".
//
// The three functions in data/data-sources.js are the ONLY
// place that swaps from mock to live fetch when the engine is
// ready. When the backend ships /v1/clusters + /v1/indices,
// this page becomes live automatically.
// ============================================================

import { esc }                 from '../ux/text.js';
import { Citations }           from '../ux/charts-ops.js';
import { Markdown }            from '../ux/tables.js';
import { defaultClusterId }    from '../data/data-sources.js';

const humanCount = (n) => {
  if (n == null) return '—';
  if (n >= 1e9) return (n / 1e9).toFixed(1) + 'B';
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(0) + 'K';
  return String(n);
};
const humanBytes = (b) => {
  if (b == null) return '—';
  const u = ['B','KB','MB','GB','TB','PB'];
  let v = b, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return v.toFixed(v < 10 ? 1 : 0) + ' ' + u[i];
};

export const dataSection = {
  id: 'data',
  name: 'Data',
  section: 'data',
  render: ({ data, time }) => {
    const clusters = data.clusters || [];
    const indicesByCluster = data.indicesByCluster || {};
    const fieldsByIndex = data.fieldsByIndex || {};
    const active = data.activeCluster || 'local';
    const activeIndices = indicesByCluster[active] || [];
    const focusIndex = data.focusIndex || activeIndices[0]?.name;
    const fields = fieldsByIndex[focusIndex] || [];

    return {
      title: 'DATA',
      kicker: 'CLUSTERS · INDICES · FIELDS',
      meta: [time, 'SOURCES'],
      caption: 'What the engine actually has. Every row on this page maps to a real endpoint: `GET /v1/clusters`, `GET /v1/clusters/:id/indices`, `GET /v1/indices/:name/_mapping`. The mock values flip to live fetch the day the backend ships — nothing else on this page changes.',
      panels: [

        { id: 'clusters', eyebrow: 'CLUSTERS · CLICK TO SET DEFAULT', cols: 12, type: 'clusters',
          render: () => renderClusters(clusters, active),
        },

        { id: 'indices', eyebrow: `INDICES · ${active.toUpperCase()} · CLICK AN INDEX TO INSPECT FIELDS`, cols: 6, type: 'indices',
          render: () => renderIndices(activeIndices, focusIndex),
        },

        { id: 'fields', eyebrow: `FIELDS · ${focusIndex || '—'} · FROM /v1/indices/:name/_mapping`, cols: 6, type: 'fields',
          render: () => renderFields(fields),
        },

        { id: 'howTo', eyebrow: 'CONNECTING A NEW CLUSTER', cols: 12, type: 'markdown',
          render: () => Markdown(
`## Point XERJ.ai at a cluster

Today, clusters are defined in \`src/data/data-sources.js\` and read from
mock arrays. **When the engine ships** the HTTP bindings that this page
expects, clusters will be configured through this UI instead.

\`\`\`
POST /v1/clusters
{
  "id":   "prod-us",
  "name": "PROD-US",
  "url":  "https://xerj-us-east-1.internal:8080",
  "auth": { "type": "bearer", "token": "$XERJ_TOKEN" }
}
\`\`\`

The current default cluster is stored in \`localStorage.xerj.cluster\`,
which you can inspect under SETTINGS. Every query goes to that cluster
unless a specific dashboard panel overrides it via its \`source: { cluster }\`
binding.`
          ),
        },

        { id: 'citations', eyebrow: 'WHY THIS SECTION EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
          render: () => Citations({
            items: [
              { id: 'gh-6498',  source: 'github', score: 57,
                title: 'Remove index pattern mapping cache',
                url: 'https://github.com/elastic/kibana/issues/6498' },
              { id: 'gh-17888', source: 'github', score: 45,
                title: 'Per-user profiles, settings in Kibana',
                url: 'https://github.com/elastic/kibana/issues/17888' },
              { id: 'gh-17542', source: 'github', score: 57,
                title: 'Ability to change the index pattern on a visualization',
                url: 'https://github.com/elastic/kibana/issues/17542' },
            ],
            total: 451,
          }),
        },

      ],
    };
  },
};

// ---------- renderers -----------------------------------

function renderClusters(clusters, active) {
  if (!clusters.length) return '<div class="mono faint">No clusters configured.</div>';
  const rows = clusters.map((c) => {
    const isActive = c.id === active;
    const status = {
      green:  `<span class="mono accent">●</span>`,
      yellow: `<span class="mono faint">◐</span>`,
      red:    `<span class="mono">○</span>`,
    }[c.status] || '—';
    return `
      <button type="button" class="mg-cluster${isActive ? ' mg-cluster-active' : ''}" data-mg-cluster="${esc(c.id)}">
        <span class="mg-cluster-status">${status}</span>
        <span class="mg-cluster-name mono${isActive ? ' accent' : ''}">${esc(c.name)}</span>
        <span class="mg-cluster-url mono faint">${esc(c.url)}</span>
        <span class="mg-cluster-stat mono">${humanCount(c.indices)}&nbsp;idx</span>
        <span class="mg-cluster-stat mono">${humanCount(c.docs)}&nbsp;docs</span>
        <span class="mg-cluster-ver mono faint">${esc(c.version)}</span>
      </button>`;
  }).join('');
  return `<div class="mg-clusters">${rows}</div>`;
}

function renderIndices(indices, focusIndex) {
  if (!indices.length) return '<div class="mono faint">No indices in this cluster.</div>';
  const cols = ['NAME', 'DOCS', 'SIZE', 'SHARDS', 'RETENTION'];
  const headRow = `<div class="mg-idx-row mg-idx-head">${cols.map((c) => `<span>${esc(c)}</span>`).join('')}</div>`;
  const body = indices.map((i) => {
    const cells = [
      `<button type="button" class="mg-idx-btn${i.name === focusIndex ? ' active' : ''}" data-mg-index="${esc(i.name)}">${esc(i.name)}</button>`,
      humanCount(i.docs),
      humanBytes(i.bytes),
      String(i.shards),
      i.retention_days == null ? '∞' : i.retention_days + 'd',
    ];
    return `<div class="mg-idx-row">${cells.map((c) => `<span>${c}</span>`).join('')}</div>`;
  }).join('');
  return `<div class="mg-idx-table">${headRow}${body}</div>`;
}

function renderFields(fields) {
  if (!fields.length) return '<div class="mono faint">No mapping for this index.</div>';
  const cols = ['FIELD', 'TYPE', 'CARDINALITY', 'ENCODING', 'RATIO'];
  const headRow = `<div class="mg-fld-row mg-fld-head">${cols.map((c) => `<span>${esc(c)}</span>`).join('')}</div>`;
  const body = fields.map((f) => `
    <div class="mg-fld-row">
      <span class="mono">${esc(f.name)}</span>
      <span class="mono faint">${esc(f.type)}</span>
      <span class="mono">${humanCount(f.cardinality)}</span>
      <span class="mono faint">${esc(f.encoding)}</span>
      <span class="mono">${(f.ratio * 100).toFixed(0)}%</span>
    </div>`).join('');
  return `<div class="mg-fld-table">${headRow}${body}</div>`;
}
