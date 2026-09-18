// ============================================================
// XERJ Console — Reader data layer
//
// The fetches the corpus home and the reader need, each shaped (never
// throwing past this file) so the shell can render an honest state for every
// outcome. It is written against a TRANSPORT, because the same reader runs in
// two very different trust positions:
//
//   operator  data/transport-console.js — the signed-in console session.
//             Searches go through the session-authenticated panel proxy
//             (`/_xerj-console/api/v1/data-sources/…/search`); the graph is a
//             same-origin `/_graph/{brain}/ego` call.
//   guest     data/transport-guest.js — a share's read-only API key against
//             exactly the shared indices, via data/guest.js#guestRequest.
//             It cannot name another index, cannot reach a console endpoint,
//             and never enumerates brains or indices.
//
// A transport is:
//   { guest: boolean,
//     search(index, body, signal)  → ES `_search` response     (throws)
//     ego(brain, params, signal)   → { status, body }           (throws) }
// where a thrown error may carry `.kind`: 'unauthorized' | 'forbidden' |
// 'expired' | 'not-found' | 'blocked' | 'network' | 'http'.
//
// Nothing here fabricates a record. Every failure becomes an `error` /
// `status` the UI prints; there is no fallback corpus.
// ============================================================

import { groupNeighbors } from '../ux/reader-render.js';
import { highlightRequest } from '../ux/safe-dom.js';
import { buildSearchBody } from './search-body.js';
import { deriveRoles } from './schema-roles.js';

/** Error kinds that end a guest session (the shell logs the guest out). */
export const FATAL_KINDS = new Set(['unauthorized', 'expired']);

function errText(e) {
  if (!e) return 'unknown error';
  if (e.kind) {
    switch (e.kind) {
      case 'unauthorized': return 'not authorized';
      case 'forbidden':    return 'not permitted';
      case 'expired':      return 'share expired';
      case 'not-found':    return 'not found';
      case 'blocked':      return 'not part of this share';
      case 'network':      return 'engine unreachable';
      default:             return e.status ? `HTTP ${e.status}` : 'request failed';
    }
  }
  return String(e.message || e).slice(0, 200);
}

function shapeHit(h, index) {
  return {
    _index: typeof h._index === 'string' ? h._index : index,
    _id: String(h._id),
    _score: h._score,
    _source: (h._source && typeof h._source === 'object') ? h._source : {},
    highlight: (h.highlight && typeof h.highlight === 'object') ? h.highlight : undefined,
  };
}
function hitsOf(resp, index) {
  const list = resp && resp.hits && Array.isArray(resp.hits.hits) ? resp.hits.hits : [];
  return list.filter((h) => h && h._id != null).map((h) => shapeHit(h, index));
}
function totalOf(resp) {
  const t = resp && resp.hits && resp.hits.total;
  if (t && typeof t === 'object') return Number(t.value) || 0;
  return Number(t) || 0;
}

/** Field types of one index, from a `GET /{index}/_mapping` response. */
export function fieldTypesOf(mapping, index) {
  const out = {};
  const entry = mapping && (mapping[index] || Object.values(mapping)[0]);
  const props = (entry && entry.mappings && entry.mappings.properties) || {};
  for (const [f, cfg] of Object.entries(props)) {
    const t = (cfg && cfg.type) || 'object';
    if (t !== 'alias') out[f] = t;
  }
  return out;
}

