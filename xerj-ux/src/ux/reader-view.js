// ============================================================
// XERJ Console — Reader view (the one stateful piece)
//
// Search box · result list · the open record · the records linked to it.
// Used unchanged by the operator shell (app.js, section READER) and by the
// guest shell (guest-app.js); what differs is the `api` it is given
// (data/reader-api.js over the console or the guest transport).
//
// All DOM goes through ux/safe-dom.js#mount. This file holds no markup
// strings and calls no HTML-parsing API: record data only ever becomes text
// nodes and inert attribute values. Navigation is by ordinary in-app hash
// links (`#/reader?index=…&id=…`) — the shell hands the parsed route back
// through `setRoute`, so "open this record" needs no click handler at all.
// ============================================================

import { h, mount } from './safe-dom.js';
import { renderResultList, renderRecord, renderGraphPanel, readerHref } from './reader-render.js';
import { QUERY_TYPES, queryPlaceholder } from '../data/search-body.js';
import { FATAL_KINDS } from '../data/reader-api.js';

/** `#/reader?index=a&id=b&brain=c` → { index, id, brain } (strings or null). */
export function parseReaderRoute(hash) {
  const raw = String(hash || '');
  const qi = raw.indexOf('?');
  const qs = new URLSearchParams(qi >= 0 ? raw.slice(qi + 1) : '');
  const get = (k) => { const v = qs.get(k); return v == null || v === '' ? null : v; };
  return { index: get('index'), id: get('id'), brain: get('brain') };
}

/**
 * What the view keeps of `reader-api.js#fetchRelated`'s answer. EVERYTHING it
 * returned, with the list defaults the renderer relies on — the first version
 * copied four named keys and so dropped the join's `truncated` flag on the
 * floor (PR #945 review): the data layer knew the attachment list was
 * incomplete and the page never said so.
 */
export function relatedForView(related, hit) {
  const r = related || {};
  const isFile = !!(hit && hit._source && hit._source.ax_locator === 'file');
  return {
    ...r,
    attachments: r.attachments || [],
    parent: r.parent || null,
    fileRecord: r.fileRecord || null,
    siblings: r.siblings || (isFile ? [] : undefined),
    siblingsTruncated: !!r.siblingsTruncated,
  };
}

/** Results per page of the list, and the most SHOW MORE will ever ask for. */
export const PAGE_SIZE = 25;
export const MAX_SHOWN = 200;

export class ReaderView {
  /**
   * opts.api       data/reader-api.js#makeReaderApi(transport)
   * opts.indices   () => string[]   indices the viewer may pick from
   * opts.guest     boolean
   * opts.onFatal   (kind) => void   a guest's key stopped working / expired
   * opts.onStatus  (status) => void optional: { kind, label } for a shell pill
   */
  constructor(opts) {
    this.api = opts.api;
    this.indicesFn = opts.indices || (() => []);
    this.guest = !!opts.guest;
    this.onFatal = opts.onFatal || (() => {});
    const report = opts.onStatus || (() => {});
    /** The last status this view reported — the shell re-reads it when it
     *  repaints around us without a new search having run. */
    this.lastStatus = null;
    this.onStatus = (st) => { this.lastStatus = st; report(st); };
    this.root = null;
    this.slots = null;
    this.seq = 0;
    this.searchSeq = 0;
    this.abort = null;
    this.searchAbort = null;
    this.s = {
      index: null, id: null, brain: null, brains: [], brainHint: null,
      q: '', type: 'match',
      field: null,           // a sample query's own field (cleared by the next typed search)
      size: PAGE_SIZE,
      result: null,          // { hits, total, took, error, hint, pending }
      hit: null, recordError: null, recordKind: null, loading: false,
      related: {},           // { attachments, parent }
      graph: { status: 'idle' },
    };
    this._onKey = (e) => this.handleKey(e);
    this._onChange = (e) => this.handleChange(e);
    this._onClick = (e) => this.handleClick(e);
  }

  // ----- routing -------------------------------------------------------

