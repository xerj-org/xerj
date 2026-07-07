// ============================================================
// XERJ.ai — Chart primitives (extended set)
// Kibana-parity chart types in typography-first style.
// All stroke widths are 1px. Scatter uses text characters for
// data points — the only "point markers" we allow because the
// data IS the character.
// ============================================================

import { esc, fmt } from './text.js';

const minMax = (xs) => {
  let mn = Infinity, mx = -Infinity;
  for (const x of xs) { if (x < mn) mn = x; if (x > mx) mx = x; }
  return [mn === Infinity ? 0 : mn, mx === -Infinity ? 0 : mx];
};

// ----- VBar / Hist -------------------------------------------

/**
 * VBar — vertical 1px-line bar chart. Replaces Kibana "Bar" / "Histogram".
 *
 *   items:  [{ label, value }]
 *   h:      intrinsic viewBox height
 *   unit:   optional unit in the legend
 *   labels: true → render label strip under the chart
 */
export const VBar = ({ items, h = 160, unit = '', labels = true } = {}) => {
  if (!items || !items.length) return '';
  const w = 1200;
  const values = items.map((i) => i.value);
  const [mn, mx] = minMax(values);
  const denom = mx || 1;
  const step = w / items.length;
  const lines = items.map((it, i) => {
    const x = ((i + 0.5) * step).toFixed(1);
    const y = (h - (it.value / denom) * h).toFixed(1);
    return `<line x1="${x}" y1="${h}" x2="${x}" y2="${y}" stroke="currentColor" stroke-width="1"/>`;
  }).join('');
  const first = items[0]?.label ?? '';
  const last  = items[items.length - 1]?.label ?? '';
  const u = unit ? ' ' + esc(unit) : '';
  const legend = labels ? `
    <div class="series-legend">
      <span>${esc(first)}</span>
      <span class="mid">min <span class="mono" style="color:var(--z-ink);">${fmt(mn)}</span>   ·   max <span class="mono" style="color:var(--z-ink);">${fmt(mx)}${u}</span></span>
      <span>${esc(last)}</span>
    </div>` : '';
  return `
  <div class="series-wrap">
    <svg class="chart" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
      ${lines}
    </svg>
    ${legend}
  </div>`;
};

/** Hist — semantic alias for VBar (column histogram of counts). */
export const Hist = VBar;

// ----- Gauge -------------------------------------------------

/**
 * Gauge — a single value on a 1px track between min and max.
 *
 *   value, min, max, unit
 *   thresholds: optional [t1, t2, ...] — rendered as tiny 1px ticks
 *   emphasis:   true → value rendered in 56px accent (default)
 */
export const Gauge = ({
  value,
  min = 0,
  max = 100,
  unit = '',
  thresholds = [],
  emphasis = true,
  label = '',
} = {}) => {
  const w = 600, h = 14;
  const v = Math.max(min, Math.min(max, value));
  const frac = (v - min) / (max - min || 1);
  const x = (frac * w).toFixed(1);
  const ticks = thresholds.map((t) => {
    const tx = (((t - min) / (max - min || 1)) * w).toFixed(1);
    return `<line x1="${tx}" y1="0" x2="${tx}" y2="${h}" stroke="currentColor" stroke-width="1" stroke-opacity="0.4"/>`;
  }).join('');
  const valCls = emphasis ? 'num-big accent' : 'num-md';
  return `
  <div class="stack">
    <div class="row-flex" style="justify-content:space-between;">
      <div class="${valCls}">${esc(fmt(value))}<span class="num-unit">${esc(unit)}</span></div>
      ${label ? `<span class="hint">${esc(label)}</span>` : ''}
    </div>
    <svg class="chart" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px; margin-top:12px;">
      <line x1="0" y1="${h - 1}" x2="${w}" y2="${h - 1}" stroke="currentColor" stroke-width="1" stroke-opacity="0.18"/>
      <line x1="0" y1="${h - 1}" x2="${x}" y2="${h - 1}" stroke="currentColor" stroke-width="1"/>
      ${ticks}
    </svg>
    <div class="row-flex" style="justify-content:space-between; margin-top:6px; font-family:var(--font-data); font-size:var(--fs-11); color:var(--z-mute);">
      <span>${esc(fmt(min))}</span>
      <span>${esc(fmt(max))}${unit ? ' ' + esc(unit) : ''}</span>
    </div>
  </div>`;
};

// ----- Scatter -----------------------------------------------

/**
 * Scatter — x/y point cloud using single characters as data points.
 * Pure typography: each point is a `·` placed absolutely. The
 * character IS the datum, which is why scatter is the only chart
 * that has "point markers".
 *
 *   points: [[x, y], ...]
 *   xLabel, yLabel: text shown in the end legend
 */
