// ============================================================
// XERJ Console — schema-derived search config
//
// The console must query the fields that ACTUALLY exist on the engine, not a
// fixed set of demo fields. The engine's own field list tells us the type of
// every field; from that we derive the roles the search UI needs:
//
//   • textField      — what `match` / `phrase` / `prefix` run against
//   • semanticField  — the `semantic_text` field `semantic` embeds into
//   • keywordFields  — candidates for the facet sidebar (terms aggs)
//   • dateField      — the time field, if any
//   • isEmail        — carries the EML extractor's fields
//
// An engine holding an autoindexed inbox (`body` = semantic_text, `email_*` =
// keyword) gets emails searched on `body` with zero manual field config; a
// log engine (`message`, `level`, `service`) gets those instead. No aliases,
// no hardcoding.
//
// Source: the session-authenticated console API (data/console-index-api.js),
// NOT `GET /_mapping` — a console session is not an engine credential, so on
// an auth-enabled engine the data plane answers 401 and the picker would be
// empty. Cached briefly. Fail-SOFT: an unreadable field list yields
// conservative defaults so a search still runs (and reports its own error).
// ============================================================

import { deriveRoles } from './schema-roles.js';
import { listUserIndices, fieldTypes, INTERNAL_INDICES } from './console-index-api.js';
export { deriveRoles };

const TTL_MS = 30_000;
let _names = null;               // { at, names }
const _fields = new Map();       // index -> { at, types }

/** Drop the caches (tests; and after the operator indexes something new). */
export function resetSchemaCache() { _names = null; _fields.clear(); }

/** Real user index names on the engine (no system indices, no autoindex
 *  plumbing). THROWS when the list cannot be read — "could not ask" and
 *  "the engine holds nothing" are different answers and must read
 *  differently on the page. */
export async function indexNamesStrict(signal) {
  if (_names && Date.now() - _names.at < TTL_MS) return _names.names;
  const names = (await listUserIndices(signal)).map((it) => it.name).filter((n) => !INTERNAL_INDICES.has(n));
  _names = { at: Date.now(), names };
  return names;
}

/** Same, for callers that only decorate the UI (the nav probe): `[]` when the
 *  list cannot be read. A failure is not cached — the next render asks again. */
export async function indexNames(_baseUrl, signal) {
  try { return await indexNamesStrict(signal); } catch (e) {
    if (e && e.name === 'AbortError') throw e;
    return [];
  }
}

/** Field roles for one concrete index. */
export async function schemaForSearch(_baseUrl, index, signal) {
  if (!index || index === '*' || index === '_all') return deriveRoles({});
  const hit = _fields.get(index);
  if (hit && Date.now() - hit.at < TTL_MS) return deriveRoles(hit.types);
  let types = {};
  try {
    types = await fieldTypes(index, signal);
    _fields.set(index, { at: Date.now(), types });
  } catch (e) {
    if (e && e.name === 'AbortError') throw e;
  }
  return deriveRoles(types);
}
