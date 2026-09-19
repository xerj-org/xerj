// ============================================================
// Xerj Console — SECOND BRAIN data layer + interaction controller
//
// This dashboard reads the LIVE graph endpoints ONLY:
//
//   GET /_cat/indices/.xerj-memory-*-edges   → discover brains
//   GET /_graph/{brain}/overview             → §4.4 of the contract
//   GET /_graph/{brain}/ego                  → §4.3 of the contract
//
// There is deliberately NO mock fallback: a fake brain would defeat
// the product ("what does my agent believe?" answered with invented
// beliefs). Every failure path returns a SHAPED object — never null —
// so data/query.js can never silently substitute mock data; the
// dashboard renders an honest unreachable / empty state instead.
//
// The second half of this file is the interaction controller. The app
// shell re-renders pages as whole HTML strings, so ledger interactions
// (refocus, belief-time scrub, evidence reveal) are handled here with
// document-level delegated listeners + in-place re-render of only the
// second-brain panel bodies (`[data-sb-body]`), each drawn by
// ux/ego-ledger.js#renderPanelBody. Scrub previews are client-side
// over already-fetched edges; RELEASING the caret re-queries the
// server with `as_of` — the server owns the bi-temporal cut.
//
// Two additions ride along:
//   READ LOG — every fetch this page makes records what/how much/why
//   (the reads panel is the only honest "which data was read and why";
//   the engine does not record other callers' intentions).
//   MAP SEAM — ux/brain-map.js is dynamic-imported and mounted into
//   the map panel's `[data-sb-map-mount]`; it talks back through
//   sbRefocus/sbLogRead/sbPublishMapStats/sbSnapshot and hears the
//   shared belief caret as sb:asof-preview / sb:asof-commit events.
// ============================================================

import {
  renderPanelBody, drawLedgerTraces, edgeStateAt, fmtTs, caretAlignClass,
  EvidencePreview, FindResults, authoredSplit, splitLabel,
} from '../ux/ego-ledger.js';

/** Panel ids of the second-brain dashboard, in render order. The
 *  controller patches exactly these after an interaction — except a
 *  LIVE map body, which is the one panel that owns its own DOM (a
 *  canvas is not re-renderable from an HTML string; see patchPanels). */
export const SB_PANEL_IDS = [
  'controls', 'map', 'scrub',
  'edgesLive', 'edgesTotal', 'invalidated', 'detectors',
  'typeDist', 'edgeTimeline', 'notes', 'crossings', 'reads',
  'ego', 'hubs', 'notShown',
];

/** Returned edge budget for the belief query. Hops=2 so neighbour rows
 *  can carry their onward-degree count; 1000 is the ego endpoint's
 *  clamp ceiling. Clips are surfaced by the honesty panel + '≥' counts. */
const EGO_LIMIT = 1000;

const K = {
  brain: 'xerj.sb.brain',
  perBrain: (b) => `xerj.sb.state.${b}`,
};

// ----- module state -------------------------------------------------
//
// A singleton, on purpose: the console is a single-page app and the
// ledger must survive the shell's whole-page re-renders (theme flips,
// time-range clicks) without losing focus/as-of. Persisted so a reload
// reopens the same view.

const S = {
  baseUrl: '',
  connected: false,
  error: null,     // transport-level error (engine unreachable)
  uiError: null,   // last interaction-level error (shown inline, non-fatal)
  overviewError: null, // overview read failed on a REAL brain — an error state, never "empty"
  brains: [],
  brain: null,
  lastHashApplied: null, // last deep-link query tail already applied
  focus: null,
  asOf: null,      // epoch-ms | null = NOW
  trail: [],
  overview: null,  // §4.4 body (or exists:false body)
  ego: null,       // §4.3 body — the belief at the caret
  timeline: null,  // §4.3 body — hop-1, include_expired, pinned at NOW
  names: {},       // node id → { title } — display names for hub ids / crumbs
  // Pinned paper trails (edge ids). Living here — NOT in the DOM — is
  // the point: every patchPanels()/scrub commit re-renders rows from
  // this set, so a pinned quote survives any re-render. Cleared on
  // refocus (a new ledger is a new reading), never on an as-of commit.
  openEvidence: new Set(),
  collapsed: new Set(), // folded link-kind groups ("side:type"), per-brain
  recentRetired: null,  // three most recently retired links (RetiredPanel)
  lastShift: null,      // what the last time-travel changed (scrubber readout)
  // ---- statistics row state ----------------------------------------
  nodeStats: null,      // { total, formats:[{format,count}], otherFormats } | { error }
  crossings: null,      // { total, crossCount, pairs:[{src,dst,count}], otherPairs } | { error }
  serverReads: null,    // { rows:[{index,count}] } | { error } — /v1/metrics counters
  reads: [],            // the page's own attributed fetch log (capped)
  readsTotal: 0,        // reads ever logged this session (survives the cap)
  mapStats: null,       // published by ux/brain-map.js via sbPublishMapStats
};

// ----- the page's own read log --------------------------------------
//
// Every fetch this dashboard makes records WHAT it read, HOW MUCH, and
// WHY — the only honest answer to "which data was read and why". The
// engine records neither other callers' whys nor per-note read counts;
// the reads panel says so instead of inventing them.

const READS_CAP = 48;

function logRead(surface, detail, reason) {
  const last = S.reads[S.reads.length - 1];
  S.readsTotal += 1;
  if (last && last.surface === surface && last.detail === detail && last.reason === reason) {
    // The same read repeating back-to-back (render loops, re-probes)
    // collapses into one row with a ×count — the log stays a record,
    // not a scroll of noise.
    last.n += 1;
    last.t = Date.now();
  } else {
    S.reads.push({ t: Date.now(), surface, detail, reason, n: 1 });
    if (S.reads.length > READS_CAP) S.reads = S.reads.slice(-READS_CAP);
  }
  scheduleReadsPatch();
}

/** The map module (and any other in-page reader) logs its reads
 *  through the same front door: sbLogRead('links index', '10 pages',
 *  'building the map'). */
export const sbLogRead = logRead;

let readsRaf = 0;
/** Repaint just the reads panel after a log entry — logs land mid-
 *  flight (FIND keystrokes, map pages) when no full patch will run. */
function scheduleReadsPatch() {
  if (typeof requestAnimationFrame === 'undefined' || readsRaf) return;
  readsRaf = requestAnimationFrame(() => {
    readsRaf = 0;
    const el = document.querySelector('[data-sb-body="reads"]');
    if (el) el.innerHTML = renderPanelBody('reads', assemble());
  });
}

/**
 * The deep-link query tail (`#/second-brain?brain=X&focus=Y&as_of=Z`),
 * parsed: a URL that is a MOMENT in a brain's beliefs, not just a
 * brain. The router (app.js parseRoute) ignores everything after `?`.
 * `raw` is the verbatim tail so "already applied" is one comparison.
 */
function hashMoment() {
  const h = location.hash || '';
  const q = h.indexOf('?');
  if (q < 0) return null;
  const raw = h.slice(q + 1);
  const p = new URLSearchParams(raw);
  const brain = (p.get('brain') || '').trim() || null;
  const focus = (p.get('focus') || '').trim() || null;
  const asOfN = Number(p.get('as_of'));
  const asOf = Number.isFinite(asOfN) && asOfN > 0 ? asOfN : null;
  if (!brain && !focus && asOf == null) return null;
  return { raw, brain, focus, asOf };
}

/**
 * Write the current moment back into the hash (replaceState — no
 * history spam) so the address bar is always a shareable artifact:
 * copy it, and the receiver opens the same brain, the same note, the
 * same point in belief time. Only touches the hash while ON the
 * second-brain route.
 */
