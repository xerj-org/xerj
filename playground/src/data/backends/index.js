// ============================================================
// Xerj Console — backend abstraction layer
//
// Xerj Console is a typography-first observability UI that today targets
// the Xerj engine but is designed to swap in other backends later
// (Kibana / OpenSearch / vanilla Elasticsearch, plus eventually a
// pure-vector backend like Pinecone or Qdrant for AI demos).
//
// Each backend is a module under `./backends/<name>.js` exporting:
//
//   export const meta = { id, label, defaultBaseUrl, supports: { … } };
//
//   export async function probe(baseUrl, signal) → boolean
//        // Cheap "is there a live backend at this URL?" check. The
//        // playground status pill goes amber until probe() returns true.
//
//   export async function search(baseUrl, dashId, ctx, signal) → data
//        // The contract is identical to the dashboard render fn input —
//        // each backend translates dashId + range + filters into the
//        // backend's native query and shapes the result back to the
//        // dashboard's expected `data` object.
//
// The active backend is selected by name; an empty/unreachable backend
// falls back to the in-memory mock so the UI is always responsive.
// ============================================================

import * as xerj from './xerj.js';
import * as mock  from './mock.js';

/** All registered backends. Add new modules here to expose them in the picker. */
export const BACKENDS = { xerj, mock };

/** Default backend selection, persisted in localStorage. */
const STORAGE_KEY = 'xerj-console.backend';
const DEFAULT_KEY = 'xerj';

export function activeBackendId() {
  if (typeof localStorage === 'undefined') return DEFAULT_KEY;
  return localStorage.getItem(STORAGE_KEY) || DEFAULT_KEY;
}

export function setActiveBackendId(id) {
  if (typeof localStorage === 'undefined') return;
  if (BACKENDS[id]) localStorage.setItem(STORAGE_KEY, id);
}

export function activeBackend() {
  return BACKENDS[activeBackendId()] || BACKENDS[DEFAULT_KEY];
}

/** Per-backend baseURL setting (default 'http://localhost:9200' for xerj). */
const URL_KEY = (id) => `xerj-console.backend.${id}.baseUrl`;

export function backendBaseUrl(id) {
  if (typeof localStorage === 'undefined') return BACKENDS[id]?.meta?.defaultBaseUrl || '';
  return localStorage.getItem(URL_KEY(id)) || BACKENDS[id]?.meta?.defaultBaseUrl || '';
}

export function setBackendBaseUrl(id, url) {
  if (typeof localStorage === 'undefined') return;
  localStorage.setItem(URL_KEY(id), url);
}