  /** Apply a parsed route. Loads only what changed. */
  setRoute({ index, id, brain } = {}) {
    const allowed = this.indicesFn();
    let idx = index;
    // A guest can only ever be on one of the share's indices; an index from
    // the URL that is not among them is ignored, not requested.
    if (this.guest && idx && !allowed.includes(idx)) idx = null;
    if (!idx) idx = this.s.index && (!this.guest || allowed.includes(this.s.index)) ? this.s.index : (allowed[0] || null);
    const indexChanged = idx !== this.s.index;
    const idChanged = (id || null) !== this.s.id;
    const hint = this.guest ? null : (brain || null);
    const hintChanged = hint !== this.s.brainHint;
    this.s.index = idx;
    this.s.brainHint = hint;
    this.s.id = id || null;
    if (indexChanged) { this.s.result = null; this.s.hit = null; this.s.size = PAGE_SIZE; }
    // No index at all (an empty engine): there is nothing to load, and the
    // shell's status pill must not sit on "LOADING…" forever waiting for a
    // search that will never run (PR #945 review).
    if (!idx) this.onStatus({ kind: 'live', label: this.guest ? 'NOTHING SHARED' : 'LIVE · NOTHING INDEXED' });
    // Coming back to a view whose last load FAILED asks again (a refused or
    // unreachable engine may have recovered). A record the engine answered
    // "no such id" for is not re-asked on every repaint.
    const searchFailed = !!(this.s.result && this.s.result.error);
    const recordFailed = !!(this.s.id && !this.s.hit && !this.s.loading && this.s.recordKind !== 'not-found');
    if (idx && (indexChanged || !this.s.result || searchFailed)) this.runSearch();
    if (indexChanged || idChanged || recordFailed) this.loadRecord();
    // Only `&brain=` changed: the record stands, the graph half is re-asked
    // with the new hint (PR #945 review: it used to keep the old brain's answer).
    else if (hintChanged && this.s.hit) this.reloadGraph();
    this.paint();
  }

  // ----- data ----------------------------------------------------------

  fatal(kind) {
    if (this.guest && FATAL_KINDS.has(kind)) { this.onFatal(kind); return true; }
    return false;
  }

  async runSearch() {
    const index = this.s.index;
    if (!index) return;
    if (this.searchAbort) this.searchAbort.abort();
    const ac = this.searchAbort = new AbortController();
    const seq = ++this.searchSeq;
    // SHOW MORE keeps the list on screen while the longer one loads.
    const keep = this.s.size > PAGE_SIZE && this.s.result && !this.s.result.error ? this.s.result.hits : [];
    this.s.result = { hits: keep, total: keep.length ? this.s.result.total : 0, pending: true };
    this.paintList();
    let r;
    try { r = await this.api.search(index, { q: this.s.q, type: this.s.type, size: this.s.size, field: this.s.field }, ac.signal); } catch { return; }
    if (seq !== this.searchSeq) return;
    if (this.fatal(r.kind)) return;
    this.s.result = r;
    this.onStatus(r.error ? { kind: 'live-error', label: `READER: ${r.error}` } : { kind: 'live', label: `LIVE · ${index}` });
    this.paintList();
  }

  async loadRecord() {
    if (this.abort) this.abort.abort();
    const ac = this.abort = new AbortController();
    const seq = ++this.seq;
    const { index, id } = this.s;
    this.s.hit = null; this.s.recordError = null; this.s.recordKind = null; this.s.related = {}; this.s.graph = { status: 'idle' };
    if (!index || !id) { this.s.loading = false; this.paintRecord(); this.paintGraph(); return; }
    this.s.loading = true;
    this.paintRecord(); this.paintGraph();
    try {
      const rec = await this.api.fetchRecord(index, id, ac.signal);
      if (seq !== this.seq) return;
      if (this.fatal(rec.kind)) return;
      this.s.loading = false;
      this.s.hit = rec.hit;
      this.s.recordError = rec.hit ? null : (rec.error || 'Not found.');
      this.s.recordKind = rec.hit ? null : (rec.kind || 'error');
      this.paintRecord(); this.paintList();
      if (!rec.hit) { this.paintGraph(); return; }

      this.s.graph = { status: 'loading' };
      this.paintGraph();
      const [related, brains] = await Promise.all([
        this.api.fetchRelated(rec.hit, ac.signal),
        this.api.resolveBrains(index, this.s.brainHint, ac.signal),
      ]);
      if (seq !== this.seq) return;
      if (this.fatal(related.kind)) return;
      this.s.related = relatedForView(related, rec.hit);
      this.s.brains = brains;
      this.s.brain = brains[0] || null;
      this.paintRecord();
      this.paintGraph();
      const graph = await this.api.fetchGraph(brains, rec.hit, related.fileRecord || null, ac.signal);
      if (seq !== this.seq) return;
      if (graph.kind === 'expired' && this.fatal('expired')) return;
      this.s.graph = graph;
      this.paintGraph();
    } catch { /* aborted: a newer load owns the view */ }
  }