function writeMoment() {
  if (typeof history === 'undefined' || typeof location === 'undefined') return;
  if (!S.brain) return;
  const h = location.hash || '';
  if (!h.startsWith('#/second-brain')) return;
  const qs = new URLSearchParams({ brain: S.brain });
  if (S.focus) qs.set('focus', S.focus);
  if (S.asOf != null) qs.set('as_of', String(S.asOf));
  const raw = qs.toString();
  S.lastHashApplied = raw; // our own write is by definition applied
  try { history.replaceState(null, '', `#/second-brain?${raw}`); } catch { /* sandboxed */ }
}

function loadBrainState() {
  try {
    const raw = sessionStorage.getItem(K.perBrain(S.brain));
    const st = raw ? JSON.parse(raw) : {};
    S.focus = st.focus || null;
    S.asOf = Number.isFinite(st.asOf) ? st.asOf : null;
    S.trail = Array.isArray(st.trail) ? st.trail : [];
    S.collapsed = new Set(Array.isArray(st.collapsed) ? st.collapsed : []);
  } catch {
    S.focus = null; S.asOf = null; S.trail = []; S.collapsed = new Set();
  }
}

function saveBrainState() {
  try {
    if (!S.brain) return;
    sessionStorage.setItem(
      K.perBrain(S.brain),
      JSON.stringify({
        focus: S.focus, asOf: S.asOf, trail: S.trail.slice(-8),
        collapsed: [...S.collapsed].slice(0, 64),
      }),
    );
  } catch { /* storage full/blocked — view still works, just not sticky */ }
}

// ----- HTTP ---------------------------------------------------------

async function getJson(url, signal) {
  const r = await fetch(url, { signal, headers: { accept: 'application/json' } });
  const text = await r.text();
  let body = null;
  try { body = text ? JSON.parse(text) : null; } catch { /* non-JSON error page */ }
  if (!r.ok) {
    // The graph API 404s an unknown brain WITH a shaped body
    // (`exists: false`) — that is a valid answer, not an error.
    if (body && body.exists === false) return body;
    const reason = body && body.error && body.error.reason
      ? body.error.reason
      : `HTTP ${r.status}`;
    throw new Error(reason);
  }
  return body;
}

/**
 * Brains = the reserved `.xerj-memory-{brain}-edges` indices. `_cat`
 * on this engine emits plain text (no format=json), so parse lines;
 * tolerate a JSON array in case a later build adds it. A wildcard
 * matching nothing is an empty 200 — an engine with no brains is a
 * normal state, not an error.
 *
 * The fetch pattern is `.xerj-memory-*` (prefix only): live-verified
 * 2026-07-30 that this engine's index-pattern matcher supports a
 * single leading or trailing `*` but NOT an infix wildcard —
 * `.xerj-memory-*-edges` returns an empty 200 even when the index
 * exists. The `-edges` cut happens in the client filter below either
 * way, so the result set is identical.
 */
async function discoverBrains(baseUrl, signal) {
  const r = await fetch(`${baseUrl}/_cat/indices/.xerj-memory-*`, { signal });
  if (r.status === 404) return [];
  if (!r.ok) throw new Error(`_cat/indices HTTP ${r.status}`);
  const text = await r.text();
  let names = [];
  const trimmed = text.trim();
  if (trimmed.startsWith('[')) {
    try { names = JSON.parse(trimmed).map((i) => i.index).filter(Boolean); } catch { names = []; }
  } else if (trimmed) {
    // cat line: `health status NAME uuid pri rep docs deleted size size`
    names = trimmed.split('\n')
      .map((l) => l.trim().split(/\s+/)[2])
      .filter(Boolean);
  }
  return names
    .filter((n) => n.startsWith('.xerj-memory-') && n.endsWith('-edges'))
    .map((n) => n.slice('.xerj-memory-'.length, -'-edges'.length))
    .filter((b) => b.length > 0)
    .sort();
}

const enc = encodeURIComponent;

/**
 * Hub lists and focus crumbs come back from the overview as raw node
 * ids — for autoindex brains those are opaque hashes no person should
 * have to read. One bounded ids-query against the brain's nodes index
 * turns them into titles. Best-effort and honest: any id that does not
 * resolve keeps rendering as its (shortened) id, never as a guess.
 */
async function hydrateNames(signal) {
  const o = S.overview;
  if (!o || o.exists === false || !o.nodes_index) return;
  const ids = new Set();
  for (const h of (o.hubs && o.hubs.in) || []) ids.add(h.id);
  for (const h of (o.hubs && o.hubs.out) || []) ids.add(h.id);
  for (const t of S.trail) ids.add(t);
  if (S.focus) ids.add(S.focus);
  for (const e of S.recentRetired || []) { ids.add(e.src); ids.add(e.dst); }
  const want = [...ids].filter((id) => !(id in S.names)).slice(0, 40);
  if (!want.length) return;
  try {
    logRead('notes index', `${want.length} note name${want.length === 1 ? '' : 's'}`, 'turning ids into titles');
    const r = await fetch(`${S.baseUrl}/${enc(o.nodes_index)}/_search`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        query: { ids: { values: want } },
        size: want.length,
        // ax_path/path: the real file behind a note — a truthful label
        // for the focus card and the map, never derived from the id.
        _source: ['title', 'text', 'body', 'ax_path', 'path'],
      }),
      signal,
    });
    if (!r.ok) return; // cosmetic enrichment only — ids still render
    const j = await r.json();
    for (const h of (j.hits && j.hits.hits) || []) {
      const src = h._source || {};
      const title = typeof src.title === 'string' && src.title.trim()
        ? src.title.trim()
        : (typeof src.text === 'string' && src.text.trim()
          ? src.text.trim().slice(0, 60)
          : (typeof src.body === 'string' && src.body.trim() ? src.body.trim().slice(0, 60) : null));
      const path = (typeof src.ax_path === 'string' && src.ax_path.trim())
        ? src.ax_path.trim()
        : (typeof src.path === 'string' && src.path.trim() ? src.path.trim() : null);
      if (title || path) S.names[h._id] = { title, path };
    }
  } catch { /* names are sugar; the ids remain the truth */ }
}

function fetchOverview(signal) {
  const qs = new URLSearchParams({ top: '10', histogram_interval: 'day' });
  if (S.asOf != null) qs.set('as_of', String(S.asOf));
  logRead('link counts', 'one overview read', 'the tiles on this page');
  return getJson(`${S.baseUrl}/_graph/${enc(S.brain)}/overview?${qs}`, signal);
}

/** The focus note's display name for the read log — hydrated title if
 *  the console knows one, else the (shortened) id. Never a guess. */
function focusLabel() {
  const n = S.focus && S.names[S.focus] && S.names[S.focus].title;
  const raw = n || String(S.focus || '');
  return raw.length > 28 ? `${raw.slice(0, 27)}…` : raw;
}

/**
 * The RETIRED panel's churn rows: the three most recently retired
 * links, one bounded ordinary search on the edges index. Deliberately
 * truth-now (not as-of-aware): "most recently retired" is a statement
 * about the record, not about the caret. Best-effort — on any failure
 * the rows simply don't render; the count above them is the truth
 * either way.
 */
