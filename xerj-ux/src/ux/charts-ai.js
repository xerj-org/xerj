// ============================================================
// XERJ.ai — AI / RAG visualization primitives
//
// Typography-first takes on the viz types people reach for when
// building AI dashboards: embedding spaces, attention maps, RAG
// retrieval flow, multi-dim parallel coordinates, and axonometric
// "3D" ribbon plots. All strokes are 1px.
// ============================================================

import { esc, fmt } from './text.js';

const minMax = (xs) => {
  let mn = Infinity, mx = -Infinity;
  for (const x of xs) { if (x < mn) mn = x; if (x > mx) mx = x; }
  return [mn === Infinity ? 0 : mn, mx === -Infinity ? 0 : mx];
};

// Andrew's monotone chain — O(n log n) convex hull.
function convexHull(points) {
  if (points.length < 3) return points.slice();
  const pts = points.slice().sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  const cross = (O, A, B) => (A[0] - O[0]) * (B[1] - O[1]) - (A[1] - O[1]) * (B[0] - O[0]);
  const lower = [];
  for (const p of pts) {
    while (lower.length >= 2 && cross(lower[lower.length - 2], lower[lower.length - 1], p) <= 0) lower.pop();
    lower.push(p);
  }
  const upper = [];
  for (let i = pts.length - 1; i >= 0; i--) {
    const p = pts[i];
    while (upper.length >= 2 && cross(upper[upper.length - 2], upper[upper.length - 1], p) <= 0) upper.pop();
    upper.push(p);
  }
  upper.pop(); lower.pop();
  return lower.concat(upper);
}

// ==============================================================
// EmbedSpace — 2D projection of embeddings, 1px cluster hulls
// ==============================================================
/**
 * The iconic AI visualization: a 2D projection of high-dimensional
 * embeddings, colored (here: opacity'd) by cluster, with 1px convex
 * hulls and centroid labels.
 *
 *   clusters: [{ label, points: [[x,y], ...], centroid: [x,y] }]
 *   h:        svg viewBox height
 *   highlight: optional [x,y] — rendered as a crosshair (e.g. the
 *              current query's projected embedding)
 */
