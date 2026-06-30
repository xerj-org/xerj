// ============================================================
// XERJ.ai — Chart primitives
// Every chart is 1px strokes or text. No fills, no dots, no
// axes, no grid, no legends. See UX_BOOK.md §3.
// ============================================================

import { esc, fmt } from './text.js';

const minMax = (xs) => {
  let mn = Infinity, mx = -Infinity;
  for (const x of xs) { if (x < mn) mn = x; if (x > mx) mx = x; }
  return [mn === Infinity ? 0 : mn, mx === -Infinity ? 0 : mx];
};

const buildPoints = (values, w, h) => {
  const [mn, mx] = minMax(values);
  const range = mx - mn || 1;
  const n = values.length;
  const step = n > 1 ? w / (n - 1) : w;
  let out = '';
  for (let i = 0; i < n; i++) {
    const x = (i * step).toFixed(2);
    const y = (h - ((values[i] - mn) / range) * h).toFixed(2);
    out += (i ? ' ' : '') + x + ',' + y;
  }
  return { pts: out, mn, mx };
};

// ----- Sparkline ---------------------------------------------

/**
 * Spark — a small inline 1px line chart. No axes, no dots, no fill.
 * Drop next to a Num for shape context.
 */
export const Spark = (values, { w = 140, h = 28, strokeOpacity = 1 } = {}) => {
  if (!values || values.length < 2) return '';
  const { pts } = buildPoints(values, w, h);
  return `<svg class="chart" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" aria-hidden="true">
    <polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${strokeOpacity}"/>
  </svg>`;
};

// ----- Full-width time series --------------------------------

/**
 * Series — a full-width 1px time line.
 * No axes. Endpoint labels and min/peak are annotated BELOW the curve,
 * never floating over it.
 *
 *   values:    number[]
 *   h:         intrinsic viewBox height (pixels); width stretches to container
 *   labels:    ['start label', 'end label']
 *   unit:      optional unit string shown with min/peak
 */
export const Series = (values, {
  h = 140,
  labels = ['', ''],
  unit = '',
} = {}) => {
  if (!values || values.length < 2) return '';
  const w = 1200;
  const { pts, mn, mx } = buildPoints(values, w, h);
  const u = unit ? ' ' + esc(unit) : '';
  return `
  <div class="series-wrap">
    <svg class="chart series" height="${h}" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none">
      <polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1"/>
    </svg>
    <div class="series-legend">
      <span>${esc(labels[0] ?? '')} · <span class="mono" style="color:var(--z-ink);">${fmt(values[0])}${u}</span></span>
      <span class="mid">min <span class="mono" style="color:var(--z-ink);">${fmt(mn)}${u}</span>   ·   peak <span class="mono" style="color:var(--z-ink);">${fmt(mx)}${u}</span></span>
      <span><span class="mono" style="color:var(--z-ink);">${fmt(values[values.length - 1])}${u}</span> · ${esc(labels[1] ?? '')}</span>
    </div>
  </div>`;
};

// ----- Distribution (replaces pies & donuts) -----------------

/**
 * Dist — a single horizontal 1px line broken into parts by stroke-opacity.
 * Below it, each segment renders as a labeled micro-metric.
 *
 *   segments: [{ label, value }]
 *   width:    svg viewBox width
 */
export const Dist = ({ segments, width = 1200, height = 10 } = {}) => {
  if (!segments || !segments.length) return '';
  const total = segments.reduce((a, s) => a + s.value, 0) || 1;
  let x = 0;
  const lines = [];
  segments.forEach((s, i) => {
    const segW = (s.value / total) * width;
    const op = Math.max(0.12, 1 - i * 0.14);
    if (segW > 0.5) {
      lines.push(
        `<line x1="${x.toFixed(1)}" y1="${(height - 1).toFixed(1)}" x2="${(x + segW).toFixed(1)}" y2="${(height - 1).toFixed(1)}" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>`
      );
    }
    x += segW;
  });
  const legend = segments.map((s, i) => {
    const pct = (100 * s.value / total).toFixed(1);
    return `
      <div class="stack" style="gap:6px;">
        <span class="key">${esc(s.label)}</span>
        <span class="mono" style="font-size:var(--fs-20); font-weight:700;">${fmt(s.value)}</span>
        <span class="mono faint" style="font-size:var(--fs-11);">${pct}%</span>
      </div>`;
  }).join('');
  return `
    <svg class="chart dist-bar" viewBox="0 0 ${width} ${height}" preserveAspectRatio="none">${lines.join('')}</svg>
    <div class="dist-legend">${legend}</div>
  `;
};

// ----- Character-intensity heatmap ---------------------------

/**
 * Heatmap — a grid of NUMBERS whose text opacity encodes magnitude. The
 * data IS the visualization. No boxes, no cells, no colors.
 *
 *   rows:     string[] (row labels)
 *   cols:     string[] (col labels)
 *   matrix:   number[][] (rows × cols)
 *   cellFmt:  (v) => string — how to render each cell (default: compact)
 */
export const Heatmap = ({ rows, cols, matrix, cellFmt = (v) => fmt(v) } = {}) => {
  if (!matrix || !matrix.length) return '';
  const flat = matrix.flat();
  const [mn, mx] = minMax(flat);
  const range = mx - mn || 1;
  // Cell width = max rendered length + 1 space, so columns align.
  let maxLen = 0;
  for (const v of flat) {
    const l = String(cellFmt(v)).length;
    if (l > maxLen) maxLen = l;
  }
  const widthCh = Math.max(3, maxLen + 1);
  const header = `<div class="heatmap-row heatmap-head"><span class="row-label">\u00a0</span>${cols.map(c =>
    `<span style="display:inline-block; width:${widthCh}ch; text-align:right; color:var(--z-mute);">${esc(c)}</span>`
  ).join('')}</div>`;
  const body = matrix.map((row, i) => {
    const cells = row.map((v) => {
      const t = (v - mn) / range;
      const op = (0.16 + 0.84 * t).toFixed(2);
      return `<span style="display:inline-block; width:${widthCh}ch; text-align:right; opacity:${op};">${esc(cellFmt(v))}</span>`;
    }).join('');
    return `<div class="heatmap-row"><span class="row-label">${esc(rows[i] ?? '')}</span>${cells}</div>`;
  }).join('');
  return `<div class="heatmap">${header}${body}</div>`;
};

// ----- Small multiples ---------------------------------------

/**
 * Multiples — grid of labeled sparklines.
 *   items: [{ label, values, value }]
 */
export const Multiples = ({ items, w = 160, h = 22 } = {}) => {
  if (!items || !items.length) return '';
  const cells = items.map(({ label, values, value }) => `
    <div class="mult">
      <div class="mult-head">
        <span class="name">${esc(label)}</span>
        <span class="val">${value != null ? esc(value) : ''}</span>
      </div>
      ${Spark(values, { w, h })}
    </div>
  `).join('');
  return `<div class="multiples">${cells}</div>`;
};