async function fetchRecentRetired(signal) {
  const inval = S.overview && S.overview.exists !== false
    && S.overview.edges && S.overview.edges.invalidated > 0;
  if (!inval) { S.recentRetired = []; return; }
  try {
    logRead('links index', 'three newest retirements', 'the retired tile');
    const r = await fetch(`${S.baseUrl}/${enc(`.xerj-memory-${S.brain}-edges`)}/_search`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        query: { exists: { field: 'invalid_at' } },
        sort: [{ invalid_at: { order: 'desc' } }],
        size: 3,
        _source: ['src', 'dst', 'type', 'detector', 'evidence', 'valid_at', 'invalid_at'],
      }),
      signal,
    });
    if (!r.ok) { S.recentRetired = null; return; }
    const j = await r.json();
    S.recentRetired = (((j.hits && j.hits.hits) || []))
      .map((h) => ({ edge_id: h._id, ...(h._source || {}) }));
  } catch { S.recentRetired = null; }
}

/**
 * The belief at the caret. `include_expired=true` so an invalidated
 * fact comes back struck-through + RETIRED instead of silently gone;
 * hops=2 feeds the neighbour onward-counts; include_nodes hydrates
 * titles/previews for the focus card and rows.
 */
function fetchBelief(signal) {
  const qs = new URLSearchParams({
    node: S.focus, hops: '2', direction: 'both',
    limit: String(EGO_LIMIT),
    include_expired: 'true', include_nodes: 'true', include_evidence: 'true',
  });
  if (S.asOf != null) qs.set('as_of', String(S.asOf));
  logRead('the ledger', `one 2-hop walk from “${focusLabel()}”`, 'you focused this note');
  return getJson(`${S.baseUrl}/_graph/${enc(S.brain)}/ego?${qs}`, signal);
}

/**
 * The scrubber's interval set: hop-1, include_expired, pinned at NOW.
 * Pinned so scrubbing left never removes rows from the strip — only
 * their visibility state changes. Refetched on focus change only.
 */
function fetchTimeline(signal) {
  const qs = new URLSearchParams({
    node: S.focus, hops: '1', direction: 'both',
    limit: String(EGO_LIMIT),
    include_expired: 'true', include_evidence: 'false',
  });
  logRead('belief strip', `one 1-hop walk from “${focusLabel()}”`, 'drawing every link’s lifetime');
  return getJson(`${S.baseUrl}/_graph/${enc(S.brain)}/ego?${qs}`, signal);
}

// ----- statistics-row fetches (all best-effort, never throw) --------

/**
 * Notes total + by-file-type tally: one size-0 search over the brain's
 * nodes indices. The total is the search's own hit count — a ran
 * number; the panel prefers it and falls back to overview.nodes.total
 * only when this tally failed (see NotesPanel's rationale).
 */
async function fetchNodeStats(signal) {
  const o = S.overview;
  if (!o || o.exists === false || !o.nodes_index) { S.nodeStats = null; return; }
  try {
    logRead('notes index', 'one tally by file type', 'the notes tile');
    const r = await fetch(`${S.baseUrl}/${enc(o.nodes_index)}/_search`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        size: 0,
        // Exact, not the engine's 10,000 'gte' floor: the tile's
        // headline is the note count, and this brain's own per-type
        // rows summed to 2.1M under a "10,000 NOTES" headline before
        // this flag (live-caught on `repo`, 2026-07-30).
        track_total_hits: true,
        aggs: { formats: { terms: { field: 'ax_format', size: 12 } } },
      }),
      signal,
    });
    if (!r.ok) { S.nodeStats = { error: `HTTP ${r.status}` }; return; }
    const j = await r.json();
    const t = j.hits && j.hits.total;
    let total = t == null ? null : (typeof t === 'object' ? t.value : t);
    // A 'gte' floor is not a count — refuse it rather than print it.
    if (t && typeof t === 'object' && t.relation === 'gte') total = null;
    const agg = (j.aggregations && j.aggregations.formats) || {};
    S.nodeStats = {
      total: Number.isFinite(total) ? total : null,
      formats: (agg.buckets || []).map((b) => ({ format: b.key, count: b.doc_count })),
      otherFormats: agg.sum_other_doc_count || 0,
    };
  } catch (e) {
    if (e && e.name === 'AbortError') return;
    S.nodeStats = { error: String((e && e.message) || e) };
  }
}

/**
 * File-type crossings: which file types link to which, from the
 * src_format/dst_format stamps on the links index. Brains indexed
 * before stamping return zero stamped links — the panel then says
 * REINDEX TO SEE FILE-TYPE CROSSINGS instead of guessing from ids.
 */
async function fetchCrossings(signal) {
  if (!S.brain) { S.crossings = null; return; }
  try {
    logRead('links index', 'one file-type crossing tally', 'the crossings tile');
    const r = await fetch(`${S.baseUrl}/${enc(`.xerj-memory-${S.brain}-edges`)}/_search`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        size: 0,
        // Exact, not the engine's 10,000 'gte' floor — the tile prints
        // this as "N stamped links tallied" and a floor there is a
        // false number (live-caught at 20,930 links, 2026-07-30).
        track_total_hits: true,
        query: { exists: { field: 'src_format' } },
        aggs: {
          src: {
            terms: { field: 'src_format', size: 20 },
            aggs: { dst: { terms: { field: 'dst_format', size: 20 } } },
          },
        },
      }),
      signal,
    });
    if (!r.ok) { S.crossings = { error: `HTTP ${r.status}` }; return; }
    const j = await r.json();
    const t = j.hits && j.hits.total;
    const total = (t == null ? 0 : (typeof t === 'object' ? t.value : t)) || 0;
    const srcAgg = (j.aggregations && j.aggregations.src) || {};
    const pairs = [];
    let otherPairs = 0;
    for (const sb of srcAgg.buckets || []) {
      const dstAgg = sb.dst || {};
      for (const db of dstAgg.buckets || []) {
        pairs.push({ src: sb.key, dst: db.key, count: db.doc_count });
      }
      if (dstAgg.sum_other_doc_count > 0) otherPairs += 1;
    }
    if (srcAgg.sum_other_doc_count > 0) otherPairs += 1;
    // Deterministic order: count desc, then label asc.
    pairs.sort((a, b) => b.count - a.count
      || (a.src < b.src ? -1 : a.src > b.src ? 1 : 0)
      || (a.dst < b.dst ? -1 : a.dst > b.dst ? 1 : 0));
    const crossCount = pairs.reduce((n, p) => n + (p.src !== p.dst ? p.count : 0), 0);
    const sameCount = pairs.reduce((n, p) => n + (p.src === p.dst ? p.count : 0), 0);
    // Bucket caps can hide pairs; when they did, every derived count
    // is a floor and the tile must say so.
    const truncated = otherPairs > 0;
    S.crossings = { total, crossCount, sameCount, truncated, pairs, otherPairs };
  } catch (e) {
    if (e && e.name === 'AbortError') return;
    S.crossings = { error: String((e && e.message) || e) };
  }
}

/**
 * Per-index search counters from the server's own metrics — framed
 * exactly as what they are: since server start, reset at boot. If the
 * metrics endpoint refuses the console's credentials, the panel shows
 * the refusal rather than a stale or invented number.
 */
async function fetchServerReads(signal) {
  try {
    logRead('server counters', 'one metrics read', 'searches per index');
    const r = await fetch(`${S.baseUrl}/v1/metrics`, { signal });
    if (!r.ok) { S.serverReads = { error: `HTTP ${r.status}` }; return; }
    const text = await r.text();
    const rows = [];
    for (const line of text.split('\n')) {
      const m = line.match(/^xerj_queries_by_index_total\{index="([^"]*)"\}\s+(\d+)/);
      if (m) rows.push({ index: m[1], count: Number(m[2]) });
    }
    rows.sort((a, b) => b.count - a.count || (a.index < b.index ? -1 : 1));
    S.serverReads = { rows };
  } catch (e) {
    if (e && e.name === 'AbortError') return;
    S.serverReads = { error: String((e && e.message) || e) };
  }
}

