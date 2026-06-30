// ============================================================
// XERJ.ai — Text primitives
// Every component is a pure function returning an HTML string.
// No state, no events, no DOM access. See UX_BOOK.md §3.
// ============================================================

/** Escape user text for safe interpolation into template strings. */
export const esc = (s) =>
  String(s ?? '').replace(/[&<>"']/g, (c) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[c]));

/** Compact number formatter (4.82M, 12.3K, 0.83). */
const nf = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 2 });
export const fmt = (v, { decimals } = {}) => {
  if (v == null || Number.isNaN(v)) return '—';
  if (decimals != null) return Number(v).toFixed(decimals);
  if (Math.abs(v) < 1) return Number(v).toFixed(2);
  if (Math.abs(v) < 100) return Number(v).toFixed(1);
  return nf.format(v);
};

/** Pad an integer string to a fixed width with thin spaces (looks tabular). */
export const num = (v) => new Intl.NumberFormat('en').format(v);

// ----- Eyebrow / headings ------------------------------------

/** Small-caps eyebrow label. The ONLY way to title a panel. */
export const Key = (text) => `<div class="key">${esc(text)}</div>`;

/** `role` ∈ 'scene' | 'section'. Scene is once per view. */
export const H = (role, text) => {
  const cls = role === 'scene' ? 'h-scene' : 'h-section';
  const tag = role === 'scene' ? 'h1' : 'h2';
  return `<${tag} class="${cls}">${esc(text)}</${tag}>`;
};

// ----- Delta indicator ---------------------------------------

/**
 * `▲ 12.3%` / `▼ 0.12%` — standalone delta.
 * `good: 'up' | 'down' | null` — which direction lights the accent.
 * If `good` is null, delta is always muted (neutral).
 */
export const Change = (pct, { good = 'up', unit = '%', fractionDigits = 1 } = {}) => {
  if (pct == null || Number.isNaN(pct)) return '';
  const up = pct >= 0;
  const arrow = up ? '▲' : '▼';
  const abs = Math.abs(pct).toFixed(fractionDigits);
  let klass = 'meh';
  if (good === 'up') klass = up ? 'good' : 'bad';
  else if (good === 'down') klass = up ? 'bad' : 'good';
  return `<span class="delta ${klass}">${arrow} ${abs}${esc(unit)}</span>`;
};

// ----- Headline metric ---------------------------------------

/**
 * Num — the point of every panel.
 *   value:     string or number (preformatted is fine)
 *   unit:      short unit like 'events', 'e/s', '%', 'ms'
 *   spark:     optional Spark(...) string rendered to the right
 *   delta:     signed percent, rendered with Change
 *   deltaGood: 'up' | 'down' | null — which direction is "good"
 *   hint:      small mono hint rendered below ('at 14:23', etc.)
 *   emphasis:  true → 96/56px display + accent; false → 32px ink
 */
export const Num = ({
  value,
  unit = '',
  spark = '',
  delta = null,
  deltaGood = 'up',
  hint = '',
  emphasis = true,
} = {}) => {
  const valCls = emphasis ? 'num-big accent' : 'num-md';
  const unitSpan = unit ? `<span class="num-unit">${esc(unit)}</span>` : '';
  const deltaPart = delta != null ? Change(delta, { good: deltaGood }) : '';
  const hintPart = hint
    ? `<span class="hint">${esc(hint)}</span>`
    : '';
  const foot = (deltaPart || hintPart)
    ? `<div class="row-flex" style="margin-top:10px;">${deltaPart}${hintPart}</div>`
    : '';
  return `
  <div class="stack">
    <div class="row-flex">
      <div class="${valCls}">${esc(value)}${unitSpan}</div>
      ${spark}
    </div>
    ${foot}
  </div>`;
};

// ----- Horizontal-bar row (one line of a TopN) ---------------

/**
 * A single ranked row. Use `TopN` to render lists; this is exported so you
 * can hand-build a row in custom panels.
 */
export const HBar = ({ label, value, pct, frac, barWidth = 200 }) => {
  const f = Math.max(0, Math.min(1, frac ?? 0));
  const x = (f * barWidth).toFixed(1);
  return `
  <div class="row">
    <div class="row__label">${esc(label)}</div>
    <div class="row__val">${esc(value)}</div>
    <div class="row__bar">
      <svg class="chart" height="6" viewBox="0 0 ${barWidth} 6" preserveAspectRatio="none">
        <line x1="0" y1="5" x2="${x}" y2="5" stroke="currentColor" stroke-width="1"/>
      </svg>
    </div>
    <div class="row__pct">${pct != null ? esc(pct) : ''}</div>
  </div>`;
};

// ----- Inline key/value used in scene kicker ----------------

export const Kv = (k, v) =>
  `<span class="mono"><span class="faint">${esc(k)}</span> ${esc(v)}</span>`;
