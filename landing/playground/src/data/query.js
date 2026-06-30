// ============================================================
// XERJ.ai — Data source swap point
//
// Today: everything is served from the in-memory mock.
// Tomorrow: replace the body of `query` with real fetch() calls
// to xerj-api. Dashboards never touch HTTP directly — this is
// the ONLY file that should change when the backend is ready.
// ============================================================

import { mock } from './mock.js';

/**
 * Fetch shaped data for a dashboard.
 *
 *   ctx.dashId:  dashboard id from the registry
 *   ctx.range:   '1H' | '24H' | '7D' | '30D' | '90D'
 *   ctx.cluster: cluster id (defaults to active cluster)
 *   ctx.filters: { [field]: value | [values] }   — global filter pills
 *   ctx.signal:  AbortSignal — real backend will honour it
 *
 * Returns a plain object shaped for the dashboard's render() fn,
 * wrapped as `{ data, meta }` where meta carries fetch timings,
 * the echoed query spec, and `fetchedAt` for the nav status line.
 */
export async function query(ctx = {}) {
  const {
    dashId,
    range = '24H',
    customRange = null,  // { from: ISO, to: ISO } when range === 'CUSTOM'
    cluster = '',
    filters = {},
  } = typeof ctx === 'string' ? { dashId: ctx } : ctx;

  const t0 = (typeof performance !== 'undefined' ? performance.now() : Date.now());
  // When the engine lands, this becomes:
  //   const res = await fetch(`/xerj/dashboards/${dashId}`, {
  //     method: 'POST',
  //     headers: { 'content-type': 'application/json' },
  //     body: JSON.stringify({ range, customRange, cluster, filters }),
  //     signal: ctx.signal,
  //   });
  //   if (!res.ok) throw new Error(`HTTP ${res.status}`);
  //   const data = await res.json();
  const data = mock(dashId, range, { cluster, filters, customRange });
  const t1 = (typeof performance !== 'undefined' ? performance.now() : Date.now());

  return {
    data,
    meta: {
      dashId,
      range,
      customRange,
      cluster,
      filters,
      fetchedAt: new Date().toISOString(),
      durationMs: Math.round((t1 - t0) * 10) / 10,
      source: DATA_SOURCE_KIND,
    },
  };
}

/** Which data source are we running on? Flipped when the API is wired. */
export const DATA_SOURCE_KIND = 'mock';

/** Data source status indicator shown in the nav. */
export const dataSourceStatus = 'MOCK DATA · BACKEND PENDING';