// ----- data assembly (§7.3 shape, extras under `sb`) ---------------

function assemble() {
  const o = S.overview && S.overview.exists !== false ? S.overview : null;
  const ed = (o && o.edges) || { total: 0, live: 0, invalidated: 0 };
  const num = (v) => new Intl.NumberFormat('en').format(v || 0);
  const detectors = (o && o.detectors) || [];
  const detTail = o && o.not_shown && o.not_shown.detectors_not_listed > 0 ? '+' : '';
  // Hints in user words, and as-of-aware: the same tile must say
  // WHEN it is true once the belief caret leaves NOW.
  const atHint = S.asOf != null ? `at ${fmtTs(S.asOf)} UTC` : 'right now';
  // The detectors hint carries the authored/structural split — the
  // honesty device that stops folder position from counting at par
  // with written citations.
  const detHint = detectors.length
    ? splitLabel(authoredSplit(detectors))
    : 'deterministic, versioned';
  return {
    brain: S.brain,
    overview: S.overview,
    ego: S.ego,
    metrics: {
      edgesLive: { formatted: num(ed.live), hint: `held true ${atHint}` },
      edgesTotal: { formatted: num(ed.total), hint: 'nothing is ever deleted' },
      invalidated: { formatted: num(ed.invalidated), hint: 'drag belief time left to revisit' },
      detectors: { formatted: num(detectors.length) + detTail, hint: detHint },
    },
    series: {
      created: ((o && o.created_over_time) || []).map((b) => b.count),
      createdT: ((o && o.created_over_time) || []).map((b) => b.t),
    },
    sb: {
      connected: S.connected,
      error: S.error,
      uiError: S.uiError,
      overviewError: S.overviewError,
      brains: S.brains.slice(),
      focus: S.focus,
      asOf: S.asOf,
      trail: S.trail.slice(),
      timeline: S.timeline,
      names: S.names,
      openEvidence: S.openEvidence,
      collapsed: S.collapsed,
      recentRetired: S.recentRetired,
      lastShift: S.lastShift,
      nodeStats: S.nodeStats,
      crossings: S.crossings,
      serverReads: S.serverReads,
      reads: S.reads,
      readsTotal: S.readsTotal,
      mapStats: S.mapStats,
    },
  };
}

/** Default focus: the strongest hub. Deterministic (out over in, then
 *  the agg's own count-desc order). */
function defaultFocus() {
  const o = S.overview;
  if (!o || o.exists === false || !o.hubs) return null;
  const out = o.hubs.out && o.hubs.out[0];
  const inn = o.hubs.in && o.hubs.in[0];
  return (out && out.id) || (inn && inn.id) || null;
}

// ----- backend adapter entry (data/backends/xerj.js dispatches here) —

export async function liveSecondBrain(baseUrl, _ctx, signal) {
  S.baseUrl = (baseUrl || '').replace(/\/+$/, '');
  S.uiError = null;
  try {
    logRead('brain list', 'one index listing', 'finding your brains');
    S.brains = await discoverBrains(S.baseUrl, signal);
    S.connected = true;
    S.error = null;
  } catch (e) {
    S.connected = false;
    S.error = String((e && e.message) || e);
    S.brains = []; S.brain = null; S.overview = null; S.ego = null; S.timeline = null;
    const data = assemble();
    // Surface transport failure on the nav status pill too (query.js
    // flags `data.error` as live-error — visibly NOT "live · ok"). A 401/403
    // is a refusal (an auth-enabled engine, a console session — #936), not an
    // outage; the pill says which.
    data.error = /HTTP 40[13]\b/.test(S.error) ? `graph API refused this console session: ${S.error}` : `graph API unreachable: ${S.error}`;
    return data;
  }

  let persisted = null;
  try { persisted = localStorage.getItem(K.brain); } catch { /* private mode */ }
  // Deep link: `#/second-brain?brain=X&focus=Y&as_of=Z` — the URL
  // `xerj brain` prints, and the shareable-moment URLs this page writes
  // back on every refocus / time-travel. Applied when it CHANGES
  // (pasting a new link jumps to that moment) but never re-asserted on
  // every render, so in-app navigation still wins for the session.
  const fromHash = hashMoment();
  let hashFocus = null;
  let hashAsOf = null;
  if (fromHash && fromHash.raw !== S.lastHashApplied) {
    S.lastHashApplied = fromHash.raw;
    if (fromHash.brain && S.brains.includes(fromHash.brain)) {
      persisted = fromHash.brain;
      hashFocus = fromHash.focus;
      hashAsOf = fromHash.asOf;
      try { localStorage.setItem(K.brain, fromHash.brain); } catch { /* private mode */ }
    } else if (fromHash.brain) {
      S.uiError = `brain "${fromHash.brain}" not found on this engine`;
    }
  }
  S.brain = persisted && S.brains.includes(persisted) ? persisted : (S.brains[0] || null);
  if (!S.brain) {
    S.overview = null; S.ego = null; S.timeline = null;
    return assemble();
  }
  loadBrainState();
  // The pasted moment outranks the persisted one — that is what makes
  // the URL an artifact: the receiver sees the sender's view.
  if (hashFocus) S.focus = hashFocus;
  if (hashAsOf != null) S.asOf = hashAsOf;

  try {
    S.overview = await fetchOverview(signal);
    S.overviewError = null;
  } catch (e) {
    // A failed read on a real brain is an ERROR, not an empty brain:
    // the renderer must show "could not read", never zeros or the
    // fill-me teaching copy.
    S.overview = null; S.ego = null; S.timeline = null;
    S.overviewError = String((e && e.message) || e);
    const data = assemble();
    data.error = `overview failed: ${S.overviewError}`;
    return data;
  }

  const hasEdges = S.overview && S.overview.exists !== false
    && S.overview.edges && S.overview.edges.total > 0;
  if (!hasEdges) {
    S.ego = null; S.timeline = null;
    // A brain can hold notes before it holds links — count them anyway.
    await Promise.all([fetchNodeStats(signal), fetchServerReads(signal)]);
    return assemble();
  }

  if (!S.focus) S.focus = defaultFocus();
  const statJobs = [ // best-effort, never throw
    fetchNodeStats(signal), fetchCrossings(signal), fetchServerReads(signal),
  ];
  if (S.focus) {
    try {
      const [belief, timeline] = await Promise.all([
        fetchBelief(signal), fetchTimeline(signal),
        fetchRecentRetired(signal), // best-effort, never throws
        ...statJobs,
      ]);
      S.ego = belief;
      S.timeline = timeline;
    } catch (e) {
      S.ego = null; S.timeline = null;
      S.uiError = String((e && e.message) || e);
    }
  } else {
    S.ego = null; S.timeline = null;
    await Promise.all([fetchRecentRetired(signal), ...statJobs]);
  }
  await hydrateNames(signal);
  saveBrainState();
  return assemble();
}

// ====================================================================
// Interaction controller
// ====================================================================

let inflight = null; // AbortController of the interaction in flight

function abortInflight() {
  if (inflight) { inflight.abort(); inflight = null; }
}

/** Re-render every second-brain panel body currently in the DOM from
 *  module state, then redraw the traces post-paint. Pinned paper
 *  trails re-render open (S.openEvidence is module state, not DOM
 *  state) — this is what makes a pin survive scrub commits. */
