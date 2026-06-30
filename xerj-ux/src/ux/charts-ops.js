// ============================================================
// XERJ.ai — Use-case primitives
//
// Shipping product features, not just pretty metrics:
//   • SearchBox    — interactive query console
//   • QueryDSL     — syntax-lit JSON of the query about to run
//   • QueryPlanTree — recursive 1px-connector plan tree from
//                     `POST /v1/indices/:name/explain-plan`
//   • Hits         — result list with _score, _index, _id, _source
//   • Facet        — "click to filter" terms-agg row
//   • AnomalyBand  — 1px series with upper/lower normal band
//                     and ×-marked anomaly ticks
//   • MetricTile   — ultra-dense key/value tile for ops panels
// ============================================================

import { esc, fmt } from './text.js';

const minMax = (xs) => {
  let mn = Infinity, mx = -Infinity;
  for (const x of xs) { if (x < mn) mn = x; if (x > mx) mx = x; }
  return [mn === Infinity ? 0 : mn, mx === -Infinity ? 0 : mx];
};

// ============================================================
// SearchBox — the core of the SEARCH dashboard
// ============================================================
/**
 * Query console. Renders a typography-only "terminal":
 *   [QUERY TYPE]                             [INDEX]
 *   ▸ <input>____________________________________
 *
 * Events bubble:
 *   [data-query-type] click → change query type
 *   [data-search-input] Enter keyup → run search
 *   [data-search-index] click → change index
 *
 *   value, activeType, activeIndex come from app state.
 *   types/indices drive the rows.
 */
export const SearchBox = ({
  value = '',
  types = ['match', 'term', 'range', 'prefix', 'phrase', 'knn', 'semantic', 'hybrid'],
  activeType = 'match',
  indices = ['*'],
  activeIndex = '*',
  filters = {},
  placeholder = "type a query · press Enter",
} = {}) => {
  // Filter pills row: shown only when at least one filter is active.
  // Each pill is a button with [data-facet-apply] which the existing app
  // click handler interprets as "toggle this filter off" (because clicking
  // a value that's already active removes it).
  const filterEntries = Object.entries(filters).filter(([, v]) => v != null && v !== '');
  const pillsRow = filterEntries.length ? `
  <div class="sbox-row sbox-pills">
    <span class="key" style="margin-right:12px;">FILTERS</span>
    ${filterEntries.map(([f, v]) =>
      `<button type="button" class="pill" data-facet-apply="${esc(f)}:${esc(v)}" title="Click to remove">
        <span class="pill-field">${esc(f)}</span><span class="pill-eq">=</span><span class="pill-val">${esc(v)}</span><span class="pill-x">✕</span>
      </button>`
    ).join('')}
    <button type="button" class="pill-clear" data-facet-clear>CLEAR ALL</button>
  </div>` : '';
  return `
<div class="sbox">
  <div class="sbox-row sbox-types">
    <span class="key" style="margin-right:12px;">QUERY</span>
    ${types.map((t) =>
      `<button type="button" data-query-type="${esc(t)}" class="${t === activeType ? 'active' : ''}">${esc(t.toUpperCase())}</button>`
    ).join('<span class="sep">·</span>')}
    <span class="spacer"></span>
    <span class="key" style="margin-right:12px;">INDEX</span>
    ${indices.map((i) =>
      `<button type="button" data-search-index="${esc(i)}" class="${i === activeIndex ? 'active' : ''}">${esc(i)}</button>`
    ).join('<span class="sep">·</span>')}
  </div>
  <div class="sbox-row sbox-input">
    <span class="prompt accent">▸</span>
    <input type="text" data-search-input value="${esc(value)}" placeholder="${esc(placeholder)}" autocomplete="off" spellcheck="false" aria-label="Search query"/>
  </div>
  ${pillsRow}
</div>`;
};

// ============================================================
// QueryDSL — the JSON about to be sent, with light syntax color
// ============================================================
/**
 * Shows the raw XERJ.ai query DSL that matches the current
 * SearchBox state. Keys are muted, strings are ink, numbers are
 * accent. No boxes, no frames.
 */