export function makeReaderApi(transport) {
  const rolesByIndex = new Map();
  const brainByIndex = new Map();

  /** Field roles for an index (text / semantic / keyword / date fields). The
   *  conservative defaults apply when the mapping cannot be read. */
  async function roles(index, signal) {
    if (rolesByIndex.has(index)) return rolesByIndex.get(index);
    let r = deriveRoles({});
    if (typeof transport.mapping === 'function') {
      try { r = deriveRoles(fieldTypesOf(await transport.mapping(index, signal), index)); } catch (e) {
        if (e && e.name === 'AbortError') throw e;
      }
    }
    rolesByIndex.set(index, r);
    return r;
  }

  /**
   * Run the reader's search box. `{ hits, total, took, error, kind }` —
   * `hits` is `[]` on any failure; it is never padded or invented.
   */
  async function search(index, { q = '', type = 'match', size = 25 } = {}, signal) {
    try {
      const r = await roles(index, signal);
      const body = buildSearchBody(q, type, {}, r, { size, aggs: false });
      if ((q || '').trim() && type !== 'semantic') {
        body.highlight = highlightRequest([r.textField, 'email_subject', 'attachment_name'].filter(Boolean));
      }
      const resp = await transport.search(index, body, signal);
      return { hits: hitsOf(resp, index), total: totalOf(resp), took: Number(resp && resp.took) || 0 };
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
      return { hits: [], total: 0, took: 0, error: errText(e), kind: e && e.kind };
    }
  }

  /** One record by id. `{ hit }` or `{ hit: null, error, kind }`. */
  async function fetchRecord(index, id, signal) {
    try {
      const resp = await transport.search(index, { query: { ids: { values: [id] } }, size: 1 }, signal);
      const hit = hitsOf(resp, index)[0] || null;
      return hit ? { hit } : { hit: null, error: 'No such record in this index.', kind: 'not-found' };
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
      return { hit: null, error: errText(e), kind: e && e.kind };
    }
  }

  /** Same, across several candidate indices (a graph node from another
   *  dataset carries no index). First hit wins. */
  async function findRecord(indices, id, signal) {
    let last = null;
    for (const index of indices || []) {
      const r = await fetchRecord(index, id, signal);
      if (r.hit) return r;
      if (FATAL_KINDS.has(r.kind)) return r;
      last = r;
    }
    return last || { hit: null, error: 'No such record.', kind: 'not-found' };
  }

  /**
   * An email's attachment records, or an attachment's parent email. Joined
   * on `email_message_id` (extract/eml.rs stamps it on the email AND on every
   * attachment record it emits). `{ attachments: [], parent: null }` when
   * there is nothing to join on.
   */
  async function fetchRelated(hit, signal) {
    const s = (hit && hit._source) || {};
    const mid = s.email_message_id;
    if (typeof mid !== 'string' || !mid) return { attachments: [], parent: null };
    const index = hit._index;
    try {
      if (s.attachment_name) {
        const resp = await transport.search(index, {
          query: { bool: { filter: [{ term: { email_message_id: mid } }], must_not: [{ exists: { field: 'attachment_name' } }] } },
          size: 1,
        }, signal);
        return { attachments: [], parent: hitsOf(resp, index)[0] || null };
      }
      const resp = await transport.search(index, {
        query: { bool: { filter: [{ term: { email_message_id: mid } }, { exists: { field: 'attachment_name' } }] } },
        size: 50,
      }, signal);
      // One record per attachment (a PDF contributes one per page): collapse
      // to the first page so the list reads as attachments, not pages.
      const seen = new Set();
      const attachments = hitsOf(resp, index).filter((a) => {
        const k = String(a._source.attachment_name);
        if (seen.has(k)) return false;
        seen.add(k);
        return true;
      });
      return { attachments, parent: null };
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
      return { attachments: [], parent: null, kind: e && e.kind };
    }
  }

  /**
   * Which brain holds the graph for `index`. A guest's brain is the share's
   * and nothing else. For the operator: the `?brain=` hint, else whatever the
   * transport can discover, else the index slug (`ax-notes` → `notes`, the
   * name `xerj brain` derives from the folder).
   */
  async function resolveBrain(index, hint, signal) {
    if (transport.guest) return transport.brain || null;
    if (hint) return hint;
    if (!index) return null;
    if (brainByIndex.has(index)) return brainByIndex.get(index);
    let found = null;
    if (typeof transport.discoverBrain === 'function') {
      try { found = await transport.discoverBrain(index, signal); } catch (e) {
        if (e && e.name === 'AbortError') throw e;
      }
    }
    const brain = found || String(index).replace(/^ax-/, '');
    brainByIndex.set(index, brain);
    return brain;
  }

  /**
   * The 1-hop neighbourhood of a record. Shaped:
   *   { status: 'ok', groups, notShown }
   *   { status: 'no-brain' }     the edges index does not exist / no brain
   *   { status: 'no-links' }     brain exists, node has no edges
   *   { status: 'denied' }       the credential may not walk this graph
   *   { status: 'error', error }
   */
  async function fetchEgo(brain, id, signal) {
    if (!brain) return { status: 'no-brain', guest: !!transport.guest };
    const params = { node: id, hops: '1', direction: 'both', limit: '200', include_nodes: 'true', include_evidence: 'false' };
    try {
      let { status, body } = await transport.ego(brain, params, signal);
      // Node summaries are authorized against the brain's nodes index
      // separately (graph_api.rs). If that is refused, the links themselves
      // may still be readable — ask again without the summaries.
      if (status === 403) ({ status, body } = await transport.ego(brain, { ...params, include_nodes: 'false' }, signal));
      if (status === 404) return { status: 'no-brain', guest: !!transport.guest };
      if (status === 401 || status === 403) return { status: 'denied', guest: !!transport.guest, error: `HTTP ${status}`, kind: status === 401 ? 'unauthorized' : 'forbidden' };
      if (status < 200 || status >= 300) return { status: 'error', error: `HTTP ${status}` };
      const groups = groupNeighbors(body, id);
      const notShown = (body && body.not_shown && typeof body.not_shown === 'object') ? body.not_shown : {};
      const dangling = Array.isArray(notShown.dangling_ids) && notShown.dangling_ids.includes(id);
      if (!groups.length) return { status: 'no-links', dangling, notShown };
      return { status: 'ok', groups, notShown };
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
      if (e && e.kind === 'blocked') return { status: 'no-brain', guest: !!transport.guest };
      return { status: 'error', error: errText(e), kind: e && e.kind };
    }
  }

  /**
   * What one index holds, from the index itself (no catalog needed — a guest
   * is never granted `autoindex-catalog`, which lists every corpus on the
   * node). `{ index, records, emails, attachments, formats: [{key,count}],
   * error }`.
   */
  async function indexSummary(index, signal) {
    try {
      const resp = await transport.search(index, {
        size: 0,
        track_total_hits: true,
        query: { match_all: {} },
        aggs: {
          emails: { filter: { bool: { filter: [{ exists: { field: 'email_message_id' } }], must_not: [{ exists: { field: 'attachment_name' } }] } } },
          attachments: { filter: { exists: { field: 'attachment_name' } } },
          formats: { terms: { field: 'ax_format', size: 12 } },
        },
      }, signal);
      const a = (resp && resp.aggregations) || {};
      const buckets = (a.formats && Array.isArray(a.formats.buckets)) ? a.formats.buckets : [];
      return {
        index,
        records: totalOf(resp),
        emails: Number(a.emails && a.emails.doc_count) || 0,
        attachments: Number(a.attachments && a.attachments.doc_count) || 0,
        formats: buckets.map((b) => ({ key: String(b.key), count: Number(b.doc_count) || 0 })),
      };
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
      return { index, records: 0, emails: 0, attachments: 0, formats: [], error: errText(e), kind: e && e.kind };
    }
  }

  return { roles, search, fetchRecord, findRecord, fetchRelated, resolveBrain, fetchEgo, indexSummary };
}