function patchPanels() {
  if (typeof document === 'undefined') return;
  hidePreview();
  // innerHTML replacement destroys the focused element; without this,
  // arrow-key time travel dies after its first commit (the second
  // keypress lands on a detached caret). Remember what held keyboard
  // focus and restore it on the freshly-rendered twin.
  const focused = document.activeElement;
  const refocusSel = focused && focused.matches
    ? (focused.matches('[data-sb-caret]') ? '[data-sb-caret]'
      : (focused.matches('[data-sb-find]') ? '[data-sb-find]' : null))
    : null;
  const data = assemble();
  for (const id of SB_PANEL_IDS) {
    if (id === 'map') {
      // A LIVE map owns its DOM — a mounted canvas cannot be re-created
      // from an HTML string. Skip the innerHTML swap and let the map
      // module restyle itself from state (syncBrainMap below).
      const mapEl = document.querySelector('[data-sb-map]');
      if (mapEl && mapEl.hasAttribute('data-sb-map-live')) continue;
    }
    const el = document.querySelector(`[data-sb-body="${id}"]`);
    if (el) el.innerHTML = renderPanelBody(id, data);
  }
  if (refocusSel) {
    const again = document.querySelector(refocusSel);
    if (again && again.focus) again.focus();
  }
  showUiError();
  scheduleTraces();
  flashFlipped();
  ensureMap();
  syncMap();
}

/** One-shot outline flash on every ledger row whose belief state the
 *  last time-travel flipped. Runs once per commit, right after the
 *  post-commit re-render; the class removes itself after the fade. */
function flashFlipped() {
  const shift = S.lastShift;
  if (!shift || !shift.flashPending || !shift.flipped || !shift.flipped.length) return;
  shift.flashPending = false;
  for (const id of shift.flipped) {
    for (const row of document.querySelectorAll(`[data-sb-edge="${CSS.escape(id)}"]`)) {
      row.classList.add('sb-flip');
      setTimeout(() => row.classList.remove('sb-flip'), 700);
    }
  }
}

function showUiError() {
  const slot = document.querySelector('[data-sb-error]');
  if (!slot) return;
  slot.textContent = S.uiError ? `QUERY FAILED · ${String(S.uiError).slice(0, 90)}` : '';
}

function setBusy(on) {
  const el = document.querySelector('[data-sb-body="ego"]');
  if (el) el.classList.toggle('sb-busy', !!on);
}

let traceRaf = 0;
function scheduleTraces() {
  if (typeof requestAnimationFrame === 'undefined') return;
  if (traceRaf) cancelAnimationFrame(traceRaf);
  traceRaf = requestAnimationFrame(() => {
    traceRaf = 0;
    drawLedgerTraces(document);
  });
}

/**
 * Post-render hook. The dashboard's render() calls this every time the
 * shell rebuilds the page so the trace overlay (which needs real
 * layout) is drawn after paint. Safe under Node (no-op) — the render
 * functions themselves stay pure.
 */
export function sbAfterRender() {
  scheduleTraces();
  ensureMap();
}

// ----- THE MAP seam -------------------------------------------------
//
// The map lives in ux/brain-map.js (its own slice). This controller
// only (1) loads it, (2) hands it a mount point (`[data-sb-map-mount]`
// drawn by the map panel body), (3) keeps panel patching from eating
// its canvas, and (4) speaks to it through the narrow API below plus
// two document events: `sb:asof-preview` / `sb:asof-commit`
// (detail.ms = epoch-ms | null for NOW). Expected module exports:
//   mountBrainMap()  idempotent; finds the mount, draws, and sets
//                    `data-sb-map-live` on [data-sb-map]
//   syncBrainMap()   restyle from current state after a panel patch

let mapMod;            // undefined = not tried · null = not in this build
let mapModLoading = null;

/** The controller calls this from render() while the dashboard is
 *  still assembling HTML strings — the mount point may not be in the
 *  DOM yet. Without a post-paint retry the first (often only) data-
 *  carrying render never mounts the map: ensureMapNow bails on the
 *  missing mount, nothing re-arms it, and the panel shows its waiting
 *  placeholder forever (live-caught on the 19k-link `repo` brain,
 *  2026-07-30). Same rAF pattern mountBrainMap uses for its own
 *  pre-commit calls. */
let ensureMapRaf = 0;
function ensureMap() {
  if (typeof document === 'undefined') return;
  if (typeof requestAnimationFrame !== 'undefined' && !ensureMapRaf) {
    ensureMapRaf = requestAnimationFrame(() => { ensureMapRaf = 0; ensureMapNow(); });
  }
  ensureMapNow();
}

function ensureMapNow() {
  if (!document.querySelector('[data-sb-map-mount]')) return;
  if (mapMod === null) { markMapMissing(); return; }
  if (mapMod) { try { mapMod.mountBrainMap && mapMod.mountBrainMap(); } catch { /* map draws best-effort */ } return; }
  if (!mapModLoading) {
    mapModLoading = import('../ux/brain-map.js')
      .then((m) => { mapMod = m; ensureMap(); })
      .catch(() => { mapMod = null; markMapMissing(); });
  }
}

/** No ux/brain-map.js in this build: say so — never a spinner that
 *  promises a map that cannot arrive. */
function markMapMissing() {
  const el = document.querySelector('[data-sb-map-mount]');
  if (el && el.querySelector('[data-sb-map-waiting]')) {
    el.innerHTML = '<div class="panel-empty mono faint">THE MAP IS NOT IN THIS BUILD · THE LEDGER BELOW SHOWS EVERY LINK</div>';
  }
}

function syncMap() {
  if (mapMod && mapMod.syncBrainMap) {
    try { mapMod.syncBrainMap(); } catch { /* the map never takes the page down */ }
  }
}

/** Map → ledger routing: every terminal click on the map lands in the
 *  ledger (the leaf view), scrolled into sight. */
export function sbRefocus(id) {
  refocus(id);
  const ego = document.querySelector('[data-sb-body="ego"]');
  if (ego && ego.scrollIntoView) ego.scrollIntoView({ behavior: 'smooth', block: 'start' });
}

/**
 * The map publishes what it actually read and derived so the
 * statistics row can state it: { orphansFloor, orphansIsFloor,
 * edgesFetched, edgesTotal, … } — every field optional, every consumer
 * renders only what is present.
 */
export function sbPublishMapStats(stats) {
  S.mapStats = stats && typeof stats === 'object' ? stats : null;
  if (typeof document === 'undefined') return;
  const data = assemble();
  for (const id of ['notes', 'crossings']) {
    const el = document.querySelector(`[data-sb-body="${id}"]`);
    if (el) el.innerHTML = renderPanelBody(id, data);
  }
}

/** Read-only view state for the map module. */
export function sbSnapshot() {
  return {
    baseUrl: S.baseUrl, brain: S.brain, focus: S.focus, asOf: S.asOf,
    overview: S.overview, names: S.names,
  };
}

// ----- interaction ops ----------------------------------------------

