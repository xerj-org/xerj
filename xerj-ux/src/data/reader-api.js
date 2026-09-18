// ============================================================
// XERJ Console — Reader data layer
//
// The fetches the Reader needs, each shaped (never null, never throwing
// past this file) so app.js can render an honest state for every outcome:
//
//   fetchRecord(index, id)      one record by id (ids query through the
//                               console's panel proxy; direct for a guest)
//   findRecord(indices, id)     the same, when the caller has no index —
//                               a graph node id from another dataset
//   fetchRelated(hit)           an email's attachments / an attachment's
//                               parent email (email_message_id join)
//   resolveBrain(index, hint)   ?brain= → brain-meta nodes_index → slug
//   fetchEgo(brain, id)         GET /_graph/{brain}/ego?node=&hops=1
//
// Reads go the same way second-brain-api.js reads: the graph API and the
// brain-meta doc are same-origin ES-compat calls; record reads use the
// authenticated panel proxy (backends/xerj.js#rawSearch).
// ============================================================

import { rawSearch } from './backends/xerj.js';
import { groupNeighbors } from '../ux/reader-render.js';
import { parseBrainIndices } from './brains-probe.js';

const BRAIN_META_ID = '__xerj-brain-meta';

function origin() {
  return (typeof window !== 'undefined' && window.location && window.location.origin) ? window.location.origin : '';
}
const enc = encodeURIComponent;

function firstHit(resp, index) {
  const h = resp?.hits?.hits?.[0];
  if (!h) return null;
  return { _index: h._index || index, _id: h._id, _score: h._score, _source: h._source || {} };
}

/** One record by id. `{ hit }` or `{ hit: null, error }`. */
export async function fetchRecord(index, id, signal) {
  try {
    const resp = await rawSearch('', index, { query: { ids: { values: [id] } }, size: 1 }, signal);
    const hit = firstHit(resp, index);
    return hit ? { hit } : { hit: null, error: `no record ${id} in ${index}` };
  } catch (e) {
    return { hit: null, error: String(e && e.message || e) };
  }
}

/** Same, across several candidate indices (a graph node from another
 *  dataset carries no index). First hit wins. */
export async function findRecord(indices, id, signal) {
  const errors = [];
  for (const index of indices || []) {
    const r = await fetchRecord(index, id, signal);
    if (r.hit) return r;
    if (r.error) errors.push(r.error);
  }
  return { hit: null, error: errors[0] || `no record ${id} in any index` };
}

function hits(resp) {
  return (resp?.hits?.hits || []).map((h) => ({ _index: h._index, _id: h._id, _score: h._score, _source: h._source || {} }));
}

/**
 * An email's attachment records, or an attachment's parent email. Joined on
 * `email_message_id` (extract/eml.rs stamps it on the email AND on every
 * attachment record it emits). `{ attachments: [] , parent: null }` when
 * there is nothing to join on.
 */
export async function fetchRelated(hit, signal) {
  const s = (hit && hit._source) || {};
  const mid = s.email_message_id;
  if (!mid) return { attachments: [], parent: null };
  const index = hit._index;
  if (s.attachment_name) {
    try {
      const resp = await rawSearch('', index, {
        query: { bool: { filter: [{ term: { email_message_id: mid } }], must_not: [{ exists: { field: 'attachment_name' } }] } },
        size: 1,
      }, signal);
      return { attachments: [], parent: firstHit(resp, index) };
    } catch { return { attachments: [], parent: null }; }
  }
  try {
    const resp = await rawSearch('', index, {
      query: { bool: { filter: [{ term: { email_message_id: mid } }, { exists: { field: 'attachment_name' } }] } },
      size: 50,
    }, signal);
    // One record per attachment (a PDF contributes one per page): collapse
    // to the first page so the list reads as attachments, not pages.
    const seen = new Set();
    const attachments = hits(resp).filter((a) => {
      const k = a._source.attachment_name;
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    });
    return { attachments, parent: null };
  } catch { return { attachments: [], parent: null }; }
}

const brainByIndex = new Map();

/**
 * Which brain holds the graph for `index`:
 *   1. the caller's `?brain=` hint,
 *   2. a brain whose meta doc (`.xerj-memory-<b>-edges/__xerj-brain-meta`)
 *      lists the index in `nodes_index` (what `xerj brain` writes),
 *   3. the index's slug (`ax-notes` → `notes`, the name `xerj brain`
 *      derives from the folder).
 */
export async function resolveBrain(index, hint, signal) {
  if (hint) return hint;
  if (!index) return null;
  if (brainByIndex.has(index)) return brainByIndex.get(index);
  let found = null;
  try {
    const r = await fetch(`${origin()}/_cat/indices/.xerj-memory-*`, { signal, headers: { accept: 'text/plain, application/json' } });
    const brains = r.ok ? parseBrainIndices(await r.text()) : [];
    for (const b of brains.slice(0, 32)) {
      try {
        const m = await fetch(`${origin()}/${enc(`.xerj-memory-${b}-edges`)}/_doc/${enc(BRAIN_META_ID)}`, { signal, headers: { accept: 'application/json' } });
        if (!m.ok) continue;
        const doc = await m.json();
        const ni = String(doc?._source?.nodes_index || '');
        if (ni.split(',').map((x) => x.trim()).includes(index)) { found = b; break; }
      } catch { /* next brain */ }
    }
  } catch { /* no brain listing — fall through to the slug */ }
  const brain = found || String(index).replace(/^ax-/, '');
  brainByIndex.set(index, brain);
  return brain;
}

/**
 * The 1-hop neighbourhood of a record. Shaped:
 *   { status: 'ok', groups, notShown, dangling }
 *   { status: 'no-brain' }          the edges index does not exist
 *   { status: 'no-links' }          brain exists, node has no edges
 *   { status: 'error', error }
 */
export async function fetchEgo(brain, id, signal) {
  if (!brain) return { status: 'no-brain' };
  const qs = new URLSearchParams({ node: id, hops: '1', direction: 'both', limit: '200', include_nodes: 'true', include_evidence: 'false' });
  try {
    const r = await fetch(`${origin()}/_graph/${enc(brain)}/ego?${qs}`, { signal, headers: { accept: 'application/json' } });
    const text = await r.text();
    let body = null;
    try { body = text ? JSON.parse(text) : null; } catch { body = null; }
    if (r.status === 404) return { status: 'no-brain' };
    if (!r.ok) return { status: 'error', error: (body && body.error && (body.error.reason || body.error.type)) || `HTTP ${r.status}` };
    const groups = groupNeighbors(body, id);
    const dangling = !!(body?.not_shown?.dangling_ids || []).includes(id);
    if (!groups.length) return { status: 'no-links', dangling, notShown: body?.not_shown || {} };
    return { status: 'ok', groups, notShown: body?.not_shown || {} };
  } catch (e) {
    return { status: 'error', error: String(e && e.message || e) };
  }
}
