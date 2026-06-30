// ============================================================
// Xerj Console backend — in-memory mock
//
// Re-exports the existing mock so the dashboards keep rendering
// when no real backend is reachable. The "MOCK DATA" status pill
// in the nav surfaces this state to the user.
// ============================================================

import { mock as mockData } from '../mock.js';

export const meta = {
  id: 'mock',
  label: 'Mock data',
  defaultBaseUrl: '',
  supports: { search: true, aggs: true, knn: true, semantic: true, hybrid: true, fsck: false },
};

export async function probe(_baseUrl, _signal) {
  return true; // mock is always "live"
}

export async function search(_baseUrl, dashId, ctx, _signal) {
  return mockData(dashId, ctx.range || '24H', {
    cluster: ctx.cluster || '',
    filters: ctx.filters || {},
    customRange: ctx.customRange || null,
  });
}