async function refetch({ overview = false, belief = false, timeline = false, stats = false }) {
  abortInflight();
  inflight = typeof AbortController !== 'undefined' ? new AbortController() : null;
  const signal = inflight && inflight.signal;
  setBusy(true);
  S.uiError = null;
  try {
    const jobs = [];
    const ovJob = overview
      ? fetchOverview(signal).then((r) => { S.overview = r; S.overviewError = null; })
      : null;
    if (ovJob) jobs.push(ovJob, fetchRecentRetired(signal)); // retired rides along; never throws
    if (belief && S.focus) jobs.push(fetchBelief(signal).then((r) => { S.ego = r; }));
    if (timeline && S.focus) jobs.push(fetchTimeline(signal).then((r) => { S.timeline = r; }));
    if (stats) {
      // Truth-now tallies (notes, crossings, server counters):
      // refreshed on brain switch / retry, deliberately NOT on every
      // as-of commit — they are statements about the record, not about
      // the caret. They need the overview's index names, so they wait
      // for it when one is in flight (and stand down if it failed —
      // the error state owns the page then).
      const run = () => Promise.all([
        fetchNodeStats(signal), fetchCrossings(signal), fetchServerReads(signal),
      ]);
      jobs.push(ovJob ? ovJob.then(run, () => {}) : run());
    }
    await Promise.all(jobs);
    await hydrateNames(signal);
  } catch (e) {
    if (!(e && e.name === 'AbortError')) {
      S.uiError = String((e && e.message) || e);
      if (overview) S.overviewError = S.uiError;
    }
  } finally {
    setBusy(false);
  }
  saveBrainState();
  patchPanels();
}

function refocus(id) {
  if (!id || id === S.focus) return;
  if (S.focus) {
    S.trail = S.trail.filter((t) => t !== S.focus);
    S.trail.push(S.focus);
    S.trail = S.trail.slice(-8);
  }
  S.focus = id;
  S.ego = null; S.timeline = null;
  // A new ledger is a new reading: pinned paper trails and the last
  // time-travel readout belong to the note that had them.
  S.openEvidence = new Set();
  S.lastShift = null;
  // Announce the change for any in-page listener. (The map does NOT
  // subscribe today — it re-reads focus via sbSnapshot on the sync
  // that follows patchPanels; this event is a seam, not a claim.)
  if (typeof document !== 'undefined') {
    document.dispatchEvent(new CustomEvent('sb:refocus', { detail: { id } }));
  }
  writeMoment();
  patchPanels(); // immediate: focus card swaps to the new node id
  refetch({ belief: true, timeline: true });
}

function switchBrain(name) {
  if (!name || name === S.brain) return;
  try { localStorage.setItem(K.brain, name); } catch { /* private mode */ }
  S.brain = name;
  S.overview = null; S.ego = null; S.timeline = null;
  S.recentRetired = null; S.openEvidence = new Set(); S.lastShift = null;
  // Another brain is another record: its tallies and its map stats.
  S.nodeStats = null; S.crossings = null; S.mapStats = null;
  loadBrainState();
  writeMoment();
  patchPanels();
  (async () => {
    await refetch({ overview: true, stats: true });
    if (!S.focus) S.focus = defaultFocus();
    if (S.focus) await refetch({ belief: true, timeline: true });
  })();
}

/**
 * What did moving the caret change? Diffed over the pinned timeline
 * set (hop-1, include_expired, fetched at NOW), so scrubbing never has
 * to guess: appearances = believed-since inside the crossed interval,
 * retirements = believed-until inside it — counted separately, never
 * netted (a link that appeared AND retired inside the interval counts
 * in both — that is the honest reading). `flipped` lists the rows
 * whose visible state changed, for the one-shot flash.
 */
function computeShift(prevMs, nextMs) {
  const edges = (S.timeline && S.timeline.edges) || [];
  if (!edges.length) return null;
  const a = prevMs == null ? Date.now() : prevMs;
  const b = nextMs == null ? Date.now() : nextMs;
  if (a === b) return null;
  const t0 = Math.min(a, b);
  const t1 = Math.max(a, b);
  let appeared = 0;
  let retired = 0;
  const flipped = [];
  for (const e of edges) {
    const va = Number(e.valid_at);
    const vi = e.invalid_at != null ? Number(e.invalid_at) : null;
    if (va > t0 && va <= t1) appeared += 1;
    if (vi != null && vi > t0 && vi <= t1) retired += 1;
    if (edgeStateAt(e, prevMs) !== edgeStateAt(e, nextMs)) flipped.push(e.edge_id);
  }
  return { t0, t1, appeared, retired, flipped, flashPending: true };
}

/** Commit an as-of. `null` = NOW. Overview + belief re-query (both are
 *  as-of-dependent); the timeline stays pinned at NOW by design.
 *  Pinned paper trails are NOT cleared here — time travel must not
 *  eat an open quote. */
function commitAsOf(ms) {
  S.lastShift = computeShift(S.asOf, ms);
  S.asOf = ms;
  // One caret, whole page: the map restyles its live/retired states
  // from the same commit the ledger re-queries on.
  if (typeof document !== 'undefined') {
    document.dispatchEvent(new CustomEvent('sb:asof-commit', { detail: { ms } }));
  }
  writeMoment();
  refetch({ overview: true, belief: true });
}

// ----- belief-time scrub (preview client-side, commit server-side) ---

function stripDomain(scrub) {
  const d0 = Number(scrub.getAttribute('data-d0'));
  const d1 = Number(scrub.getAttribute('data-d1'));
  return Number.isFinite(d0) && Number.isFinite(d1) && d1 > d0 ? [d0, d1] : null;
}

function msFromClientX(strip, dom, clientX) {
  const rect = strip.getBoundingClientRect();
  const frac = Math.max(0, Math.min(1, (clientX - rect.left) / Math.max(1, rect.width)));
  return Math.round(dom[0] + frac * (dom[1] - dom[0]));
}

/** ≥99% of the way to the right edge counts as NOW (as_of unset). */
function normalizeAsOf(ms, dom) {
  return (dom[1] - ms) / (dom[1] - dom[0]) < 0.01 ? null : ms;
}

/** Belief count at `ms` from the strip's step attribute
 *  ("t:count,t:count,…", ascending): the last step at or before ms. */
function countFromSteps(stepsAttr, ms) {
  let count = 0;
  for (const part of stepsAttr.split(',')) {
    const i = part.indexOf(':');
    if (i < 1) continue;
    const t = Number(part.slice(0, i));
    if (t > ms) break;
    count = Number(part.slice(i + 1)) || 0;
  }
  return count;
}

/** Move the caret + relabel + restate the strip and every ledger row
 *  for a candidate as-of. Pure DOM, no queries — this is the mid-drag
 *  preview over edges the server already returned. The live counter in
 *  the legend follows the caret so the drag narrates itself. Handles
 *  both strip forms: lifetime rows restyle per-row; the belief-count
 *  curve reads its count off `data-sb-steps`. */
