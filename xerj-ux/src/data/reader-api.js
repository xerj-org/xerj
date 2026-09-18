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
//     ego(brain, params, signal)   → { status, body }           (throws)
//     discoverBrains?(index)       → string[]  brains listing the index }
// where a thrown error may carry `.kind`: 'unauthorized' | 'forbidden' |
// 'expired' | 'not-found' | 'blocked' | 'network' | 'http'.
//
// Nothing here fabricates a record. Every failure becomes an `error` /
// `status` the UI prints; there is no fallback corpus.
// ============================================================

import { groupNeighbors } from '../ux/reader-render.js';
import { highlightRequest } from '../ux/safe-dom.js';
import { buildSearchBody, searchFieldsOf } from './search-body.js';
import { deriveRoles } from './schema-roles.js';

/** Error kinds that end a guest session (the shell logs the guest out). */
export const FATAL_KINDS = new Set(['unauthorized', 'expired']);
/** How many brains a record's graph panel walks when several list its index. */
export const MAX_BRAINS = 4;

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
/** Sort key for a record's `ax_locator` ("msg-s0", "att0-p3-s1", "s12"):
 *  numbers compare as numbers, so page 10 follows page 9. */
function locatorKey(h) {
  const loc = String((h._source && h._source.ax_locator) || '');
  const pad = loc.replace(/\d+/g, (d) => d.padStart(8, '0'));
  return `${loc.startsWith('msg') ? '0' : '1'}${pad}`;
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
        // Exactly the fields the query ran over (schema-roles.js#searchFields:
        // the text field plus subject / title / attachment name when the
        // mapping has them), so a match in a subject line is marked too.
        body.highlight = highlightRequest(searchFieldsOf(r));
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
    const index = hit._index;
    const out = { attachments: [], parent: null, fileRecord: null };
    try {
      const af = typeof s.ax_file === 'string' && s.ax_file ? s.ax_file : null;
      if (af && s.ax_locator === 'file') {
        // The file's own record: list what came out of the file.
        const resp = await transport.search(index, {
          query: { bool: { filter: [{ term: { ax_file: af } }], must_not: [{ term: { ax_locator: 'file' } }] } },
          size: 200,
        }, signal);
        const siblings = hitsOf(resp, index).sort((a, b) => locatorKey(a).localeCompare(locatorKey(b)));
        return { ...out, siblings, siblingsTruncated: totalOf(resp) > siblings.length };
      }
      if (af) {
        // Any other record: find its file's record — the node the brain's
        // file-level links (same folder, markdown link, path citation) hang on.
        const resp = await transport.search(index, {
          query: { bool: { filter: [{ term: { ax_file: af } }, { term: { ax_locator: 'file' } }] } },
          size: 1,
        }, signal);
        out.fileRecord = hitsOf(resp, index)[0] || null;
      }
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
      if (FATAL_KINDS.has(e && e.kind)) return { ...out, kind: e.kind };
    }
    const mid = s.email_message_id;
    if (typeof mid !== 'string' || !mid) return out;
    try {
      if (s.attachment_name) {
        const resp = await transport.search(index, {
          query: { bool: { filter: [{ term: { email_message_id: mid } }], must_not: [{ exists: { field: 'attachment_name' } }] } },
          size: 1,
        }, signal);
        return { ...out, parent: hitsOf(resp, index)[0] || null };
      }
      const resp = await transport.search(index, {
        query: { bool: { filter: [{ term: { email_message_id: mid } }, { exists: { field: 'attachment_name' } }] } },
        size: 200,
      }, signal);
      // One record per attachment (a PDF contributes one per page): collapse
      // to each file's LOWEST page so the list reads as attachments, not
      // pages, and a click opens page one. Filter hits arrive in no useful
      // order, so the minimum is taken here rather than trusted from the sort.
      const byName = new Map();
      const pageOf = (a) => { const n = Number(a._source.page); return Number.isFinite(n) ? n : 0; };
      for (const a of hitsOf(resp, index)) {
        const k = String(a._source.attachment_name);
        const cur = byName.get(k);
        if (!cur || pageOf(a) < pageOf(cur)) byName.set(k, a);
      }
      return { ...out, attachments: [...byName.values()], truncated: totalOf(resp) > 200 };
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
      return { ...out, kind: e && e.kind };
    }
  }

  /**
   * Which brains hold a graph for `index`, in the order they are walked. A
   * guest's brain is the share's and nothing else. For the operator: the
   * `?brain=` hint alone, else every brain the transport can discover (at most
   * MAX_BRAINS), else the index slug (`ax-notes` → `notes`, the name
   * `xerj brain` derives from the folder).
   */
  async function resolveBrains(index, hint, signal) {
    if (transport.guest) return transport.brain ? [transport.brain] : [];
    if (hint) return [hint];
    if (!index) return [];
    if (brainByIndex.has(index)) return brainByIndex.get(index);
    let found = [];
    try {
      if (typeof transport.discoverBrains === 'function') found = await transport.discoverBrains(index, signal);
      else if (typeof transport.discoverBrain === 'function') found = [await transport.discoverBrain(index, signal)];
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
    }
    const brains = (Array.isArray(found) ? found : []).filter((b) => typeof b === 'string' && b).slice(0, MAX_BRAINS);
    if (!brains.length) brains.push(String(index).replace(/^ax-/, ''));
    brainByIndex.set(index, brains);
    return brains;
  }

  /** The first of `resolveBrains` (kept for callers that want one name). */
  async function resolveBrain(index, hint, signal) {
    return (await resolveBrains(index, hint, signal))[0] || null;
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
   * The graph panel's data for an open record: its own 1-hop neighbourhood,
   * plus — when it is not the file's record — the neighbourhood of the file it
   * came from. autoindex's file-level detectors (same_dir, mdlink, pathcite,
   * href) link FILE records; an email or a PDF page opened on its own would
   * otherwise always read "no links" beside a file that has several.
   */
  async function fetchGraphOne(brain, hit, fileRecord, signal) {
    const own = await fetchEgo(brain, hit._id, signal);
    if (!fileRecord || fileRecord._id === hit._id) return own;
    if (own.status !== 'ok' && own.status !== 'no-links') return own; // no brain / denied / error: the same answer would come back
    const viaFile = await fetchEgo(brain, fileRecord._id, signal);
    if (viaFile.status !== 'ok') return own;
    const groups = (own.groups || []).map((g) => ({ ...g, items: [...g.items] }));
    const seen = new Set(groups.flatMap((g) => g.items.map((it) => `${g.type}\n${it.id}`)));
    let added = 0;
    for (const g of viaFile.groups) {
      for (const it of g.items) {
        if (it.id === hit._id || seen.has(`${g.type}\n${it.id}`)) continue;
        let into = groups.find((x) => x.type === g.type);
        if (!into) { into = { type: g.type, label: g.label, items: [] }; groups.push(into); }
        into.items.push({ ...it, via: 'file' });
        added++;
      }
    }
    if (!groups.length) return own;
    const fs = fileRecord._source || {};
    return { status: 'ok', groups, notShown: own.notShown || viaFile.notShown || {}, viaFile: added, filePath: typeof fs.ax_path === 'string' ? fs.ax_path : null };
  }

  const sortItems = (items) => items.sort((a, b) => String(a.title).localeCompare(String(b.title)) || String(a.id).localeCompare(String(b.id)));

  /**
   * The graph panel over EVERY candidate brain (`resolveBrains`), walked in
   * order and merged. Two `xerj brain` runs over different folders land in the
   * same index with two brains that each hold links only for their own files;
   * asking one of them read "no links" for the other's records (PR #945
   * review). The result always says which brains were consulted (`brains`)
   * and, when links were found, which of them had any (`brainsWithLinks`);
   * every neighbour carries the brain its link came from.
   *
   * With no links anywhere the most informative answer wins: a brain that
   * exists but has none over a refusal, a refusal over an error, an error over
   * a missing brain.
   */
  async function fetchGraph(brainOrList, hit, fileRecord, signal) {
    const brains = (Array.isArray(brainOrList) ? brainOrList : [brainOrList]).filter((b) => typeof b === 'string' && b).slice(0, MAX_BRAINS);
    if (!brains.length) return { status: 'no-brain', guest: !!transport.guest, brain: null, brains: [] };
    if (brains.length === 1) return { ...(await fetchGraphOne(brains[0], hit, fileRecord, signal)), brain: brains[0], brains };
    const results = [];
    for (const brain of brains) results.push({ brain, r: await fetchGraphOne(brain, hit, fileRecord, signal) });
    const oks = results.filter((x) => x.r.status === 'ok');
    if (oks.length) {
      const groups = [];
      const seen = new Set();
      const notShown = {};
      let viaFile = 0;
      let filePath = null;
      for (const { brain, r } of oks) {
        for (const g of r.groups) {
          for (const it of g.items) {
            const key = `${g.type}\n${it.id}`;
            if (seen.has(key)) continue;
            seen.add(key);
            let into = groups.find((x) => x.type === g.type);
            if (!into) { into = { type: g.type, label: g.label, items: [] }; groups.push(into); }
            into.items.push({ ...it, brain });
            if (it.via === 'file') viaFile++;
          }
        }
        if (!filePath && r.filePath) filePath = r.filePath;
        for (const k of ['edges_clipped', 'frontier_clipped', 'dangling_nodes']) {
          const n = Number(r.notShown && r.notShown[k]) || 0;
          if (n) notShown[k] = (Number(notShown[k]) || 0) + n;
        }
      }
      for (const g of groups) sortItems(g.items);
      return { status: 'ok', groups, notShown, viaFile, filePath, brain: oks[0].brain, brains, brainsWithLinks: oks.map((x) => x.brain) };
    }
    const pick = (st) => results.find((x) => x.r.status === st);
    const best = pick('no-links') || pick('denied') || pick('error') || pick('no-brain') || results[0];
    return { ...best.r, brain: best.brain, brains };
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

  return { roles, search, fetchRecord, findRecord, fetchRelated, resolveBrain, resolveBrains, fetchEgo, fetchGraph, indexSummary };
}