  /** Re-ask the graph half for the open record (the `&brain=` hint changed). */
  async reloadGraph() {
    const hit = this.s.hit;
    if (!hit) return;
    if (this.abort) this.abort.abort();
    const ac = this.abort = new AbortController();
    const seq = ++this.seq;
    this.s.graph = { status: 'loading' };
    this.paintGraph();
    try {
      const brains = await this.api.resolveBrains(this.s.index, this.s.brainHint, ac.signal);
      if (seq !== this.seq) return;
      this.s.brains = brains;
      this.s.brain = brains[0] || null;
      this.paintGraph();
      const graph = await this.api.fetchGraph(brains, hit, (this.s.related && this.s.related.fileRecord) || null, ac.signal);
      if (seq !== this.seq) return;
      if (graph.kind === 'expired' && this.fatal('expired')) return;
      this.s.graph = graph;
      this.paintGraph();
    } catch { /* aborted: a newer load owns the view */ }
  }

  // ----- DOM -----------------------------------------------------------

  /** (Re)attach to a container: builds the skeleton and paints every slot
   *  from current state. Safe to call after the shell re-rendered around us. */
  attach(container) {
    this.detach();
    this.root = container;
    if (!container) return;
    const indices = this.indicesFn();
    mount(container, h('div', { class: 'rd-root' },
      h('div', { class: 'rd-search' },
        h('input', { type: 'search', class: 'rd-q', name: 'rd-q', value: this.s.q, 'aria-label': 'Search this corpus',
          placeholder: queryPlaceholder(this.s.type), autocomplete: 'off', spellcheck: 'false' }),
        h('select', { class: 'rd-type', name: 'rd-type', 'aria-label': 'Query type' },
          QUERY_TYPES.map((t) => h('option', { value: t, selected: t === this.s.type }, t.toUpperCase()))),
        indices.length > 1
          ? h('select', { class: 'rd-index', name: 'rd-index', 'aria-label': 'Index' },
            indices.map((i) => h('option', { value: i, selected: i === this.s.index }, i)))
          : h('span', { class: 'rd-index-one mono faint' }, this.s.index || '')),
      h('div', { class: 'rd-cols' },
        h('section', { class: 'rd-col rd-col--list' }, h('div', { class: 'key', 'data-rd-slot': 'list-head' }), h('div', { 'data-rd-slot': 'list' })),
        h('section', { class: 'rd-col rd-col--record' }, h('div', { class: 'key' }, 'RECORD'), h('div', { 'data-rd-slot': 'record' }))),
      h('section', { class: 'rd-graph' }, h('div', { class: 'key' }, 'LINKED RECORDS · FROM THE BRAIN'), h('div', { 'data-rd-slot': 'graph' }))));
    const slot = (n) => container.querySelector(`[data-rd-slot="${n}"]`);
    this.slots = { listHead: slot('list-head'), list: slot('list'), record: slot('record'), graph: slot('graph') };
    container.addEventListener('keydown', this._onKey);
    container.addEventListener('change', this._onChange);
    container.addEventListener('click', this._onClick);
    this.paint();
  }

  detach() {
    if (this.root) {
      this.root.removeEventListener('keydown', this._onKey);
      this.root.removeEventListener('change', this._onChange);
      this.root.removeEventListener('click', this._onClick);
    }
    this.root = null; this.slots = null;
  }

  paint() { this.paintIndex(); this.paintList(); this.paintRecord(); this.paintGraph(); }

  /** The index label / picker beside the search box follows the route
   *  (attach() renders it before setRoute() has set the index). */
  paintIndex() {
    if (!this.root) return;
    const one = this.root.querySelector('.rd-index-one');
    if (one) mount(one, this.s.index || '');
    const sel = this.root.querySelector('.rd-index');
    if (sel && this.s.index && sel.value !== this.s.index) sel.value = this.s.index;
  }

