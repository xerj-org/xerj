// ============================================================
// XERJ Console — schema-derived search config
//
// The console must query the fields that ACTUALLY exist on the engine, not a
// fixed set of demo fields. One `_mapping` call tells us every user index and
// the type of every field; from that we derive the roles the search UI needs:
//
//   • textField      — what `match` / `phrase` / `prefix` run against
//   • semanticField  — the `semantic_text` field `semantic` embeds into
//   • keywordFields  — candidates for the facet sidebar (terms aggs)
//   • dateField      — the time field, if any
//   • isEmail        — carries the EML extractor's fields
//
// This is the Kibana move: the UI aligns to indexed data. An engine holding an
// autoindexed inbox (`body` = semantic_text, `email_*` = keyword) gets emails
// searched by meaning with zero manual field config; a log engine (`message`,
// `level`, `service`) gets those instead. No aliases, no hardcoding.
//
// One cached `_mapping` fetch, classified client-side. Fail-SOFT: an empty
// mapping yields conservative defaults so search still runs.
// ============================================================

import { deriveRoles } from './schema-roles.js';
export { deriveRoles };

const TTL_MS = 30_000;
const cache = new Map(); // baseUrl -> { at, mapping }

// The bundled console serves the engine's ES-compat API on its OWN origin, so
// direct `_mapping` calls go same-origin (this is what data-sources.js's
// liveBaseUrl() uses). The logical `backendBaseUrl` ('http://localhost:9200')
// is a proxy identifier, not a reachable host — don't fetch it directly.
function resolveBase(baseUrl) {
  if (typeof window !== 'undefined' && window.location && window.location.origin) {
    return window.location.origin;
  }
  return (baseUrl || '').replace(/\/+$/, '');
}

// A shared-link guest (data/guest.js) may read ONE index; a whole-engine
// `_mapping` would be refused, so the shell scopes this module to that index
// and the fetch becomes `/<index>/_mapping`. Null = unscoped (the operator).
let _scopeIndex = null;
export function setMappingScope(index) {
  _scopeIndex = index || null;
  cache.clear();
}

async function fetchMapping(baseUrl, signal) {
  const base = resolveBase(baseUrl);
  if (!base) return {};
  const hit = cache.get(base);
  if (hit && Date.now() - hit.at < TTL_MS) return hit.mapping;
  let mapping = {};
  try {
    const path = _scopeIndex ? `/${encodeURIComponent(_scopeIndex)}/_mapping` : '/_mapping';
    const r = await fetch(`${base}${path}`, { signal, headers: { accept: 'application/json' } });
    if (r.ok) mapping = await r.json();
  } catch { mapping = {}; }
  cache.set(base, { at: Date.now(), mapping });
  return mapping;
}

// Non-dot indices that are still engine plumbing, not user search targets.
const INTERNAL_INDICES = new Set(['autoindex-catalog']);

/** User (non-system) index names present in a mapping. */
function userIndices(mapping) {
  return Object.keys(mapping || {}).filter((n) => !n.startsWith('.') && !INTERNAL_INDICES.has(n));
}

// Union the properties of the named index (or all user indices, for _all)
// into one { field: type } map. If a concrete index name isn't in the mapping
// (e.g. it's an alias), fall back to all user indices so roles still resolve.
function fieldsOf(mapping, index) {
  const all = userIndices(mapping);
  const wildcard = !index || index === '*' || index === '_all';
  const names = wildcard ? all : (mapping?.[index] ? [index] : all);
  const out = {};
  for (const n of names) {
    const props = mapping?.[n]?.mappings?.properties || {};
    for (const [f, cfg] of Object.entries(props)) {
      const t = (cfg && cfg.type) || 'object';
      if (t === 'alias') continue;          // aliases resolve engine-side; not a real field
      if (!(f in out)) out[f] = t;
    }
  }
  return out;
}

/** Field roles for a given index (or _all). One cached `_mapping` call. */
export async function schemaForSearch(baseUrl, index, signal) {
  const mapping = await fetchMapping(baseUrl, signal);
  return deriveRoles(fieldsOf(mapping, index));
}

/** Real user index names on the engine (no system indices). [] on failure. */
export async function indexNames(baseUrl, signal) {
  return userIndices(await fetchMapping(baseUrl, signal));
}

/** First index that looks like an email corpus, or null. */
export async function emailIndex(baseUrl, signal) {
  const mapping = await fetchMapping(baseUrl, signal);
  for (const n of userIndices(mapping)) {
    const props = mapping?.[n]?.mappings?.properties || {};
    if ('email_from' in props || 'email_subject' in props) return n;
  }
  return null;
}
