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
import { buildSearchBody, searchFieldsOf, queryProblem } from './search-body.js';
import { deriveRoles, withSearchField } from './schema-roles.js';

/** Error kinds that end a guest session (the shell logs the guest out). */
export const FATAL_KINDS = new Set(['unauthorized', 'expired']);
/** How many brains a record's graph panel walks when several list its index. */
export const MAX_BRAINS = 4;
/**
 * An email's attachment RECORDS are read in pages of ATTACHMENT_PAGE, at most
 * ATTACHMENT_RECORDS_READ of them. A PDF contributes one record per page
 * section, so one attachment can be hundreds of records (autoindex's
 * per-document cap, extract/mod.rs#MAX_RECORDS_PER_FILE, is 4096; an email can
 * carry several documents). Past the limit the list is marked incomplete —
 * with the numbers — never silently cut (PR #945 review: a fixed `size: 200`
 * hid every attachment after a 200-page PDF and said nothing).
 */
export const ATTACHMENT_PAGE = 1000;
export const ATTACHMENT_RECORDS_READ = 5000;
/** The fields an attachment list entry shows. The join asks for these and
 *  nothing else: never the page text, never a vector. */
const ATTACHMENT_SOURCE = ['attachment_name', 'attachment_content_type', 'attachment_bytes', 'page', 'ax_locator', 'ax_file', 'email_message_id'];
/** How many records of a file are listed on its file record. */
export const SIBLINGS_SHOWN = 200;
/**
 * One hit per email MESSAGE. autoindex writes `msg-s0` as the first (usually
 * only) section of every .eml and `m<offset>-msg-s0` for every message of a
 * mailbox (PR #949). Counting records that carry `email_message_id` instead
 * missed every email without a Message-ID header (the PR #945 review corpus
 * holds 9 emails and the guest card said 8) and counted a long email once per
 * section. Records not written by autoindex (no `ax_locator`) still count by
 * message id.
 */
export const EMAIL_MESSAGE_QUERY = { bool: { should: [
  { term: { ax_locator: 'msg-s0' } },
  { wildcard: { ax_locator: { value: 'm*-msg-s0' } } },
  { bool: { filter: [{ exists: { field: 'email_message_id' } }], must_not: [{ exists: { field: 'attachment_name' } }, { exists: { field: 'ax_locator' } }] } },
], minimum_should_match: 1 } };

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
/**
 * Where a record sits inside its file, read from its `ax_locator`.
 *
 * autoindex writes ONE message per `.eml` — `msg-s0` (its sections `msg-s1`…),
 * `att<N>-s0`, `att<N>-p<page>-s0`, `att<N>-card` — and, for a mailbox (mbox /
 * Google Takeout, PR #949), MANY messages per file, each namespaced by its
 * byte offset: `m1048-msg-s0`, `m1048-att0-p3-s0`, `m2210-raw-s0`. Every record
 * of one mailbox shares ONE `ax_file`, so "this message's attachments" is the
 * file AND the message prefix; joined on the file alone, one email of a
 * mailbox would list the attachments of every other message in it.
 *
 * `{ message, part, ordinal }` — `message` is `''` for an `.eml` and
 * `m<offset>-` inside a mailbox; `part` is `msg` | `att` | `raw`; `ordinal` is
 * the attachment's number. `null` for any other locator (`file`, `s12`, a
 * record not written by autoindex).
 */
export function locatorParts(loc) {
  const m = /^(m\d+-)?(msg|att(\d+)|raw)(?=-|$)/.exec(String(loc || ''));
  if (!m) return null;
  return { message: m[1] || '', part: m[3] != null ? 'att' : m[2], ordinal: m[3] != null ? Number(m[3]) : null };
}
/** Sort key for a record's `ax_locator` ("msg-s0", "att0-p3-s1", "s12",
 *  "m1048-att0-p3-s0"): numbers compare as numbers, so page 10 follows page 9
 *  and a mailbox's messages follow their byte offsets; within one message the
 *  message's own sections come before its attachments. */