export const Scatter = ({ points, h = 220, xLabel = 'X', yLabel = 'Y' } = {}) => {
  if (!points || !points.length) return '';
  const xs = points.map((p) => p[0]);
  const ys = points.map((p) => p[1]);
  const [xmn, xmx] = minMax(xs); const xr = xmx - xmn || 1;
  const [ymn, ymx] = minMax(ys); const yr = ymx - ymn || 1;
  const dots = points.map(([x, y]) => {
    const px = (((x - xmn) / xr) * 100).toFixed(2);
    const py = (((ymx - y) / yr) * 100).toFixed(2);
    return `<span style="position:absolute; left:${px}%; top:${py}%; transform:translate(-50%,-50%); font-family:var(--font-data); font-size:var(--fs-16); color:var(--z-ink); line-height:1; pointer-events:none;">·</span>`;
  }).join('');
  return `
  <div style="position:relative; width:100%; height:${h}px;">${dots}</div>
  <div class="series-legend">
    <span>${esc(xLabel)} <span class="mono" style="color:var(--z-ink);">${fmt(xmn)}..${fmt(xmx)}</span></span>
    <span class="mid"><span class="mono" style="color:var(--z-ink);">${points.length}</span> points</span>
    <span>${esc(yLabel)} <span class="mono" style="color:var(--z-ink);">${fmt(ymn)}..${fmt(ymx)}</span></span>
  </div>`;
};

// ----- Stacked -----------------------------------------------

/**
 * Stacked — horizontal stacked bars, one per row. Replaces Kibana
 * "Stacked bar". Each row's segments are distinguished by opacity.
 *
 *   rows: [{ label, segments: [{ label, value }] }]
 */
export const Stacked = ({ rows, width = 1200, height = 10, showSegments = true } = {}) => {
  if (!rows || !rows.length) return '';
  const segmentLabels = showSegments ? rows[0].segments.map((s) => esc(s.label)).join(' · ') : '';
  const body = rows.map((row) => {
    const total = row.segments.reduce((a, s) => a + s.value, 0) || 1;
    let x = 0;
    const lines = row.segments.map((s, i) => {
      const segW = (s.value / total) * width;
      const op = Math.max(0.14, 1 - i * 0.16);
      const line = segW > 0.5
        ? `<line x1="${x.toFixed(1)}" y1="${(height - 1).toFixed(1)}" x2="${(x + segW).toFixed(1)}" y2="${(height - 1).toFixed(1)}" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>`
        : '';
      x += segW;
      return line;
    }).join('');
    return `
      <div class="stk-row">
        <span class="row__label">${esc(row.label)}</span>
        <svg class="chart stk-bar" viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" style="height:${height}px;">${lines}</svg>
        <span class="mono faint" style="text-align:right;">${fmt(total)}</span>
      </div>`;
  }).join('');
  const legend = segmentLabels ? `<div class="hint" style="margin-top:var(--sp-2);">${segmentLabels}</div>` : '';
  return `<div class="stacked">${body}</div>${legend}`;
};

// ----- Treemap (text hierarchy) ------------------------------

/**
 * Treemap — nested ranked list with indent and 1px bars. Replaces
 * Kibana "Treemap" (and arguably "Sunburst"). Accepts a tree of
 * { label, value, children? } nodes.
 */
export const Treemap = ({ items, depth = 0, parentTotal = null } = {}) => {
  if (!items || !items.length) return '';
  const total = parentTotal ?? (items.reduce((a, i) => a + i.value, 0) || 1);
  const max = Math.max(...items.map((i) => i.value));
  return items.map((it) => {
    const frac = it.value / max;
    const pct = ((it.value / total) * 100).toFixed(1);
    const x = (200 * frac).toFixed(1);
    const indent = '·  '.repeat(depth);
    const kids = it.children && it.children.length
      ? Treemap({ items: it.children, depth: depth + 1, parentTotal: it.value })
      : '';
    return `
      <div class="row">
        <div class="row__label"><span class="faint">${indent}</span>${esc(it.label)}</div>
        <div class="row__val">${esc(fmt(it.value))}</div>
        <div class="row__bar">
          <svg class="chart" height="6" viewBox="0 0 200 6" preserveAspectRatio="none">
            <line x1="0" y1="5" x2="${x}" y2="5" stroke="currentColor" stroke-width="1" stroke-opacity="${(1 - depth * 0.2).toFixed(2)}"/>
          </svg>
        </div>
        <div class="row__pct">${pct}%</div>
      </div>
      ${kids}`;
  }).join('');
};
