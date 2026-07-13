// ============================================================
// XERJ.ai — Dashboards CRUD client
//
// Thin async wrapper over the durable `/_xerj-console/api/v1/dashboards`
// surface (implemented in engine/crates/xerj-console-api/src/dashboards.rs).
// This is the ONLY thing that turns a user-created dashboard into
// backend-durable state — before this module existed the SPA wrote
// only to localStorage and the CRUD backend had no caller.
//
// Every method returns the parsed Dashboard doc (or the list envelope).
// Optimistic concurrency: PUT/PATCH send `If-Match: W/"<version>"` built
// from the doc's own `version` field (the primary concurrency source);
// the ETag response header is captured too as a convenience.
//
// Errors bubble up as Error objects carrying `.status` (409/403/404/…)
// so the store can branch on a stale-etag 409 and re-GET.
// ============================================================

import { apiRaw } from '../xerj-console-auth.js';

const BASE = '/dashboards';
const enc = encodeURIComponent;

/** Build the weak-etag If-Match header from a version number. */
function ifMatch(version) {
  if (version == null) return undefined;
  return { 'if-match': `W/"${version}"` };
}

/** GET /dashboards → { dashboards: [...], total }. */
export async function list() {
  const { data } = await apiRaw('GET', BASE);
  return data || { dashboards: [], total: 0 };
}

/** GET /dashboards/{id} → { doc, etag }. */
export async function get(id) {
  const { data, etag } = await apiRaw('GET', `${BASE}/${enc(id)}`);
  return { doc: data, etag };
}

/** POST /dashboards → { doc, etag }. Body per the CREATE contract. */
export async function create(body) {
  const { data, etag } = await apiRaw('POST', BASE, body);
  return { doc: data, etag };
}

/** PUT /dashboards/{id} (full replace) → { doc, etag }. */
export async function replace(id, body, version) {
  const { data, etag } = await apiRaw('PUT', `${BASE}/${enc(id)}`, body, ifMatch(version));
  return { doc: data, etag };
}

/** PATCH /dashboards/{id} (partial) → { doc, etag }. Omitted fields unchanged. */
export async function patch(id, partial, version) {
  const { data, etag } = await apiRaw('PATCH', `${BASE}/${enc(id)}`, partial, ifMatch(version));
  return { doc: data, etag };
}

/** DELETE /dashboards/{id} (soft delete). 204, no body. */
export async function remove(id, version) {
  await apiRaw('DELETE', `${BASE}/${enc(id)}`, undefined, ifMatch(version));
}