function locatorKey(h) {
  const loc = String((h._source && h._source.ax_locator) || '');
  const pad = (x) => x.replace(/\d+/g, (d) => d.padStart(20, '0'));
  const [, message = '', rest] = /^(m\d+-)?(.*)$/s.exec(loc);
  return `${pad(message)}${rest.startsWith('msg') ? '0' : '1'}${pad(rest)}`;
}
/** The attachment a record belongs to. autoindex's EML extractor numbers the
 *  attachments of one message and puts the ordinal in the locator
 *  (`att0-p3-s1`, `att2-s0`, `att5-card`, in a mailbox `m1048-att0-…`), so two
 *  attachments that share a FILE NAME stay two. Records from anywhere else
 *  fall back to the name. */
function attachmentKey(h) {
  const lp = locatorParts(h._source && h._source.ax_locator);
  return lp && lp.part === 'att'
    ? { key: `${lp.message}#${lp.ordinal}`, ordinal: lp.ordinal }
    : { key: `name:${String(h._source && h._source.attachment_name)}`, ordinal: Infinity };
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
   *
   *   field  a corpus card's sample query names the field it was written for;
   *          it joins the search fields (schema-roles.js#withSearchField)
   *   size   how many hits to return (the view's SHOW MORE raises it)
   *
   * `{ hits: [], hint }` when the query cannot run as typed (TERM / RANGE
   * without `field=value`): nothing is sent, and above all not `match_all`.
   */
  async function search(index, { q = '', type = 'match', size = 25, field = null } = {}, signal) {
    const hint = queryProblem(q, type);
    if (hint) return { hits: [], total: 0, took: 0, hint };
    try {
      const r = withSearchField(await roles(index, signal), field);
      const body = buildSearchBody(q, type, {}, r, { size, aggs: false });
      if ((q || '').trim() && type !== 'semantic') {
        // Exactly the fields the query ran over (schema-roles.js#searchFields:
        // every text field plus subject / title / attachment name when the
        // mapping has them), so a match in a subject line is marked too.
        body.highlight = highlightRequest(searchFieldsOf(r));
      }
      const resp = await transport.search(index, body, signal);
      return { hits: hitsOf(resp, index), total: totalOf(resp), took: Number(resp && resp.took) || 0, fields: searchFieldsOf(r) };
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
   * Read every attachment record matching `filter`, a light page at a time,
   * and collapse them to ONE entry per attachment at its lowest page (a click
   * opens page one). `{ attachments, truncated }` — `truncated` is
   * `{ read, total }` when the engine holds more records than were read.
   */
  async function readAttachments(index, filter, sorted, signal) {
    const best = new Map();
    const pageOf = (a) => { const n = Number(a._source.page); return Number.isFinite(n) ? n : 0; };
    const lower = (a, b) => (pageOf(a) - pageOf(b)) || locatorKey(a).localeCompare(locatorKey(b));
    let read = 0;
    let total = 0;
    while (read < ATTACHMENT_RECORDS_READ) {
      const resp = await transport.search(index, {
        query: { bool: { filter: [...filter, { exists: { field: 'attachment_name' } }] } },
        // A filter has no useful order of its own; paging needs a stable one.
        // (`ax_locator` is only known to exist beside `ax_file`.)
        ...(sorted ? { sort: [{ ax_locator: 'asc' }] } : {}),
        from: read,
        size: Math.min(ATTACHMENT_PAGE, ATTACHMENT_RECORDS_READ - read),
        track_total_hits: true,
        _source: ATTACHMENT_SOURCE,
      }, signal);
      const hits = hitsOf(resp, index);
      total = Math.max(total, totalOf(resp));
      for (const a of hits) {
        const { key, ordinal } = attachmentKey(a);
        const cur = best.get(key);
        if (!cur || lower(a, cur.hit) < 0) best.set(key, { hit: a, ordinal });
      }
      read += hits.length;
      if (!hits.length || read >= total) break;
    }
    const attachments = [...best.values()]
      .sort((x, y) => (x.ordinal - y.ordinal) || String(x.hit._source.attachment_name).localeCompare(String(y.hit._source.attachment_name)))
      .map((x) => x.hit);
    return { attachments, truncated: total > read ? { read, total } : null };
  }

  /**
   * An email's attachment records, or an attachment's parent email.
   *
   * Joined on `ax_file` — the id autoindex stamps on EVERY record that came
   * out of one file — narrowed to the record's MESSAGE by its locator
   * (`locatorParts`): an attachment list is the file's `<message>att…`
   * records, a parent is the file's `<message>msg-…` record. For an `.eml` the
   * message part is empty (one file, one message); inside a mailbox it is the
   * message's `m<offset>-`. The first version joined on `email_message_id`,
   * which the PR #945 review showed is wrong twice: the extractor stamps it
   * only when the message HAS a Message-ID header (an email without one
   * listed no attachments), and two files can carry the same id (each copy
   * listed the other's attachments). `email_message_id` remains the fallback
   * for records that carry no `ax_file` (not written by autoindex); a record
   * with an `ax_file` but a locator of another shape joins on the file alone.
   * `{ attachments: [], parent: null }` when there is nothing to join on;
   * `attachmentsError` when the join itself failed.
   */
  async function fetchRelated(hit, signal) {
    const s = (hit && hit._source) || {};
    const index = hit._index;
    const out = { attachments: [], attachmentsTruncated: null, parent: null, fileRecord: null };
    const af = typeof s.ax_file === 'string' && s.ax_file ? s.ax_file : null;
    try {
      if (af && s.ax_locator === 'file') {
        // The file's own record: list what came out of the file.
        const resp = await transport.search(index, {
          query: { bool: { filter: [{ term: { ax_file: af } }], must_not: [{ term: { ax_locator: 'file' } }] } },
          sort: [{ ax_locator: 'asc' }],
          size: SIBLINGS_SHOWN,
          track_total_hits: true,
        }, signal);
        const siblings = hitsOf(resp, index).sort((a, b) => locatorKey(a).localeCompare(locatorKey(b)));
        const total = totalOf(resp);
        return { ...out, siblings, siblingsTruncated: total > siblings.length, siblingsTotal: total };
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
    const mid = typeof s.email_message_id === 'string' && s.email_message_id ? s.email_message_id : null;
    const join = af ? [{ term: { ax_file: af } }] : (mid ? [{ term: { email_message_id: mid } }] : null);
    if (!join) return out;
    const isEmailish = s.attachment_name != null || s.email_subject != null || s.email_from != null || mid || s.ax_format === 'eml';
    if (!isEmailish) return out; // a note or a PDF page: nothing to join
    // The message inside the file (see locatorParts): `m1048-` in a mailbox,
    // empty for an .eml. Only an autoindex locator narrows the join.
    const lp = af ? locatorParts(s.ax_locator) : null;
    const inMessage = (part) => (lp ? [{ prefix: { ax_locator: `${lp.message}${part}` } }] : []);
    try {
      if (s.attachment_name) {
        // The message this attachment came with: the message's own records
        // (neither an attachment nor the file record); its first section.
        const resp = await transport.search(index, {
          query: { bool: { filter: [...join, ...inMessage('msg-')], must_not: [{ exists: { field: 'attachment_name' } }, { term: { ax_locator: 'file' } }] } },
          ...(af ? { sort: [{ ax_locator: 'asc' }] } : {}),
          size: 20,
        }, signal);
        const parent = hitsOf(resp, index).sort((a, b) => locatorKey(a).localeCompare(locatorKey(b)))[0] || null;
        return { ...out, parent };
      }
      if (lp && lp.part === 'raw') return out; // a mailbox message autoindex could not parse: no attachments were read from it
      const { attachments, truncated } = await readAttachments(index, [...join, ...inMessage('att')], !!af, signal);
      return { ...out, attachments, attachmentsTruncated: truncated };
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
      return { ...out, kind: e && e.kind, attachmentsError: errText(e) };
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
      const [resp, emails] = await Promise.all([
        transport.search(index, {
          size: 0,
          track_total_hits: true,
          query: { match_all: {} },
          aggs: {
            attachments: { filter: { exists: { field: 'attachment_name' } } },
            formats: { terms: { field: 'ax_format', size: 12 } },
          },
        }, signal),
        // Emails are counted with a QUERY, not a filter aggregation: the
        // engine's filter aggregation counts 0 for a wildcard with an inner
        // `*` (`m*-msg-s0`) while `_count` / a query answers right (#959,
        // measured on a mailbox: 0 vs 5).
        transport.search(index, { size: 0, track_total_hits: true, query: EMAIL_MESSAGE_QUERY }, signal),
      ]);
      const a = (resp && resp.aggregations) || {};
      const buckets = (a.formats && Array.isArray(a.formats.buckets)) ? a.formats.buckets : [];
      return {
        index,
        records: totalOf(resp),
        emails: totalOf(emails),
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