function previewAsOf(scrub, ms) {
  const dom = stripDomain(scrub);
  if (!dom) return;
  const pct = ((ms - dom[0]) / (dom[1] - dom[0])) * 100;
  const caret = scrub.querySelector('[data-sb-caret]');
  const label = scrub.querySelector('[data-sb-caret-label]');
  const atNow = normalizeAsOf(ms, dom) == null;
  if (caret) {
    caret.style.left = `${pct}%`;
    caret.setAttribute('aria-valuenow', String(ms));
    caret.setAttribute('aria-valuetext', atNow ? 'now' : `${fmtTs(ms)} UTC`);
  }
  if (label) {
    const lp = Math.max(1, Math.min(99, pct));
    label.style.left = `${lp}%`;
    label.textContent = atNow ? 'NOW' : fmtTs(ms);
    label.classList.toggle('sb-cl-left', caretAlignClass(lp) === 'sb-cl-left');
    label.classList.toggle('sb-cl-right', caretAlignClass(lp) === 'sb-cl-right');
  }
  const t = atNow ? null : ms;
  // Mid-drag, the map previews the same moment the ledger rows do.
  document.dispatchEvent(new CustomEvent('sb:asof-preview', { detail: { ms: t } }));
  const counter = scrub.querySelector('[data-sb-livecount]');
  const steps = scrub.getAttribute('data-sb-steps');
  if (steps) {
    // Curve form. The rendered counter may carry a '≥' floor prefix
    // (clipped fetch) — preserve it.
    if (counter) {
      const floor = counter.textContent.trimStart().startsWith('≥') ? '≥' : '';
      const total = (counter.textContent.split('OF')[1] || '').trim();
      const live = countFromSteps(steps, t == null ? Date.now() : t);
      counter.textContent = `${floor}${live.toLocaleString('en')} OF ${total}`;
    }
  } else {
    let live = 0;
    let total = 0;
    for (const seg of scrub.querySelectorAll('.sb-lseg')) {
      const e = { valid_at: Number(seg.getAttribute('data-va')), invalid_at: seg.hasAttribute('data-vi') ? Number(seg.getAttribute('data-vi')) : null };
      const state = edgeStateAt(e, t);
      total += 1;
      if (state === 'live') live += 1;
      seg.classList.remove('sb-l-live', 'sb-l-future', 'sb-l-expired');
      seg.classList.add(`sb-l-${state}`);
    }
    if (counter) {
      // Same floor rule as the curve branch: a clipped fetch renders
      // '≥' on both numbers — the preview must not quietly drop it.
      const floor = counter.textContent.trimStart().startsWith('≥') ? '≥' : '';
      counter.textContent = `${floor}${live} OF ${floor}${total}`;
    }
  }
  for (const row of document.querySelectorAll('[data-sb-edge][data-va]')) {
    const e = { valid_at: Number(row.getAttribute('data-va')), invalid_at: row.hasAttribute('data-vi') ? Number(row.getAttribute('data-vi')) : null };
    const state = edgeStateAt(e, t);
    row.classList.toggle('sb-off', state === 'future');
    row.classList.toggle('sb-expired', state === 'expired');
  }
}

// ----- event wiring (once per page load) ----------------------------

let wired = false;
let drag = null; // { scrub, dom, ms }

/**
 * Belief-time dragging starts ANYWHERE on the strip, not only on the
 * 13px caret — discoverability beats precision here. A press previews
 * immediately at the pointer; moving scrubs; releasing commits (a
 * plain click is just a zero-length drag). pointercancel reverts the
 * preview instead of committing a half-drag.
 */
function onPointerDown(e) {
  const strip = e.target.closest && e.target.closest('[data-sb-strip]');
  if (!strip) return;
  const scrub = strip.closest('[data-sb-scrub]');
  const dom = scrub && stripDomain(scrub);
  if (!dom) return;
  e.preventDefault();
  const ms = msFromClientX(strip, dom, e.clientX);
  drag = { scrub, dom, ms };
  previewAsOf(scrub, ms);
  const caret = scrub.querySelector('[data-sb-caret]');
  if (caret && caret.setPointerCapture && e.pointerId != null) {
    try { caret.setPointerCapture(e.pointerId); } catch { /* older engines */ }
  }
}

function onPointerMove(e) {
  if (!drag) return;
  const strip = drag.scrub.querySelector('[data-sb-strip]');
  if (!strip) { drag = null; return; }
  drag.ms = msFromClientX(strip, drag.dom, e.clientX);
  previewAsOf(drag.scrub, drag.ms);
}

/** The strip's belief events (assertions + retirements), ascending.
 *  Snap targets and arrow-key stops — moments where nothing changed
 *  are not interesting places to stand. */
function stripEvents(scrub) {
  const attr = scrub.getAttribute('data-sb-events');
  if (!attr) return [];
  return attr.split(',').map(Number).filter(Number.isFinite);
}

/** Within 1.5% of the strip span of a belief event, land ON the event
 *  (an assertion snaps to its believed-since, a retirement to its
 *  believed-until) — pixel-hunting the exact moment is the strip's
 *  job, not the user's. */
function snapToEvent(scrub, ms, dom) {
  const events = stripEvents(scrub);
  if (!events.length) return ms;
  let best = null;
  for (const t of events) {
    if (best == null || Math.abs(t - ms) < Math.abs(best - ms)) best = t;
  }
  return best != null && Math.abs(best - ms) <= (dom[1] - dom[0]) * 0.015 ? best : ms;
}

function onPointerUp() {
  if (!drag) return;
  const snapped = snapToEvent(drag.scrub, drag.ms, drag.dom);
  const ms = normalizeAsOf(snapped, drag.dom);
  drag = null;
  commitAsOf(ms);
}

/** A cancelled drag (scroll steal, palm touch) must not time-travel:
 *  put the preview back where the committed caret is. */
function onPointerCancel() {
  if (!drag) return;
  const { scrub, dom } = drag;
  drag = null;
  previewAsOf(scrub, S.asOf == null ? dom[1] : S.asOf);
}

function onClick(e) {
  const t = e.target;
  if (!t || !t.closest) return;

  const brainBtn = t.closest('[data-sb-brain]');
  if (brainBtn) { switchBrain(brainBtn.getAttribute('data-sb-brain')); return; }

  if (t.closest('[data-sb-now]')) { commitAsOf(null); return; }

  if (t.closest('[data-sb-retry]')) {
    refetch({ overview: true, belief: true, timeline: true, stats: true });
    return;
  }

  const copyBtn = t.closest('[data-sb-copy]');
  if (copyBtn) {
    const cmd = copyBtn.parentElement && copyBtn.parentElement.querySelector('[data-sb-cmd]');
    const text = cmd ? cmd.textContent.trim() : '';
    if (text && navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(() => {
        const was = copyBtn.textContent;
        copyBtn.textContent = 'COPIED';
        setTimeout(() => { copyBtn.textContent = was; }, 1200);
      }).catch(() => { /* clipboard blocked — the text is selectable */ });
    }
    return;
  }

  const focusBtn = t.closest('[data-sb-focus]');
  if (focusBtn) { refocus(focusBtn.getAttribute('data-sb-focus')); return; }

  const evBtn = t.closest('[data-sb-evidence]');
  if (evBtn) {
    // Pin / unpin the full paper trail. The pin lives in S.openEvidence
    // (module state), so it survives every re-render; the direct DOM
    // toggle here is just the immediate feedback.
    const id = evBtn.getAttribute('data-sb-evidence');
    const pin = !S.openEvidence.has(id);
    if (pin) S.openEvidence.add(id); else S.openEvidence.delete(id);
    const row = evBtn.closest('[data-sb-edge]');
    const body = row && row.querySelector('[data-sb-evbody]');
    if (body) body.hidden = !pin;
    evBtn.setAttribute('aria-expanded', String(pin));
    hidePreview();
    scheduleTraces(); // row heights changed under the rails
    return;
  }

  const foldBtn = t.closest('[data-sb-collapse]');
  if (foldBtn) {
    // Fold / unfold a link-kind group. Pure client-side: header + count
    // stay, honesty counters don't move, nothing is refetched. Direct
    // DOM toggle (not patchPanels) so an open FIND list isn't eaten.
    const key = foldBtn.getAttribute('data-sb-collapse');
    const fold = !S.collapsed.has(key);
    if (fold) S.collapsed.add(key); else S.collapsed.delete(key);
    const rows = foldBtn.closest('.sb-group');
    const body = rows && rows.querySelector('[data-sb-grouprows]');
    if (body) body.hidden = fold;
    const glyph = foldBtn.querySelector('[data-sb-ghcaret]');
    if (glyph) glyph.textContent = fold ? '▸' : '▾';
    foldBtn.setAttribute('aria-expanded', String(!fold));
    saveBrainState();
    scheduleTraces(); // group geometry changed under the rails
    return;
  }
  // Strip clicks are handled by the pointer pipeline above (a click is
  // a zero-length drag) — no second commit path here.
}