export const EmbedSpace = ({ clusters = [], h = 360, highlight = null } = {}) => {
  if (!clusters.length) return '';
  const W = 1200;
  const allPts = clusters.flatMap((c) => c.points);
  if (!allPts.length) return '';
  const [xmn, xmx] = minMax(allPts.map((p) => p[0]));
  const [ymn, ymx] = minMax(allPts.map((p) => p[1]));
  const xr = xmx - xmn || 1;
  const yr = ymx - ymn || 1;
  const pad = 40;
  const px = (x) => pad + ((x - xmn) / xr) * (W - pad * 2);
  const py = (y) => pad + (1 - (y - ymn) / yr) * (h - pad * 2);

  const dots = clusters.flatMap((c, ci) => {
    const op = Math.max(0.35, 1 - ci * 0.09);
    return c.points.map(([x, y]) =>
      `<text x="${px(x).toFixed(1)}" y="${py(y).toFixed(1)}" fill="currentColor" opacity="${op.toFixed(2)}" font-family="var(--font-data)" font-size="14" text-anchor="middle" dominant-baseline="middle">·</text>`
    );
  }).join('');

  const hulls = clusters.map((c, ci) => {
    if (c.points.length < 3) return '';
    const h2 = convexHull(c.points);
    if (h2.length < 3) return '';
    const closed = h2.concat([h2[0]]);
    const pts = closed.map(([x, y]) => `${px(x).toFixed(1)},${py(y).toFixed(1)}`).join(' ');
    const op = Math.max(0.2, 0.55 - ci * 0.07);
    return `<polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>`;
  }).join('');

  const labels = clusters.map((c) => {
    const [cx, cy] = c.centroid || c.points[0];
    return `<text x="${px(cx).toFixed(1)}" y="${(py(cy) - 14).toFixed(1)}" fill="currentColor" font-family="var(--font-prose)" font-size="11" font-weight="700" letter-spacing="1.5" text-anchor="middle">${esc((c.label || '').toUpperCase())}</text>`;
  }).join('');

  const hl = highlight ? `
    <line x1="${px(highlight[0]).toFixed(1)}" y1="0" x2="${px(highlight[0]).toFixed(1)}" y2="${h}" stroke="var(--z-accent)" stroke-width="1" stroke-opacity="0.5"/>
    <line x1="0" y1="${py(highlight[1]).toFixed(1)}" x2="${W}" y2="${py(highlight[1]).toFixed(1)}" stroke="var(--z-accent)" stroke-width="1" stroke-opacity="0.5"/>
    <text x="${px(highlight[0]).toFixed(1)}" y="${py(highlight[1]).toFixed(1)}" fill="var(--z-accent)" font-family="var(--font-data)" font-size="18" text-anchor="middle" dominant-baseline="middle">×</text>
    <text x="${(px(highlight[0]) + 8).toFixed(1)}" y="${(py(highlight[1]) - 8).toFixed(1)}" fill="var(--z-accent)" font-family="var(--font-data)" font-size="11" letter-spacing="0.08em">QUERY</text>
  ` : '';

  return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
    ${hulls}
    ${dots}
    ${labels}
    ${hl}
  </svg>
  <div class="series-legend">
    <span class="mono" style="color:var(--z-mute);">UMAP · <span style="color:var(--z-ink);">${allPts.length}</span> embeddings · <span style="color:var(--z-ink);">${clusters.length}</span> clusters</span>
  </div>`;
};

// ==============================================================
// Ribbon3D — axonometric stacked 1px time series
// ==============================================================
/**
 * Multiple time series rendered with per-series x/y offsets to
 * create an axonometric "3D" effect. Every line is still a 1px
 * polyline — no fills, no perspective tricks, no lighting.
 *
 *   series: [{ label, values }]
 *   h:      intrinsic svg height
 *   depth:  pixel offset per ribbon in the depth axis
 */
export const Ribbon3D = ({ series = [], h = 260, depth = 14 } = {}) => {
  if (!series.length) return '';
  const W = 1200;
  const [mn, mx] = minMax(series.flatMap((s) => s.values));
  const range = mx - mn || 1;
  const n = series.length;
  const innerW = W - (n - 1) * depth - 120;
  const innerH = h - (n - 1) * depth - 40;

  const bodies = series.map((s, i) => {
    const offX = 110 + (n - 1 - i) * depth;
    const offY = 20 + (n - 1 - i) * depth;
    const pts = s.values.map((v, j) => {
      const x = offX + (j / (s.values.length - 1)) * innerW;
      const y = offY + innerH - ((v - mn) / range) * innerH;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    }).join(' ');
    const op = Math.max(0.32, 1 - i * 0.14);
    const labelX = offX - 6;
    const labelY = offY + innerH * 0.5;
    const label = `<text x="${labelX.toFixed(1)}" y="${labelY.toFixed(1)}" fill="currentColor" opacity="${op.toFixed(2)}" font-family="var(--font-data)" font-size="11" text-anchor="end" dominant-baseline="middle">${esc(s.label)}</text>`;
    const endVal = `<text x="${(offX + innerW + 6).toFixed(1)}" y="${((offY + innerH - ((s.values[s.values.length-1] - mn) / range) * innerH)).toFixed(1)}" fill="currentColor" opacity="${op.toFixed(2)}" font-family="var(--font-data)" font-size="11" dominant-baseline="middle">${esc(fmt(s.values[s.values.length - 1]))}</text>`;
    return `<polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>${label}${endVal}`;
  }).join('');

  return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
    ${bodies}
  </svg>
  <div class="series-legend">
    <span class="mono" style="color:var(--z-mute);">axonometric · ${n} series · min <span style="color:var(--z-ink);">${fmt(mn)}</span> · max <span style="color:var(--z-ink);">${fmt(mx)}</span></span>
  </div>`;
};

// ==============================================================
// ChordArcs — 1px Bézier arcs from sources to targets
// ==============================================================
/**
 * Flow between two ordered sets. Perfect for RAG:
 *   sources = queries, targets = chunks/citations, flows = retrievals.
 *
 *   sources: [{ id, label }]
 *   targets: [{ id, label }]
 *   flows:   [{ from: sourceId, to: targetId, weight }]
 */
