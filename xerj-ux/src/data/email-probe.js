// ============================================================
// XERJ Console — "does this engine hold an email corpus?" probe
//
// Sibling to data-probe.js / brains-probe.js. The Case Review dashboard
// (dashboards/case-review.js) is an email/document reader; it should appear in
// the nav ONLY once someone has indexed emails — i.e. an index carries the
// fields the EML extractor emits (`email_subject` / `email_from` /
// `attachment_name`). This asks that question so app.js can auto-activate the
// dashboard, exactly the pattern secondBrain uses via brains-probe.
//
// One `_mapping` call, classified client-side. Fail-CLOSED: false on any
// transport failure or non-OK status, so the dashboard never appears without a
// real email corpus behind it.
// ============================================================

const TTL_MS = 30_000;
const cache = new Map(); // baseUrl -> { at, value }

const EMAIL_MARKERS = ['email_subject', 'email_from', 'email_message_id', 'attachment_name'];

/** True if any non-system index's mapping carries the EML extractor's fields. */
export function mappingHasEmailCorpus(mapping) {
  for (const [index, def] of Object.entries(mapping || {})) {
    if (index.startsWith('.')) continue; // skip system indices
    const props = def?.mappings?.properties || {};
    if (EMAIL_MARKERS.some((f) => f in props)) return true;
  }
  return false;
}

/**
 * Whether the engine at `baseUrl` holds an email corpus. Never throws; returns
 * false on transport failure, non-OK status, or empty baseUrl.
 */
export async function emailCorpusPresent(baseUrl, signal) {
  // Direct `_mapping` goes same-origin (the console serves ES-compat on its own
  // origin). The logical `backendBaseUrl` ('http://localhost:9200') is a proxy
  // id, not reachable, so prefer the window origin like data-sources.js does.
  const base = (typeof window !== 'undefined' && window.location && window.location.origin)
    ? window.location.origin
    : (baseUrl || '').replace(/\/+$/, '');
  if (!base) return false;

  const hit = cache.get(base);
  if (hit && Date.now() - hit.at < TTL_MS) return hit.value;

  let value = false;
  try {
    const r = await fetch(`${base}/_mapping`, { signal, headers: { accept: 'application/json' } });
    if (r.ok) value = mappingHasEmailCorpus(await r.json());
  } catch {
    value = false; // engine down / CORS / abort — no claim
  }

  cache.set(base, { at: Date.now(), value });
  return value;
}
