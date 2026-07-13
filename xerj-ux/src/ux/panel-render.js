// ============================================================
// XERJ.ai — Declarative panel renderer
//
// Maps (viz.type, per-panel query result) onto the EXISTING chart
// primitives — the same Series / VBar / Dist / TopN / Table / Num /
// Gauge / Heatmap / AI charts the code dashboards use — but driven by a
// per-panel result from data/panel-query.js instead of a whole-dashboard
// `data` object. This is what gives net-from-scratch user panels real,
// live data under the same visual language.
//
// It NEVER throws: every branch degrades to a typography-first note so a
// half-configured panel reads as "configure me", not "broken".
// ============================================================

import { esc } from './text.js';
import { Num, fmt } from './text.js';
import { Spark, Series, Dist, Heatmap } from './charts.js';
import { VBar, Gauge, Treemap } from './charts-ext.js';
import { TopN, Events } from './layout.js';
import { Table } from './tables.js';
import { chartTypes } from './chart-types.js';

// viz types we can feed from a generic query result.
const DATA_DRIVEN = new Set([
  'metric', 'gauge', 'spark', 'line', 'bar', 'histogram',
  'dist', 'topn', 'treemap', 'table', 'events',
]);

const note = (t) => `<div class="panel-empty mono faint">${esc(t)}</div>`;

/** buckets [{key,count}] or timeseries [{t,count}] → [{label,value}]. */
function toItems(d) {
  if (d.buckets && d.buckets.length) return d.buckets.map((b) => ({ label: b.key, value: b.count }));
  if (d.series && d.series.length) return d.series.map((s) => ({ label: String(s.t), value: s.count }));
  return [];
}
function toValues(d) {
  if (d.series && d.series.length) return d.series.map((s) => s.count);
  if (d.buckets && d.buckets.length) return d.buckets.map((b) => b.count);
  return [];
}
function scalar(d) {
  if (d.value != null) return d.value;
  if (d.total != null) return d.total;
  return null;
}

/**
 * Render the INNER html of a declarative panel (app.js wraps it in the
 * <section class="panel"> shell + eyebrow). `result` is the object
 * data/panel-query.js#panelResult returns.
 */
export function renderDeclarativePanel(panel, result) {
  const type = (panel.viz && panel.viz.type) || 'metric';

  if (!panel.query || !panel.query.index || !panel.query.kind) {
    return note('UNCONFIGURED · OPEN EDIT → PICK AN INDEX + QUERY');
  }
  if (!result || result.status === 'loading') {
    return `<div class="panel-empty mono faint">LOADING · ${esc((panel.query.index || '').toUpperCase())}…</div>`;
  }
  if (result.status === 'error') {
    return note('QUERY ERROR · ' + String(result.error || '').slice(0, 90));
  }
  const d = result.data || {};

  try {
    switch (type) {
      case 'metric': {
        const v = scalar(d);
        const spark = d.series && d.series.length ? Spark(toValues(d), { w: 180, h: 30 }) : '';
        return Num({ value: v == null ? '—' : fmt(v), unit: panel.query.metric || 'count', spark, emphasis: true });
      }
      case 'spark': {
        const vals = toValues(d);
        return Num({
          value: vals.length ? fmt(vals[vals.length - 1]) : (scalar(d) == null ? '—' : fmt(scalar(d))),
          unit: panel.query.metric || 'count',
          spark: vals.length ? Spark(vals, { w: 200, h: 36 }) : '',
          emphasis: false,
        });
      }
      case 'gauge': {
        const v = scalar(d) || 0;
        const vals = toValues(d);
        const max = vals.length ? Math.max(v, ...vals) : Math.max(1, v);
        return Gauge({ value: v, min: 0, max: max || 1, unit: panel.query.metric || '', label: esc(panel.query.field || '') });
      }
      case 'line': {
        const vals = toValues(d);
        if (vals.length < 2) return note('NO SERIES · USE A TIMESERIES QUERY');
        return Series(vals, { h: 140, labels: ['START', 'NOW'], unit: panel.query.metric || '' });
      }
      case 'bar':
      case 'histogram': {
        const items = toItems(d);
        if (!items.length) return note('NO DATA');
        return VBar({ items, h: 150, unit: panel.query.metric || '' });
      }
      case 'dist': {
        const items = toItems(d);
        if (!items.length) return note('NO DATA');
        return Dist({ segments: items.map((i) => ({ label: i.label, value: i.value })), width: 1200 });
      }
      case 'topn': {
        const items = toItems(d);
        if (!items.length) return note('NO DATA');
        const total = items.reduce((a, i) => a + i.value, 0);
        return TopN({
          items, total, n: panel.query.size || 8,
          filterField: panel.query.field || '',
        });
      }
      case 'treemap': {
        const items = toItems(d);
        if (!items.length) return note('NO DATA');
        return Treemap({ items });
      }
      case 'table': {
        if (d.hits && d.hits.length) {
          return Table({
            columns: ['INDEX', 'ID', 'SCORE'],
            rows: d.hits.slice(0, 20).map((h) => [h._index, h._id, h._score == null ? '—' : fmt(h._score)]),
            align: ['left', 'left', 'right'],
          });
        }
        const items = toItems(d);
        if (!items.length) return note('NO DATA');
        const total = items.reduce((a, i) => a + i.value, 0) || 1;
        return Table({
          columns: [(panel.query.field || 'KEY').toUpperCase(), 'COUNT', 'PCT'],
          rows: items.map((i) => [i.label, fmt(i.value), ((100 * i.value) / total).toFixed(1) + '%']),
          align: ['left', 'right', 'right'],
        });
      }
      case 'events': {
        const hits = d.hits || [];
        if (!hits.length) return note('NO EVENTS · USE A SEARCH QUERY');
        return Events({
          items: hits.slice(0, 12).map((h) => ({
            at: h._source?.['@timestamp'] || h._id,
            sev: (h._source?.level || 'info'),
            msg: h._source?.message || JSON.stringify(h._source || {}).slice(0, 120),
          })),
        });
      }
      default:
        // Exotic / AI viz types (embedspace, ribbon3d, chord, pcoords,
        // attention, flowband, heatmap, multiples, scatter, stacked,
        // markdown) have no generic query mapping — preview them with
        // their branded demo render so the catalog stays whole.
        if (chartTypes[type] && typeof chartTypes[type].render === 'function') {
          return chartTypes[type].render(d);
        }
        return note('UNSUPPORTED VIZ · ' + esc(type));
    }
  } catch (err) {
    return note('RENDER ERROR · ' + String(err.message || err).slice(0, 80));
  }
}

/** Is this viz type one we can drive from a query (vs. demo-only)? */
export function isDataDriven(type) {
  return DATA_DRIVEN.has(type);
}