export const ChordArcs = ({ sources = [], targets = [], flows = [], h = 420, unit = '' } = {}) => {
  if (!sources.length || !targets.length || !flows.length) return '';
  const W = 1200;
  const pad = 20;
  const innerH = h - pad * 2;
  const srcY = (id) => {
    const i = sources.findIndex((s) => s.id === id);
    return pad + ((i + 0.5) / sources.length) * innerH;
  };
  const tgtY = (id) => {
    const i = targets.findIndex((t) => t.id === id);
    return pad + ((i + 0.5) / targets.length) * innerH;
  };
  const leftX = 220;
  const rightX = W - 220;
  const maxW = Math.max(...flows.map((f) => f.weight)) || 1;

  const arcs = flows.map((f) => {
    const y1 = srcY(f.from);
    const y2 = tgtY(f.to);
    if (y1 == null || y2 == null) return '';
    const cx1 = leftX + (rightX - leftX) * 0.38;
    const cx2 = leftX + (rightX - leftX) * 0.62;
    const op = Math.max(0.1, Math.min(0.9, f.weight / maxW * 0.85));
    return `<path d="M ${leftX},${y1.toFixed(1)} C ${cx1.toFixed(1)},${y1.toFixed(1)} ${cx2.toFixed(1)},${y2.toFixed(1)} ${rightX},${y2.toFixed(1)}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>`;
  }).join('');

  const srcLabels = sources.map((s) => `
    <text x="${(leftX - 12).toFixed(1)}" y="${srcY(s.id).toFixed(1)}" fill="currentColor" font-family="var(--font-data)" font-size="11" text-anchor="end" dominant-baseline="middle">${esc(s.label)}</text>
  `).join('');

  const tgtLabels = targets.map((t) => `
    <text x="${(rightX + 12).toFixed(1)}" y="${tgtY(t.id).toFixed(1)}" fill="currentColor" font-family="var(--font-data)" font-size="11" dominant-baseline="middle">${esc(t.label)}</text>
  `).join('');

  const srcHead = `<text x="${(leftX - 12).toFixed(1)}" y="${(pad - 8).toFixed(1)}" fill="currentColor" opacity="0.6" font-family="var(--font-prose)" font-size="11" font-weight="600" letter-spacing="2" text-transform="uppercase" text-anchor="end">SOURCE</text>`;
  const tgtHead = `<text x="${(rightX + 12).toFixed(1)}" y="${(pad - 8).toFixed(1)}" fill="currentColor" opacity="0.6" font-family="var(--font-prose)" font-size="11" font-weight="600" letter-spacing="2" text-transform="uppercase">TARGET</text>`;

  return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px; overflow:visible;">
    ${arcs}
    ${srcLabels}
    ${tgtLabels}
    ${srcHead}
    ${tgtHead}
  </svg>
  <div class="series-legend">
    <span class="mono" style="color:var(--z-mute);">${flows.length} flows · <span style="color:var(--z-ink);">${sources.length}</span>→<span style="color:var(--z-ink);">${targets.length}</span>${unit ? ' · ' + esc(unit) : ''}</span>
  </div>`;
};

// ==============================================================
// ParallelCoords — 1px parallel coordinate plot
// ==============================================================
/**
 * Multi-dimensional rows rendered as 1px polylines across N axes.
 * Standard ML tool. Every axis is a 1px vertical line. Stacked
 * rows form density patterns via accumulated opacity.
 *
 *   dims: [{ name, min?, max? }]
 *   rows: [[v0, v1, ..., vN]]
 */
export const ParallelCoords = ({ dims = [], rows = [], h = 260, highlight = null } = {}) => {
  if (!dims.length || !rows.length) return '';
  const W = 1200;
  const pad = 60;
  const top = 36, bot = 36;
  const colX = (i) => pad + (i / (dims.length - 1 || 1)) * (W - pad * 2);
  const extents = dims.map((d, i) => {
    if (d.min != null && d.max != null) return [d.min, d.max];
    const vals = rows.map((r) => r[i]).filter((v) => v != null);
    return minMax(vals);
  });

  const axes = dims.map((d, i) => {
    const x = colX(i).toFixed(1);
    return `<line x1="${x}" y1="${top}" x2="${x}" y2="${h - bot}" stroke="currentColor" stroke-width="1" stroke-opacity="0.2"/>`;
  }).join('');

  const polyline = (row, op) => {
    const pts = row.map((v, i) => {
      const [mn, mx] = extents[i];
      const frac = (v - mn) / (mx - mn || 1);
      const x = colX(i);
      const y = top + (1 - Math.max(0, Math.min(1, frac))) * (h - top - bot);
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    }).join(' ');
    return `<polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${op}"/>`;
  };
  const bodies = rows.map(() => polyline).map((fn, i) => fn(rows[i], '0.16')).join('');
  const hl = highlight ? polyline(highlight, '1').replace('stroke="currentColor"', 'stroke="var(--z-accent)"') : '';

  const labels = dims.map((d, i) => {
    const x = colX(i).toFixed(1);
    const [mn, mx] = extents[i];
    return `
      <text x="${x}" y="22" fill="currentColor" font-family="var(--font-prose)" font-size="11" font-weight="700" letter-spacing="1.5" text-anchor="middle">${esc(d.name.toUpperCase())}</text>
      <text x="${x}" y="${h - 16}" fill="currentColor" opacity="0.55" font-family="var(--font-data)" font-size="10" text-anchor="middle">${esc(fmt(mn))}</text>
      <text x="${x}" y="${h - 4}" fill="currentColor" opacity="0.55" font-family="var(--font-data)" font-size="10" text-anchor="middle">${esc(fmt(mx))}</text>
    `;
  }).join('');

  return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
    ${axes}
    ${bodies}
    ${hl}
    ${labels}
  </svg>
  <div class="series-legend">
    <span class="mono" style="color:var(--z-mute);"><span style="color:var(--z-ink);">${rows.length}</span> rows · <span style="color:var(--z-ink);">${dims.length}</span> dimensions${highlight ? ' · <span style="color:var(--z-accent);">1 highlighted</span>' : ''}</span>
  </div>`;
};

// ==============================================================
// AttentionMap — inline text with per-token opacity
// ==============================================================
/**
 * A paragraph where each word's opacity equals its attention weight.
 * The paragraph IS the chart. For RAG, this is how you show which
 * retrieved tokens actually mattered to the answer.
 *
 *   tokens: [{ text, weight }]
 *   maxWeight: optional — normalized to this rather than observed max
 */
export const AttentionMap = ({ tokens = [], maxWeight = null } = {}) => {
  if (!tokens.length) return '';
  const mx = maxWeight ?? Math.max(...tokens.map((t) => t.weight));
  const spans = tokens.map((t) => {
    const op = Math.max(0.08, (t.weight / (mx || 1)));
    const hot = t.weight / mx > 0.82;
    const style = hot
      ? `color:var(--z-accent); opacity:${op.toFixed(2)};`
      : `opacity:${op.toFixed(2)};`;
    return `<span style="${style}">${esc(t.text)}</span>`;
  }).join(' ');
  const top = [...tokens].sort((a, b) => b.weight - a.weight).slice(0, 3);
  return `
  <div class="attention" style="font-family:var(--font-data); font-size:var(--fs-16); line-height:1.9; max-width:92ch;">${spans}</div>
  <div class="series-legend" style="margin-top:var(--sp-3);">
    <span class="mono" style="color:var(--z-mute);">peak attention on <span class="accent">${top.map((t) => esc(t.text)).join(' · ')}</span></span>
  </div>`;
};

// ==============================================================
// FlowBand — a single stacked horizontal allocation strip
// ==============================================================
/**
 * Budget allocation, token allocation, context-window breakdown.
 * One row, N segments, each labeled with absolute value directly
 * under the segment. Replaces "funnel" charts.
 */
export const FlowBand = ({ segments = [], width = 1200, unit = '' } = {}) => {
  if (!segments.length) return '';
  const total = segments.reduce((a, s) => a + s.value, 0) || 1;
  const H = 16;
  let x = 0;
  // SVG layer is just the horizontal band + tick divisions.
  const parts = segments.map((s, i) => {
    const w = (s.value / total) * width;
    const op = Math.max(0.28, 1 - i * 0.16);
    const line = w > 1
      ? `<line x1="${x.toFixed(1)}" y1="8" x2="${(x + w).toFixed(1)}" y2="8" stroke="currentColor" stroke-width="2" stroke-opacity="${op.toFixed(2)}"/>`
      : '';
    const tick = `<line x1="${x.toFixed(1)}" y1="2" x2="${x.toFixed(1)}" y2="14" stroke="currentColor" stroke-width="1" stroke-opacity="0.4"/>`;
    x += w;
    return tick + line;
  }).join('');
  const endTick = `<line x1="${width.toFixed(1)}" y1="2" x2="${width.toFixed(1)}" y2="14" stroke="currentColor" stroke-width="1" stroke-opacity="0.4"/>`;
  // HTML label layer — flex proportional to segment values. Tiny
  // segments collapse with ellipsis instead of overflowing into
  // neighboring labels. min-width:0 is required on every flex item
  // or the default min-width:auto prevents shrink below content.
  const labels = segments.map((s, i) => {
    const pct = ((s.value / total) * 100).toFixed(1);
    const op = Math.max(0.28, 1 - i * 0.16);
    return `
      <div class="fb-seg" style="flex:${s.value} 0 0; min-width:0; opacity:${op.toFixed(2)};">
        <div class="fb-label">${esc(s.label.toUpperCase())}</div>
        <div class="fb-value">${esc(fmt(s.value))}${unit ? ' ' + esc(unit) : ''} · ${pct}%</div>
      </div>`;
  }).join('');
  return `
  <div class="flowband-wrap">
    <svg class="chart flowband-svg" viewBox="0 0 ${width} ${H}" preserveAspectRatio="none" style="width:100%; height:${H}px; overflow:visible; display:block;">
      ${parts}${endTick}
    </svg>
    <div class="flowband-labels" style="display:flex; width:100%; margin-top:8px;">${labels}</div>
  </div>`;
};