export const QueryDSL = (obj) => {
  const json = JSON.stringify(obj, null, 2);
  const out = esc(json)
    .replace(/&quot;([^&]+?)&quot;(\s*:)/g, '<span class="tok-key">"$1"</span>$2')
    .replace(/:\s*&quot;([^&]*?)&quot;/g, ': <span class="tok-str">"$1"</span>')
    .replace(/:\s*(-?\d+\.?\d*)/g, ': <span class="tok-num">$1</span>')
    .replace(/:\s*(true|false|null)\b/g, ': <span class="tok-kw">$1</span>');
  return `<pre class="qdsl mono">${out}</pre>`;
};

// ============================================================
// QueryPlanTree — what the planner picked, indented
// ============================================================
/**
 * Recursive render of a query plan tree. Each node has:
 *   { op, field?, value?, estimate?, cost?, children? }
 * Indent is typographic, not graphic. Connectors are ASCII.
 */
export const QueryPlanTree = (node, depth = 0, last = true) => {
  if (!node) return '';
  const prefix = depth === 0 ? '' : (last ? '└─ ' : '├─ ');
  const indent = '<span class="faint mono">' + ('│  '.repeat(Math.max(0, depth - 1))) + prefix + '</span>';
  const est = node.estimate != null ? ` <span class="faint mono">est <span class="mono" style="color:var(--z-ink);">${fmt(node.estimate)}</span></span>` : '';
  const cost = node.cost != null ? ` <span class="faint mono">cost <span class="mono" style="color:var(--z-ink);">${fmt(node.cost)}</span></span>` : '';
  const field = node.field ? `<span class="mono" style="color:var(--z-ink);"> ${esc(node.field)}</span>` : '';
  const val = node.value != null ? `<span class="mono faint"> = ${esc(node.value)}</span>` : '';
  const selfRow = `<div class="plan-row">${indent}<span class="plan-op accent mono">${esc(node.op)}</span>${field}${val}${est}${cost}</div>`;
  const kids = (node.children || []);
  const childRows = kids.map((c, i) => QueryPlanTree(c, depth + 1, i === kids.length - 1)).join('');
  return selfRow + childRows;
};

// ============================================================
// Hits — search result list
// ============================================================
/**
 * Renders the hits returned by a search. Each row:
 *   <index>  <id>        <score>   <body>
 * Body is a one-line preview of _source. No frames.
 *
 *   hits:        [{ _index, _id, _score, _ts, _source }]
 *   total,
 *   tookMs       — header line
 *   sort:        { field: '_score'|'_ts'|'_index'|'_id', dir: 'asc'|'desc' }
 *   showTime:    boolean — whether to render the _ts column (default true)
 *   labels:      { _index: 'INDEX', _id: 'ID', _score: 'SCORE', _ts: 'TIME' }
 *   exportable:  boolean — render the EXPORT CSV link in the header (default true)
 */
