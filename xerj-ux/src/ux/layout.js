// ============================================================
// XERJ.ai — Layout primitives
// Panel, TopN. Nothing more. See UX_BOOK.md §3.
// ============================================================

import { esc, fmt } from './text.js';

/**
 * Panel — a slot in the 12-column canvas grid.
 * Renders NO box, NO border, NO background. The only chrome a panel
 * has is: an eyebrow line above the content, and its column span.
 *
 *   eyebrow:  small-caps label (the panel's identity)
 *   title:    optional section heading below the eyebrow
 *   cols:     1..12 column span
 *   children: string or string[]
 */
export const Panel = ({ eyebrow, title, cols = 6, children = '' } = {}) => {
  const body = Array.isArray(children) ? children.join('') : children;
  return `
<section class="panel" style="grid-column: span ${cols};">
  ${eyebrow ? `<div class="key">${esc(eyebrow)}</div>` : ''}
  ${title ? `<div class="h-section">${esc(title)}</div>` : ''}
  ${body}
</section>`;
};

/**
 * TopN — the ranked list. Replaces pies, donuts, treemaps, and most
 * bar charts. Every observability question starts with "top what?".
 *
 *   items:       [{ label, value }]
 *   total:       optional total for percentage column
 *   n:           limit (default 10)
 *   valueFmt:    function to render the value (default compact number)
 *   barWidth:    svg viewBox width for the 1px fill line (default 200)
 *   scale:       'max' (relative to row max) | 'total' (relative to total)
 *   filterField: if set, each row becomes click-to-filter on this field.
 *                Clicking a row emits a `[data-filter-add]` event the
 *                app router catches and turns into a global filter pill.
 */
export const TopN = ({
  items,
  total,
  n = 10,
  valueFmt = (v) => fmt(v),
  barWidth = 200,
  scale = 'max',
  filterField = '',
} = {}) => {
  if (!items || !items.length) return '<div class="faint mono">No data.</div>';
  const slice = items.slice(0, n);
  const max = Math.max(...slice.map((i) => i.value));
  const denom = scale === 'total' && total ? total : max;
  const out = slice.map((i) => {
    const frac = denom > 0 ? Math.max(0, Math.min(1, i.value / denom)) : 0;
    const pct = total ? ((100 * i.value) / total).toFixed(1) + '%' : '';
    const x = (frac * barWidth).toFixed(1);
    const clickable = filterField
      ? ` data-filter-add="${esc(filterField)}:${esc(i.label)}" role="button" title="Filter ${esc(filterField)} = ${esc(i.label)}"`
      : '';
    return `
    <div class="row${filterField ? ' clickable' : ''}"${clickable}>
      <div class="row__label">${esc(i.label)}</div>
      <div class="row__val">${esc(valueFmt(i.value))}</div>
      <div class="row__bar">
        <svg class="chart" height="6" viewBox="0 0 ${barWidth} 6" preserveAspectRatio="none">
          <line x1="0" y1="5" x2="${x}" y2="5" stroke="currentColor" stroke-width="1"/>
        </svg>
      </div>
      <div class="row__pct">${esc(pct)}</div>
    </div>`;
  }).join('');
  return out;
};

/**
 * Events — a text list of recent events. Three-column grid: time, severity,
 * message. No cells, no borders. Severity `err` lights the accent.
 *
 *   items: [{ at: ISO or HH:mm:ss, sev: 'info'|'warn'|'err', msg: string }]
 */
export const Events = ({ items } = {}) => {
  if (!items || !items.length) return '<div class="faint mono">No events.</div>';
  const rows = items.map((e) => `
    <div class="ev">
      <span class="when">${esc(e.at)}</span>
      <span class="sev ${e.sev === 'err' ? 'err' : ''}">${esc((e.sev || '').toUpperCase())}</span>
      <span class="msg">${esc(e.msg)}</span>
    </div>
  `).join('');
  return `<div class="events">${rows}</div>`;
};