let keyCommit = 0;
function onKeyDown(e) {
  const caret = e.target && e.target.closest && e.target.closest('[data-sb-caret]');
  if (!caret) return;
  if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight' && e.key !== 'End') return;
  const scrub = caret.closest('[data-sb-scrub]');
  const dom = scrub && stripDomain(scrub);
  if (!dom) return;
  e.preventDefault();
  const cur = S.asOf == null ? dom[1] : S.asOf;
  let next;
  if (e.key === 'End') {
    next = dom[1]; // NOW
  } else {
    // Step between belief events — each keypress lands on the previous/
    // next moment something actually changed, not a blind 1% hop.
    const events = stripEvents(scrub);
    if (e.key === 'ArrowRight') {
      next = events.find((t) => t > cur);
      if (next == null) next = dom[1];
    } else {
      const prevs = events.filter((t) => t < cur);
      next = prevs.length ? prevs[prevs.length - 1] : dom[0];
    }
    next = Math.max(dom[0], Math.min(dom[1], next));
  }
  previewAsOf(scrub, next);
  // Debounce the server commit so holding the key doesn't queue queries.
  clearTimeout(keyCommit);
  keyCommit = setTimeout(() => commitAsOf(normalizeAsOf(next, dom)), 250);
}

function onResize() {
  scheduleTraces();
}

// ----- FIND — a note by the words in it ------------------------------
//
// One bounded query against the brain's nodes index (the same index
// hydrateNames reads), debounced 250ms, capped at 7 hits. Results are
// ordinary refocus links; an empty answer says what the brain is.

let findTimer = 0;
let findSeq = 0; // only the latest keystroke's response may render

async function runFind(q) {
  const slot = document.querySelector('[data-sb-findresults]');
  if (!slot) return;
  if (!q) { slot.hidden = true; slot.innerHTML = ''; return; }
  const o = S.overview;
  if (!o || o.exists === false || !o.nodes_index) return;
  const seq = ++findSeq;
  try {
    logRead('notes index', `one text search “${q.slice(0, 24)}${q.length > 24 ? '…' : ''}”`, 'you typed in FIND');
    const r = await fetch(`${S.baseUrl}/${enc(o.nodes_index)}/_search`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        query: {
          bool: {
            should: [
              { match: { title: q } },
              { match: { text: q } },
              { match: { body: q } },
            ],
          },
        },
        size: 7,
        _source: ['title'],
      }),
    });
    if (seq !== findSeq) return; // a newer keystroke owns the slot
    if (!r.ok) {
      slot.innerHTML = `<span class="sb-sidenote mono faint">FIND FAILED · HTTP ${r.status}</span>`;
      slot.hidden = false;
      return;
    }
    const j = await r.json();
    const hits = ((j.hits && j.hits.hits) || []).map((h) => ({
      id: h._id,
      title: (h._source && typeof h._source.title === 'string' && h._source.title.trim())
        ? h._source.title.trim() : null,
    }));
    slot.innerHTML = FindResults({ hits, names: S.names });
    slot.hidden = false;
  } catch (e) {
    if (seq !== findSeq) return;
    slot.innerHTML = `<span class="sb-sidenote mono faint">FIND FAILED · ${String((e && e.message) || e).slice(0, 60)}</span>`;
    slot.hidden = false;
  }
}

function onFindInput(e) {
  const inp = e.target && e.target.closest && e.target.closest('[data-sb-find]');
  if (!inp) return;
  clearTimeout(findTimer);
  const q = inp.value.trim();
  findTimer = setTimeout(() => runFind(q), 250);
}

function onFindKey(e) {
  const inp = e.target && e.target.closest && e.target.closest('[data-sb-find]');
  if (!inp) return;
  if (e.key === 'Enter') {
    // Enter opens the first match — the keyboard path to a refocus.
    const first = document.querySelector('[data-sb-findresults] .sb-findfirst');
    if (first) { e.preventDefault(); refocus(first.getAttribute('data-sb-focus')); }
  } else if (e.key === 'Escape') {
    inp.value = '';
    clearTimeout(findTimer);
    const slot = document.querySelector('[data-sb-findresults]');
    if (slot) { slot.hidden = true; slot.innerHTML = ''; }
  }
}

// ----- evidence hover preview ---------------------------------------
//
// Pointer rest (≥300ms) or keyboard focus on a row's meta button
// floats the quote/rationale up in a small fixed card; clicking pins
// the full in-flow paper trail instead. The card is display-only
// (pointer-events none) and never shows raw ids.

let pvTimer = 0;
let pvEl = null; // the one floating card, if any

function edgeById(id) {
  for (const e of (S.ego && S.ego.edges) || []) if (e.edge_id === id) return e;
  for (const e of (S.timeline && S.timeline.edges) || []) if (e.edge_id === id) return e;
  return null;
}

function hidePreview() {
  clearTimeout(pvTimer);
  pvTimer = 0;
  if (pvEl && pvEl.parentNode) pvEl.parentNode.removeChild(pvEl);
  pvEl = null;
}

function showPreview(btn) {
  const id = btn.getAttribute('data-sb-evidence');
  if (S.openEvidence.has(id)) return; // already pinned in-flow — no float
  const e = edgeById(id);
  if (!e) return;
  hidePreview();
  pvEl = document.createElement('div');
  pvEl.className = 'sb-evpreview';
  pvEl.innerHTML = EvidencePreview(e);
  document.body.appendChild(pvEl);
  // Float up: above the row when there is room, below it otherwise,
  // clamped to the viewport either way.
  const r = btn.getBoundingClientRect();
  const w = pvEl.offsetWidth;
  const h = pvEl.offsetHeight;
  const left = Math.max(8, Math.min(r.left, window.innerWidth - w - 8));
  const top = r.top - h - 6 >= 4 ? r.top - h - 6 : r.bottom + 6;
  pvEl.style.left = `${Math.round(left)}px`;
  pvEl.style.top = `${Math.round(top)}px`;
}

function onPointerOver(e) {
  const btn = e.target && e.target.closest && e.target.closest('[data-sb-evidence]');
  if (!btn) return;
  clearTimeout(pvTimer);
  pvTimer = setTimeout(() => showPreview(btn), 300);
}

function onPointerOut(e) {
  const btn = e.target && e.target.closest && e.target.closest('[data-sb-evidence]');
  if (!btn) return;
  // Still inside the same button (child-to-child move)? Keep the card.
  if (e.relatedTarget && btn.contains(e.relatedTarget)) return;
  hidePreview();
}

function onFocusIn(e) {
  const btn = e.target && e.target.closest && e.target.closest('[data-sb-evidence]');
  if (!btn) return;
  showPreview(btn); // keyboard focus previews immediately — no rest delay
}

function onFocusOut(e) {
  const btn = e.target && e.target.closest && e.target.closest('[data-sb-evidence]');
  if (!btn) return;
  hidePreview();
}

(function wire() {
  if (wired || typeof document === 'undefined') return;
  wired = true;
  document.addEventListener('click', onClick);
  document.addEventListener('pointerdown', onPointerDown);
  document.addEventListener('pointermove', onPointerMove);
  document.addEventListener('pointerup', onPointerUp);
  document.addEventListener('pointercancel', onPointerCancel);
  document.addEventListener('keydown', onKeyDown);
  document.addEventListener('input', onFindInput);
  document.addEventListener('keydown', onFindKey);
  document.addEventListener('pointerover', onPointerOver);
  document.addEventListener('pointerout', onPointerOut);
  document.addEventListener('focusin', onFocusIn);
  document.addEventListener('focusout', onFocusOut);
  window.addEventListener('resize', onResize);
})();