export const Hits = ({
  hits = [], total = 0, tookMs = 0, maxScore = null,
  sort = { field: '_score', dir: 'desc' },
  showTime = true,
  labels = { _index: 'INDEX', _id: 'ID', _score: 'SCORE', _ts: 'TIME' },
  exportable = true,
} = {}) => {
  const sortIndicator = (field) => {
    if (sort?.field !== field) return '';
    return sort.dir === 'asc' ? ' ▲' : ' ▼';
  };
  const colHeader = (field, label) =>
    `<button type="button" class="col-sort ${sort?.field === field ? 'active' : ''}" data-sort-field="${esc(field)}" data-sort-dir="${esc(sort?.field === field && sort.dir === 'desc' ? 'asc' : 'desc')}">${esc(label)}${sortIndicator(field)}</button>`;

  const header = `
    <div class="hits-meta">
      <span class="key">HITS</span>
      <span class="mono" style="margin-left:var(--sp-3);"><span class="accent" style="font-size:var(--fs-20); font-weight:700;">${fmt(total)}</span> <span class="faint">documents</span></span>
      <span class="mono" style="margin-left:var(--sp-3);"><span class="accent">${fmt(tookMs)}</span> <span class="faint">ms</span></span>
      ${maxScore != null ? `<span class="mono" style="margin-left:var(--sp-3);"><span class="faint">max_score</span> ${maxScore.toFixed(3)}</span>` : ''}
      <span style="flex:1;"></span>
      ${exportable ? `<button type="button" class="hits-action" data-export-csv title="GH#1992 · 372 reactions">↓ CSV</button>` : ''}
      <button type="button" class="hits-action" data-toggle-time aria-pressed="${showTime ? 'true' : 'false'}" title="GH#3319 · 44 reactions">${showTime ? 'HIDE TIME' : 'SHOW TIME'}</button>
    </div>`;

  if (!hits.length) {
    return header + `<div class="mono faint" style="padding:var(--sp-3) 0;">No matches. Try a broader query or a different query type.</div>`;
  }

  const colHead = `
    <div class="hit-head hit-headrow ${showTime ? '' : 'no-time'}">
      ${colHeader('_index', labels._index)}
      ${colHeader('_id', labels._id)}
      ${colHeader('_score', labels._score)}
      ${showTime ? colHeader('_ts', labels._ts) : ''}
      <span class="faint">${esc(labels._source || 'MESSAGE')}</span>
    </div>`;

  const rows = hits.map((h) => {
    const body = typeof h._source === 'string' ? h._source : JSON.stringify(h._source);
    return `
      <div class="hit">
        <div class="hit-head mono ${showTime ? '' : 'no-time'}">
          <button type="button" class="hit-cell-clickable" data-facet-apply="_index:${esc(h._index)}" title="Filter for this">${esc(h._index)}</button>
          <span class="faint">${esc(h._id)}</span>
          <span class="accent">${(h._score ?? 0).toFixed(3)}</span>
          ${showTime ? `<span class="faint">${esc(h._ts || '')}</span>` : ''}
        </div>
        <div class="hit-body mono">${esc(body)}</div>
      </div>`;
  }).join('');
  return header + colHead + `<div class="hits-list">${rows}</div>`;
};

// ============================================================
// Facet — a single terms-agg row as a clickable filter
// ============================================================
/**
 * One facet block: an eyebrow label + N rows where each row is
 * a click target `[data-facet-apply="field:value"]`.
 *
 *   field:  e.g. 'level'
 *   items:  [{ label, value, count }]
 *   active: currently applied value (for styling)
 */
export const Facet = ({ field, items = [], active = null } = {}) => {
  if (!items.length) return '';
  const max = Math.max(...items.map((i) => i.count));
  const rows = items.map((i) => {
    const frac = i.count / (max || 1);
    const x = (180 * frac).toFixed(1);
    const on = i.value === active;
    return `
      <button type="button" class="facet-row ${on ? 'active' : ''}" data-facet-apply="${esc(field)}:${esc(i.value)}">
        <span class="facet-label">${esc(i.label)}</span>
        <span class="facet-count mono">${fmt(i.count)}</span>
        <svg class="chart facet-bar" height="5" viewBox="0 0 180 5" preserveAspectRatio="none">
          <line x1="0" y1="4" x2="${x}" y2="4" stroke="currentColor" stroke-width="1"/>
        </svg>
      </button>`;
  }).join('');
  return `<div class="facet"><div class="key" style="margin-bottom:6px;">${esc(field.toUpperCase())}</div>${rows}</div>`;
};

// ============================================================
// AnomalyBand — 1px line + rolling normal band + × marks
// ============================================================
/**
 * A time series with a rolling normal band computed upstream
 * (mean ± k*std). Anomalies are points outside the band; the
 * caller passes their indices.
 *
 *   values, upper, lower:   same length
 *   anomalies:              [{ idx, score }]
 *   h:                      intrinsic SVG height
 *   labels:                 ['START','END']
 */