  paintList() {
    if (!this.slots) return;
    const r = this.s.result;
    const hits = (r && r.hits) || [];
    let head = 'RESULTS';
    let empty;
    let more = null;
    if (!this.s.index) empty = this.guest ? 'Nothing is shared.' : 'Nothing is indexed yet. Run: xerj brain <folder>';
    else if (!r || (r.pending && !hits.length)) { head = 'SEARCHING…'; empty = 'Searching…'; }
    else if (r.error) { head = 'SEARCH FAILED'; empty = `Search failed: ${r.error}. Nothing is shown in its place.`; }
    else if (r.hint) { head = 'NOT SEARCHED'; empty = r.hint; }
    else {
      head = `${r.total} RESULT${r.total === 1 ? '' : 'S'}${hits.length < r.total ? ` · SHOWING ${hits.length}` : ''} · CLICK TO OPEN`;
      empty = (this.s.q || '').trim() ? 'No matches. Try a broader query or another query type.' : `No records in ${this.s.index}.`;
      if (hits.length < r.total) {
        more = r.pending ? h('div', { class: 'rd-more faint mono' }, 'Loading more…')
          : hits.length < MAX_SHOWN
            ? h('button', { class: 'text-btn rd-more', type: 'button', 'data-rd-more': '1' }, `SHOW ${Math.min(PAGE_SIZE, r.total - hits.length, MAX_SHOWN - hits.length)} MORE`)
            : h('div', { class: 'rd-more faint mono' }, `The list stops at the top ${MAX_SHOWN} of ${r.total}. Narrow the query to see the rest.`);
      }
    }
    mount(this.slots.listHead, head);
    mount(this.slots.list, [renderResultList(hits, { selectedId: this.s.id, brain: this.s.brainHint, emptyText: empty }), more]);
  }

  paintRecord() {
    if (!this.slots) return;
    const s = this.s;
    mount(this.slots.record, s.hit
      ? renderRecord(s.hit, { related: s.related, brain: s.brainHint })
      : renderRecord(null, { emptyText: s.loading ? 'Loading…' : (s.recordError || undefined) }));
  }

  paintGraph() {
    if (!this.slots) return;
    const s = this.s;
    mount(this.slots.graph, renderGraphPanel(s.hit
      ? { brain: s.brain, brains: s.brains, ...s.graph, index: s.hit._index, guest: this.guest }
      : { status: 'idle' }));
  }

  // ----- events --------------------------------------------------------

  handleKey(e) {
    const t = e.target;
    if (e.key !== 'Enter' || !t || !t.classList || !t.classList.contains('rd-q')) return;
    e.preventDefault();
    this.s.q = String(t.value || '');
    this.s.field = null;       // a typed search runs over the Reader's own fields
    this.s.size = PAGE_SIZE;
    this.runSearch();
  }

  handleClick(e) {
    const t = e.target;
    const more = t && t.closest ? t.closest('[data-rd-more]') : null;
    if (!more) return;
    e.preventDefault();
    this.s.size = Math.min(MAX_SHOWN, this.s.size + PAGE_SIZE);
    this.runSearch();
  }

  handleChange(e) {
    const t = e.target;
    if (!t || !t.classList) return;
    if (t.classList.contains('rd-type')) {
      if (QUERY_TYPES.includes(t.value)) {
        // Run what is IN THE BOX, not the last submitted query: a person who
        // typed a new query and then picked a type saw results for the old one
        // under the new text (PR #945 review).
        const box = this.root && this.root.querySelector('.rd-q');
        if (box) { this.s.q = String(box.value || ''); box.setAttribute('placeholder', queryPlaceholder(t.value)); }
        this.s.type = t.value;
        this.s.field = null;
        this.s.size = PAGE_SIZE;
        this.runSearch();
      }
    } else if (t.classList.contains('rd-index')) {
      const win = this.root && this.root.ownerDocument.defaultView;
      if (win && this.indicesFn().includes(t.value)) win.location.hash = readerHref({ index: t.value, brain: this.s.brainHint });
    }
  }

  /** Run a query chosen elsewhere (a corpus card's sample query). `field` is
   *  the field the sample was written for; it is searched too. */
  setQuery({ q, type, field }) {
    this.s.q = String(q || '');
    if (QUERY_TYPES.includes(type)) this.s.type = type;
    this.s.field = typeof field === 'string' && field ? field : null;
    this.s.size = PAGE_SIZE;
    this.s.result = null;
  }
}
