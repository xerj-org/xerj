// ============================================================
// Xerj Console — query gateway
//
// All dashboard data flows through this one entry point. It picks
// the active backend (default: xerj), tries a live call, and
// falls back to the in-memory mock when the live call fails or
// returns null (no adapter for this dashboard yet).
//
// This file is the only place in the playground that touches HTTP.
// Dashboards stay backend-agnostic — they receive a shaped `data`
// object and never see fetch().
// ============================================================

import { mock as mockData } from './mock.js';
import {
  activeBackend,
  activeBackendId,
  backendBaseUrl,
  BACKENDS,
} from './backends/index.js';

/** Last-known status for the nav pill. Updated on every query. */
let _lastSourceKind = 'pending';
let _lastSourceLabel = 'STARTING UP';

/** Function form (preferred). Returns the live status string. */
export function dataSourceStatus() {
  return _lastSourceLabel;
}

export function dataSourceKind() {
  return _lastSourceKind;
}

/**
 * Fetch shaped data for a dashboard.
 *
 *   ctx.dashId:  dashboard id from the registry
 *   ctx.range:   '1H' | '24H' | '7D' | '30D' | '90D' | 'CUSTOM'
 *   ctx.cluster: cluster id (defaults to active cluster)
 *   ctx.filters: { [field]: value | [values] }
 *   ctx.search:  per-dashboard freeform state (q, type, index, …)
 *   ctx.signal:  AbortSignal — backends that honour it cancel in flight
 *
 * Returns `{ data, meta }` where meta carries timing, the active
 * backend, and `fetchedAt` for the nav status line.
 */
export async function query(ctx = {}) {
  const {
    dashId,
    range = '24H',
    customRange = null,
    cluster = '',
    filters = {},
    search = {},
    signal,
  } = typeof ctx === 'string' ? { dashId: ctx } : ctx;

  const t0 = perfNow();
  const backend = activeBackend();
  const backendId = activeBackendId();
  const baseUrl = backendBaseUrl(backendId);

  // Try the live backend. A return of `null` means the backend
  // recognises the dashId but has no live adapter yet — we fall
  // through to mock without flipping the status pill to red.
  let data = null;
  let liveError = null;
  if (backend && typeof backend.search === 'function') {
    try {
      data = await backend.search(baseUrl, dashId, { range, customRange, cluster, filters, search }, signal);
    } catch (e) {
      liveError = String(e);
    }
  }

  if (data == null) {
    // Fallback to mock.
    data = mockData(dashId, range, { cluster, filters, customRange });
    _lastSourceKind = backendId === 'mock' ? 'mock' : 'fallback';
    _lastSourceLabel = backendId === 'mock'
      ? 'MOCK DATA'
      : `${BACKENDS[backendId]?.meta?.label || backendId}: MOCK FALLBACK`;
  } else if (data.error) {
    // The backend ran but returned an error envelope. Surface it to
    // the dashboard but keep the live source label so the user can
    // see "the backend is up but this query failed."
    _lastSourceKind = 'live-error';
    _lastSourceLabel = `${BACKENDS[backendId]?.meta?.label || backendId}: ${String(data.error).slice(0, 80)}`;
  } else {
    _lastSourceKind = 'live';
    _lastSourceLabel = `LIVE · ${BACKENDS[backendId]?.meta?.label || backendId} · ${baseUrl || ''}`;
  }

  const t1 = perfNow();
  return {
    data,
    meta: {
      dashId,
      range,
      customRange,
      cluster,
      filters,
      search,
      fetchedAt: new Date().toISOString(),
      durationMs: Math.round((t1 - t0) * 10) / 10,
      backend: backendId,
      backendLabel: BACKENDS[backendId]?.meta?.label || backendId,
      baseUrl,
      sourceKind: _lastSourceKind,
      liveError,
    },
  };
}

/** Probe the active backend. Used by the nav to flip the status
 *  pill from amber → green when the engine becomes reachable. */
export async function probeActiveBackend(signal) {
  const backend = activeBackend();
  const backendId = activeBackendId();
  const baseUrl = backendBaseUrl(backendId);
  if (!backend || typeof backend.probe !== 'function') return false;
  try {
    return await backend.probe(baseUrl, signal);
  } catch (_e) {
    return false;
  }
}

function perfNow() {
  return typeof performance !== 'undefined' ? performance.now() : Date.now();
}

/** Initial value (overwritten at runtime). Kept as a default export
 *  for any old `import { DATA_SOURCE_KIND }` call sites. */
export const DATA_SOURCE_KIND = 'pending';