export const AnomalyBand = ({
  values = [], upper = [], lower = [],
  anomalies = [], h = 200, labels = ['', ''], unit = '',
} = {}) => {
  if (!values.length) return '';
  const W = 1200;
  const all = values.concat(upper, lower);
  const [mn, mx] = minMax(all);
  const range = mx - mn || 1;
  const pad = 10;
  const py = (v) => pad + (h - pad * 2) - (((v - mn) / range) * (h - pad * 2));
  const px = (i, n) => ((i / (n - 1)) * W);
  const pts = (arr) => arr.map((v, i) => `${px(i, arr.length).toFixed(1)},${py(v).toFixed(1)}`).join(' ');
  const upperLine = upper.length ? `<polyline points="${pts(upper)}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="0.22"/>` : '';
  const lowerLine = lower.length ? `<polyline points="${pts(lower)}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="0.22"/>` : '';
  const valueLine = `<polyline points="${pts(values)}" fill="none" stroke="currentColor" stroke-width="1"/>`;
  const marks = anomalies.map(({ idx }) => {
    const x = px(idx, values.length).toFixed(1);
    const y = py(values[idx]).toFixed(1);
    return `
      <line x1="${x}" y1="${pad}" x2="${x}" y2="${h - pad}" stroke="var(--z-accent)" stroke-width="1" stroke-opacity="0.4"/>
      <text x="${x}" y="${(py(values[idx]) - 8).toFixed(1)}" fill="var(--z-accent)" font-family="var(--font-data)" font-size="14" text-anchor="middle">×</text>
    `;
  }).join('');
  const u = unit ? ' ' + esc(unit) : '';
  return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
    ${upperLine}${lowerLine}${valueLine}${marks}
  </svg>
  <div class="series-legend">
    <span>${esc(labels[0] || '')} · <span class="mono" style="color:var(--z-ink);">${fmt(values[0])}${u}</span></span>
    <span class="mid">normal band · <span class="mono" style="color:var(--z-ink);">${fmt(Math.min(...lower))}..${fmt(Math.max(...upper))}${u}</span> · <span class="accent">${anomalies.length}</span> anomalies</span>
    <span><span class="mono" style="color:var(--z-ink);">${fmt(values[values.length - 1])}${u}</span> · ${esc(labels[1] || '')}</span>
  </div>`;
};

// ============================================================
// Citations — the link from a panel back to real user pain
// ============================================================
/**
 * Renders a row of citations attributing the dashboard to specific
 * user-feedback artifacts. Each entry comes from
 * `data/feedback-citations.js`, which is auto-generated from the
 * Kibana feedback corpus.
 *
 *   items: [{ id, source, score, title, url }]
 */
export const Citations = ({ items = [], total = null } = {}) => {
  if (!items.length) {
    return `<div class="mono faint">No citations available — re-run <span class="accent">node user-feedback/kibana/pipeline/design-inputs.mjs</span></div>`;
  }
  const rows = items.map((c) => `
    <a class="cite" href="${esc(c.url)}" target="_blank" rel="noopener">
      <span class="cite-src mono faint">${esc((c.source || '').toUpperCase())}</span>
      <span class="cite-score mono accent">${c.score || 0}</span>
      <span class="cite-title">${esc(c.title || '— no title —')}</span>
      <span class="cite-arrow mono faint">↗</span>
    </a>
  `).join('');
  const meta = total != null
    ? `<div class="hint" style="margin-bottom:var(--sp-2);">drawn from <span class="mono accent">${total}</span> ranked artifacts in <span class="mono">user-feedback/kibana/themes/design-inputs.md</span></div>`
    : '';
  return meta + `<div class="cites">${rows}</div>`;
};

// ============================================================
// MetricTile — ultra-dense key/value row for ops dashboards
// ============================================================
/**
 * Not every metric deserves a 96px Num. MetricTile is a compact
 * row of label + value + optional spark/delta, six to a row.
 *
 *   tiles: [{ label, value, unit?, spark?, delta?, deltaGood? }]
 */
export const MetricTile = ({ tiles = [] } = {}) => {
  const cells = tiles.map((t) => `
    <div class="mtile">
      <div class="mtile-head">
        <span class="key">${esc(t.label)}</span>
        ${t.delta != null ? `<span class="delta ${(t.deltaGood === 'down' ? (t.delta <= 0 ? 'good' : 'bad') : (t.delta >= 0 ? 'good' : 'bad'))}" style="font-size:var(--fs-11);">${t.delta >= 0 ? '▲' : '▼'} ${Math.abs(t.delta).toFixed(1)}%</span>` : ''}
      </div>
      <div class="mtile-val mono">${esc(t.value)}<span class="num-unit">${esc(t.unit || '')}</span></div>
      ${t.spark || ''}
    </div>`).join('');
  return `<div class="mtiles">${cells}</div>`;
};
