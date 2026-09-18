// ============================================================
// XERJ.ai — SECOND BRAIN · EGO LEDGER primitives
//
// The graph view is a LEDGER, not a hairball. A global force-directed
// graph was rejected on purpose: it collapses past ~200 nodes, its
// layout is non-deterministic (fatal for a memory you revisit — the
// same brain must always draw the same picture), and it optimises for
// looking at over navigating. The ledger is an ego view:
//
//   LEFT column   = what links INTO the focus note, grouped by kind
//   CENTRE        = the focus card (one note, its text, its provenance)
//   RIGHT column  = what the focus note links OUT to, grouped by kind
//
// Graph gestalt comes from 1px orthogonal traces drawn from the focus
// card to each GROUP HEADER (never to every row — zero occlusion, zero
// simulation). Two-steps-away appears as a "+N MORE" count on each
// neighbour row — a promise of the next click, not 27 more drawn
// nodes. The signature interaction is the BELIEF-TIME strip: every
// link's lifetime on one time axis with a draggable as-of caret.
//
// THE MAP (ux/brain-map.js) sits ABOVE this ledger and does not repeal
// the rationale: it draws a BOUNDED number of structure-derived groups
// with a deterministic seeded layout (no live simulation), and every
// terminal click lands back HERE — the ledger stays the leaf view and
// the only evidence surface. This file also renders the mount point,
// the shared scrubber panel, and the statistics row.
//
// LANGUAGE RULE: this surface speaks user words — "link", "believed
// since", "retired", "what taught this". Schema names (src/dst/
// valid_at/edge_id) appear ONLY inside the evidence paper-trail, which
// is the developer-facing raw view, and even there labelled in words.
//
// Honesty rules (§0 invariant 8 of the second-brain contract): every
// cap this view applies is labelled in place ("TOP 12 OF 41"), the
// honesty panel always renders — zeros prove absence — and the
// lexical-embedder disclosure rides the timeline panel.
//
// Every export is a pure HTML-string renderer except
// `drawLedgerTraces`, which measures the live DOM (trace geometry
// depends on final text layout, so it must run post-paint).
// ============================================================

import { esc, fmt, num, Num } from './text.js';
import { Spark, Series, Dist } from './charts.js';

// Rows shown per link-kind group before the in-band "TOP n OF m" cap.
export const LEDGER_GROUP_CAP = 12;
// Belief-strip form switch: up to this many links the strip draws one
// lifetime row per link; beyond it, individual 1px rows become an
// unreadable hatch, so the strip switches to a belief-count curve over
// the same time axis (same caret, same drag). Both forms count every
// fetched link — the switch changes the drawing, never the truth.
export const SCRUB_LIFETIME_MAX = 24;

// ----- user-word vocabulary ----------------------------------------

/** Detector-flavoured edge types → words a first-run user knows.
 *  Unknown types (hand-asserted vocabularies) are prettified raw. */
const TYPE_WORDS = {
  wikilink: 'WIKI-LINKS',
  mdlink: 'MARKDOWN LINKS',
  href: 'HTML LINKS',
  pathcite: 'CITES FILE',
  cratecite: 'CITES CRATE',
  sequence: 'READING ORDER',
  same_dir: 'SAME FOLDER',
  shared_term: 'SHARED WORDING',
  manual: 'HAND-ASSERTED',
};
export const typeName = (t) =>
  TYPE_WORDS[t] || String(t || 'links').replace(/[_-]+/g, ' ').toUpperCase();

/** Detector tags ("wikilink@1") → the same user vocabulary as the link
 *  kinds, with the version kept — versioning IS part of the promise
 *  (same folder in, same links out, bump on any behavior change). The
 *  raw tag stays available on hover and in the evidence paper-trail. */
const DETECTOR_WORDS = {
  wikilink: 'WIKI-LINKS',
  mdlink: 'MARKDOWN LINKS',
  href: 'HTML LINKS',
  pathcite: 'CITES FILE',
  cratecite: 'CITES CRATE',
  sequence: 'READING ORDER',
  samedir: 'FOLDER NEIGHBORS',
  sharedterm: 'SHARED WORDING',
  manual: 'HAND-ASSERTED',
};
export const detectorName = (tag) => {
  const [base, v] = String(tag || '').split('@');
  const w = DETECTOR_WORDS[base];
  if (!w) return String(tag || 'unknown').toUpperCase();
  return v ? `${w} · v${v}` : w;
};

// ----- evidence honesty classes -------------------------------------
//
// Not every link's evidence is a quote, and saying so is load-bearing:
//   AUTHORED links (wikilink/mdlink/href) carry the exact text the
//     author wrote — a real quote, quoted.
//   STRUCTURAL links (samedir/sequence/sharedterm) carry a detector-
//     generated rationale — real evidence of WHY, but never text from
//     the note, so it must never wear quotation marks. sharedterm's
//     rationale NAMES the shared words, which is the whole reason an
//     inferred link is inspectable at all.
//   Links with no evidence at all (asserted by hand or agent without a
//     quote) say so — absence is shown, not papered over.

// pathcite/cratecite quote the author's actual line (a real citation the
// author typed, resolved structurally) — authored, quote-rendered.
const AUTHORED_DETECTORS = new Set(['wikilink', 'mdlink', 'href', 'pathcite', 'cratecite']);
const STRUCTURAL_DETECTORS = new Set(['samedir', 'sequence', 'sharedterm']);
const detectorBase = (tag) => String(tag || '').split('@')[0];

/** 'quote' | 'rationale' | 'none' — how this link's evidence renders. */
export const evidenceKind = (e) => {
  const q = e && e.evidence && e.evidence.quote;
  if (!q) return 'none';
  return STRUCTURAL_DETECTORS.has(detectorBase(e.detector)) ? 'rationale' : 'quote';
};

/**
 * Live-link counts split by what kind of teaching stands behind them,
 * from `overview.detectors`. AUTHORED = a person wrote that link;
 * STRUCTURAL = folder position / sort order (a prior, not a citation);
 * anything else (hand- or agent-asserted vocabularies) is counted
 * apart, never folded in. This split is what keeps a wikilink-free
 * corpus honest: "0 authored · 214 structural", not a proud 214.
 */
export function authoredSplit(detectors) {
  const s = { authored: 0, structural: 0, asserted: 0 };
  for (const d of detectors || []) {
    const b = detectorBase(d.detector);
    if (AUTHORED_DETECTORS.has(b)) s.authored += d.live || 0;
    else if (STRUCTURAL_DETECTORS.has(b)) s.structural += d.live || 0;
    else s.asserted += d.live || 0;
  }
  return s;
}

/** The split as one lowercase hint phrase. */
export const splitLabel = (s) =>
  `${num(s.authored)} authored · ${num(s.structural)} structural${s.asserted > 0 ? ` · ${num(s.asserted)} asserted` : ''}`;

/** Ids are opaque doc ids — autoindex ones are long hashes. Never make
 *  a human read 32 hex chars as a name. */
const looksHashy = (id) => /^[0-9a-f]{16,}$/i.test(String(id || ''));
export const displayId = (id) => {
  const s = String(id || '');
  return looksHashy(s) ? `${s.slice(0, 10)}…` : s;
};

/** Display name for a node id: hydrated title first (ego nodes, then
 *  the console's own name cache), else the id (shortened if hashy). */
const nodeName = (id, ctx) => {
  const fromEgo = ctx && ctx.nodes && ctx.nodes[id] && ctx.nodes[id].title;
  if (fromEgo) return fromEgo;
  const fromCache = ctx && ctx.names && ctx.names[id] && ctx.names[id].title;
  if (fromCache) return fromCache;
  return displayId(id);
};

// ----- time ---------------------------------------------------------

/** Epoch-ms → 'YYYY-MM-DD HH:MM' (UTC — belief time is server time). */
export const fmtTs = (ms) => {
  if (ms == null || !Number.isFinite(Number(ms))) return '—';
  return new Date(Number(ms)).toISOString().slice(0, 16).replace('T', ' ');
};

/** Epoch-ms → 'YYYY-MM-DD' (UTC). Row-width-friendly. */
export const fmtDay = (ms) => {
  if (ms == null || !Number.isFinite(Number(ms))) return '—';
  return new Date(Number(ms)).toISOString().slice(0, 10);
};

/** A link's lifetime as words. The tags on the row (RETIRED / NOT
 *  YET) carry the state at the caret; this carries the facts. */
const fmtWindow = (validAt, invalidAt) =>
  invalidAt == null
    ? `since ${fmtDay(validAt)}`
    : `${fmtDay(validAt)} → ${fmtDay(invalidAt)}`;

// ----- bi-temporal cut (client-side preview only) -------------------
//
// The SERVER owns the as-of cut (`ego?as_of=`). These helpers exist so
// the scrubber can preview visibility over already-fetched links while
// the caret is mid-drag; the release commits an honest re-query.

export const edgeStateAt = (edge, asOfMs) => {
  const t = asOfMs == null ? Date.now() : asOfMs;
  if (Number(edge.valid_at) > t) return 'future';
  if (edge.invalid_at != null && Number(edge.invalid_at) <= t) return 'expired';
  return 'live';
};

// ----- neighbours + grouping ---------------------------------------

/** The node on the far end of an edge, given the expansion direction. */
const farEnd = (edge) => (edge.direction === 'in' ? edge.src : edge.dst);

/**
 * Hop-1 edges of one side, grouped by type. Order is deterministic:
 * groups by count desc then type asc; rows keep the server's contract
 * order (hop asc, weight desc, edge_id asc) untouched — the same data
 * always draws the same picture.
 */
export function groupEdges(edges, side) {
  const byType = new Map();
  for (const e of edges || []) {
    if (e.hop !== 1) continue;
    if ((side === 'out') !== (e.direction === 'out')) continue;
    if (!byType.has(e.type)) byType.set(e.type, []);
    byType.get(e.type).push(e);
  }
  const groups = [...byType.entries()].map(([type, list]) => ({ type, list }));
  groups.sort((a, b) => b.list.length - a.list.length || (a.type < b.type ? -1 : a.type > b.type ? 1 : 0));
  return groups;
}

/**
 * Per-neighbour hop-2 degree: how many two-steps-away links touch this
 * node, counting only links live at the caret. When the server clipped
 * the result set the count is a floor, rendered '≥n' — never a fake
 * exact.
 */
export function hop2Counts(edges, asOfMs) {
  const counts = new Map();
  for (const e of edges || []) {
    if (e.hop !== 2) continue;
    if (edgeStateAt(e, asOfMs) !== 'live') continue;
    counts.set(e.src, (counts.get(e.src) || 0) + 1);
    counts.set(e.dst, (counts.get(e.dst) || 0) + 1);
  }
  return counts;
}

// ----- evidence -----------------------------------------------------

/** Detector-aware evidence line. A quote is quoted; a structural
 *  rationale is prefixed WHY — and never dressed as text from the
 *  note; no evidence says so in as many words. */
const evidenceLine = (e) => {
  const kind = evidenceKind(e);
  if (kind === 'quote') return `<p class="sb-evquote">“${esc(e.evidence.quote)}”</p>`;
  if (kind === 'rationale') return `<p class="sb-evwhy">WHY — ${esc(e.evidence.quote)}</p>`;
  return `<p class="sb-evquote sb-evnone">NO EVIDENCE RECORDED — ASSERTED, NOT DETECTED</p>`;
};

/** The evidence that taught this link, plus its full paper trail.
 *  This block is the ONLY place raw ids surface — labelled. `pinned`
 *  renders it open in-flow (the pin set lives in module state on the
 *  controller side, so pins survive every re-render). */
const EvidenceBlock = (e, pinned) => {
  const ev = e.evidence || {};
  const src = ev.source
    ? `<span>from ${esc(ev.source)}${ev.offset != null && ev.offset > 0 ? ` · byte ${esc(ev.offset)}` : ''}</span>`
    : '';
  const conf = e.confidence ?? 1;
  const invalid = e.invalid_at != null
    ? ` · believed until ${esc(fmtTs(e.invalid_at))}` : '';
  const expired = e.expired_at != null
    ? ` · retirement recorded ${esc(fmtTs(e.expired_at))}` : '';
  return `
  <div class="sb-evbody" data-sb-evbody${pinned ? '' : ' hidden'}>
    ${evidenceLine(e)}
    <div class="sb-evmeta mono faint">
      ${src}
      <span>taught by ${esc(detectorName(e.detector).toLowerCase())} (${esc(e.detector || 'unknown detector')}) · ${esc(fmt(conf * 100, { decimals: 0 }))}% confident · strength ${esc(fmt(e.weight ?? 1, { decimals: 2 }))}</span>
      <span>believed since ${esc(fmtTs(e.valid_at))}${invalid}${expired}</span>
      <span>link id ${esc(e.edge_id)} · this is the handle the API takes</span>
    </div>
  </div>`;
};

/**
 * Floating hover card: the evidence line + one taught-by sentence. No
 * raw ids here — the full paper trail is one click (the pin) away.
 * Rendered into a fixed-position card by the interaction controller.
 */
export const EvidencePreview = (e) => {
  const conf = e.confidence ?? 1;
  return `
  ${evidenceLine(e)}
  <div class="sb-evmeta mono faint">
    <span>taught by ${esc(detectorName(e.detector).toLowerCase())} · ${esc(fmt(conf * 100, { decimals: 0 }))}% confident · click to pin the paper trail</span>
  </div>`;
};

// ----- rows + groups ------------------------------------------------

/** The row chip naming what stands behind the link: QUOTE (authored
 *  text), WHY (structural rationale), or NO EVIDENCE. */
const evidenceChip = (e) => {
  const kind = evidenceKind(e);
  if (kind === 'quote') return '<span class="sb-eq">QUOTE</span>';
  if (kind === 'rationale') return '<span class="sb-eq sb-eq-why">WHY</span>';
  return '<span class="sb-eq sb-eq-none">NO EVIDENCE</span>';
};

const edgeRow = (e, side, { counts, asOf, clipped, nodes, names, open }) => {
  const id = farEnd(e);
  const state = edgeStateAt(e, asOf);
  const n = counts.get(id) || 0;
  const pinned = !!(open && open.has(e.edge_id));
  const hop2 = n > 0
    ? `+${clipped ? '≥' : ''}${num(n)} MORE`
    : '';
  return `
  <div class="sb-erow${state === 'expired' ? ' sb-expired' : ''}${state === 'future' ? ' sb-off' : ''}"
       data-sb-edge="${esc(e.edge_id)}" data-va="${esc(e.valid_at)}"${e.invalid_at != null ? ` data-vi="${esc(e.invalid_at)}"` : ''}>
    <div class="sb-erow-main">
      <button type="button" class="sb-ename" data-sb-focus="${esc(id)}"
              title="Open the ledger for ${esc(id)}">${esc(nodeName(id, { nodes, names }))}</button>
      <button type="button" class="sb-emeta" data-sb-evidence="${esc(e.edge_id)}"
              aria-expanded="${pinned}"
              title="Show what taught this link — click to pin the full paper trail">
        <span class="sb-ewin">${esc(fmtWindow(e.valid_at, e.invalid_at))}</span>
        ${evidenceChip(e)}
        ${hop2 ? `<span class="sb-ehop2" title="${esc(num(n))} more link${n === 1 ? '' : 's'} one step beyond — click the name to walk there">${esc(hop2)}</span>` : ''}
        <span class="sb-tag sb-tag-retired">RETIRED</span>
        <span class="sb-tag sb-tag-notyet">NOT YET</span>
      </button>
    </div>
    ${EvidenceBlock(e, pinned)}
  </div>`;
};

const groupBlock = (g, side, ctx) => {
  const total = g.list.length;
  const shown = Math.min(total, LEDGER_GROUP_CAP);
  const countLabel = shown < total
    ? `TOP ${num(shown)} OF ${num(total)}`
    : `${num(total)}`;
  // Collapse is a client-side fold — header + count stay visible, the
  // honesty counters never change, nothing is refetched.
  const key = `${side}:${g.type}`;
  const collapsed = !!(ctx.collapsed && ctx.collapsed.has(key));
  return `
  <div class="sb-group">
    <button type="button" class="sb-grouphead" data-sb-grouphead="${side}"
            data-sb-collapse="${esc(key)}" aria-expanded="${!collapsed}"
            title="${collapsed ? 'Show' : 'Fold away'} these links (nothing is refetched)">
      <span class="sb-ghtype"><span class="sb-ghcaret" data-sb-ghcaret>${collapsed ? '▸' : '▾'}</span>${esc(typeName(g.type))}</span>
      <span class="sb-ghcount">${esc(countLabel)}</span>
    </button>
    <div class="sb-grouprows" data-sb-grouprows${collapsed ? ' hidden' : ''}>
      ${g.list.slice(0, LEDGER_GROUP_CAP).map((e) => edgeRow(e, side, ctx)).join('')}
    </div>
  </div>`;
};

// ----- focus card ---------------------------------------------------

const FocusCard = ({ ego, nodes, names, brain, asOf }) => {
  const id = ego.node;
  const hop1 = (ego.edges || []).filter((e) => e.hop === 1);
  // Degrees speak the page's tense — BELIEVED AT THIS MOMENT — so they
  // count only links live at the caret; retired ones are counted apart
  // (they are drawn struck-through in the columns either way).
  const live = hop1.filter((e) => edgeStateAt(e, asOf) === 'live');
  const nRetired = hop1.filter((e) => edgeStateAt(e, asOf) === 'expired').length;
  const nIn = live.filter((e) => e.direction === 'in').length;
  const nOut = live.length - nIn;
  const kinds = new Set(live.map((e) => e.type)).size;
  const earliest = hop1.length
    ? Math.min(...hop1.map((e) => Number(e.valid_at)))
    : null;
  const meta = nodes && nodes[id];
  const preview = meta && meta.preview
    ? `<p class="sb-ftext">${esc(meta.preview)}</p>`
    : '';
  // The real file behind the note, when the engine (or the console's
  // name cache) knows it — a truthful label, never derived from the id.
  const path = (meta && typeof meta.path === 'string' && meta.path)
    || (names && names[id] && names[id].path) || null;
  const dangling = !meta && ego.not_shown
    && Array.isArray(ego.not_shown.dangling_ids)
    && ego.not_shown.dangling_ids.includes(id);
  return `
  <div class="sb-focuscard" data-sb-focuscard>
    <div class="key">FOCUS</div>
    <div class="sb-ftitle">${esc(nodeName(id, { nodes, names }))}</div>
    ${preview}
    <div class="sb-fdeg mono">
      <span>← <span class="sb-fnum">${num(nIn)}</span> IN</span>
      <span><span class="sb-fnum">${num(nOut)}</span> OUT →</span>
      <span><span class="sb-fnum">${num(kinds)}</span> KIND${kinds === 1 ? '' : 'S'}</span>
      ${nRetired > 0 ? `<span class="sb-fret">+${num(nRetired)} RETIRED</span>` : ''}
    </div>
    <div class="sb-fprov mono faint">
      ${esc(meta && meta.index ? meta.index : `brain ${brain}`)}${path ? ` · ${esc(path)}` : ''} · id ${esc(id)}${dangling ? ' · NO DOCUMENT BEHIND THIS ID' : ''}${earliest != null ? ` · in this brain since ${esc(fmtDay(earliest))}` : ''}
    </div>
  </div>`;
};

// ----- controls line ------------------------------------------------

const EgoControls = ({ brain, brains, focus, asOf, trail, names, nodes }) => {
  const others = (brains || []).filter((b) => b !== brain);
  const brainSwitch = others.length
    ? `<span class="sb-cswitch">${others.map((b) =>
      `<button type="button" class="sb-clink" data-sb-brain="${esc(b)}" title="Switch to brain ${esc(b)}">${esc(b)}</button>`).join('<span class="faint"> · </span>')}</span>`
    : '';
  const crumbs = (trail || []).filter((t) => t !== focus).slice(-3);
  const crumbName = (t) => {
    const n = nodeName(t, { nodes, names });
    return n.length > 18 ? `${n.slice(0, 17)}…` : n;
  };
  const trailHtml = crumbs.length
    ? `<span class="sb-ctrail">${crumbs.map((t) =>
      `<button type="button" class="sb-clink" data-sb-focus="${esc(t)}" title="Back to ${esc(t)}">${esc(crumbName(t))}</button>`).join('<span class="faint"> → </span>')}<span class="faint"> → </span></span>`
    : '';
  return `
  <div class="sb-controls mono">
    <span><span class="key">BRAIN</span> <span class="sb-cval">${esc(brain || '—')}</span> ${brainSwitch}</span>
    <span><span class="key">FOCUS</span> ${trailHtml}<span class="sb-cval">${esc(focus ? crumbName(focus) : '—')}</span></span>
    <span><span class="key">VIEWING</span> <span class="sb-cval${asOf != null ? ' accent' : ''}">${asOf != null ? esc(fmtTs(asOf)) + ' UTC' : 'NOW'}</span>${asOf != null ? ` <button type="button" class="sb-clink" data-sb-now>BACK TO NOW</button>` : ''}</span>
    <span class="sb-cfind"><span class="key">FIND</span><input class="sb-findinput" type="text"
      data-sb-find spellcheck="false" autocomplete="off"
      placeholder="a note, by the words in it"
      aria-label="Find a note in this brain — Enter opens the first match"></span>
    <span class="sb-cerr" data-sb-error></span>
  </div>
  <div class="sb-findresults" data-sb-findresults hidden></div>`;
};

/**
 * FIND results: a temporary strip of note links under the controls.
 * Every hit is the same refocus affordance as any other note name; an
 * empty answer teaches what the brain is (and is not).
 */
export const FindResults = ({ hits, names }) => {
  if (!hits || !hits.length) {
    return `<span class="sb-sidenote mono faint">NO NOTE MATCHES — THE BRAIN ONLY KNOWS WHAT THE FOLDER TAUGHT IT</span>`;
  }
  return hits.map((h, i) =>
    `<button type="button" class="sb-clink sb-findrow${i === 0 ? ' sb-findfirst' : ''}"
      data-sb-focus="${esc(h.id)}" title="Open the ledger for ${esc(h.id)}">${esc(h.title || nodeName(h.id, { names }))}</button>`)
    .join('<span class="faint"> · </span>');
};

// ----- belief-time scrubber ----------------------------------------

/** Caret-label edge alignment: centred by default, left-anchored near
 *  the left edge, right-anchored near the right edge — so the label
 *  never clips off the strip at any width. Shared with the drag
 *  preview in data/second-brain-api.js. */
export const caretAlignClass = (pct) =>
  pct < 12 ? 'sb-cl-left' : pct > 88 ? 'sb-cl-right' : '';

/**
 * Full-width belief-time strip with a draggable as-of caret. Two forms
 * from one time axis, picked by how many links there are:
 *
 *   ≤ SCRUB_LIFETIME_MAX  one row per link, drawn from the moment the
 *                         brain started believing it to the moment it
 *                         stopped (or to now); retired rows go dashed.
 *   beyond                a 1px step curve counting how many links were
 *                         believed at every moment (individual 1px rows
 *                         beyond ~24 are an unreadable hatch).
 *
 * Dragging previews client-side; releasing re-queries the server with
 * `as_of` (the caret is a query parameter, not a client filter). The
 * interval set is pinned at NOW with retired links included, so
 * scrubbing never removes data from the strip — only states change.
 */
export const BeliefScrubber = ({ timeline, asOf, shift }) => {
  const edges = (timeline && timeline.edges) || [];
  if (!edges.length) {
    return `<div class="sb-scrub" data-sb-scrub><div class="panel-empty mono faint">NOTHING TO REPLAY YET · EVERY LINK WILL DRAW ITS LIFETIME HERE</div></div>`;
  }
  // The fetch that feeds the strip is capped (ego limit); when the
  // server clipped, every count here is a floor — say so with '≥'.
  const clipped = !!(timeline && timeline.not_shown && timeline.not_shown.edges_clipped > 0);
  const floor = clipped ? '≥' : '';

  const now = Date.now();
  const d0raw = Math.min(...edges.map((e) => Number(e.valid_at)));
  const span = Math.max(now - d0raw, 60_000);
  const d0 = d0raw - span * 0.04;         // 4% air before the first assertion
  const d1 = now;
  const pctOf = (ms) => Math.max(0, Math.min(100, ((ms - d0) / (d1 - d0)) * 100));

  const caretMs = asOf == null ? d1 : asOf;
  const caretPct = pctOf(caretMs);
  const labelPct = Math.max(1, Math.min(99, caretPct));
  const liveAt = (t) => edges.reduce((a, e) => a + (edgeStateAt(e, t) === 'live' ? 1 : 0), 0);
  const liveAtCaret = liveAt(asOf);

  // Every belief event on the strip (assertions AND retirements),
  // sorted, deduped: the caret snaps to these on release and the arrow
  // keys step between them — moments where nothing changed are not
  // interesting stops.
  const eventTimes = [...new Set(
    edges.flatMap((e) => (e.invalid_at != null
      ? [Number(e.valid_at), Number(e.invalid_at)]
      : [Number(e.valid_at)])),
  )].sort((a, b) => a - b);
  const eventsAttr = ` data-sb-events="${esc(eventTimes.join(','))}"`;

  const lifetimes = edges.length <= SCRUB_LIFETIME_MAX;
  let stripInner = '';
  let h = 0;
  let stepsAttr = '';
  let explain = '';

  if (lifetimes) {
    // Deterministic row order: strength desc, believed-since asc, id asc.
    const rows = [...edges].sort((a, b) =>
      (b.weight - a.weight)
      || (Number(a.valid_at) - Number(b.valid_at))
      || (a.edge_id < b.edge_id ? -1 : 1)
    );
    const rowH = rows.length <= 6 ? 10 : rows.length <= 12 ? 8 : 6;
    h = Math.max(44, rows.length * rowH + 8);
    const lines = rows.map((e, i) => {
      const y = 6 + i * rowH;
      const x1 = pctOf(Number(e.valid_at));
      let x2 = e.invalid_at != null ? pctOf(Number(e.invalid_at)) : 100;
      // A belief held briefly must not vanish into a zero-width line.
      if (x2 - x1 < 0.3) x2 = Math.min(100, x1 + 0.3);
      const state = edgeStateAt(e, asOf);
      const tick = e.invalid_at != null
        ? `<line class="sb-ltick" x1="${x2}%" x2="${x2}%" y1="${y - 3}" y2="${y + 3}" stroke="currentColor" stroke-width="1"/>`
        : '';
      return `<line class="sb-lseg sb-l-${state}" data-va="${esc(e.valid_at)}"${e.invalid_at != null ? ` data-vi="${esc(e.invalid_at)}"` : ''} x1="${x1}%" x2="${x2}%" y1="${y}" y2="${y}" stroke="currentColor" stroke-width="1"/>${tick}`;
    }).join('');
    stripInner = `<svg class="chart sb-striplines" width="100%" height="${h}" aria-hidden="true">${lines}</svg>`;
    explain = `EACH LINE IS ONE LINK'S LIFETIME · RETIRED LINKS KEEP THEIR ROW, DASHED`;
  } else {
    // Belief-count step curve. Events: +1 when a link starts being
    // believed, −1 when it retires; the curve is the running count.
    const ev = [];
    for (const e of edges) {
      ev.push([Number(e.valid_at), 1]);
      if (e.invalid_at != null) ev.push([Number(e.invalid_at), -1]);
    }
    ev.sort((a, b) => a[0] - b[0] || b[1] - a[1]);
    const steps = []; // [t, countFromT]
    let c = 0;
    for (const [t, d] of ev) {
      c += d;
      if (steps.length && steps[steps.length - 1][0] === t) steps[steps.length - 1][1] = c;
      else steps.push([t, c]);
    }
    const maxC = Math.max(...steps.map((s) => s[1]), 1);
    h = 88;
    const X = 1000; // viewBox units; preserveAspectRatio=none stretches
    const xOf = (ms) => (pctOf(ms) / 100) * X;
    const yOf = (n) => 4 + (1 - n / maxC) * (h - 10);
    let pts = `0,${yOf(0).toFixed(1)}`;
    let prevY = yOf(0);
    for (const [t, n] of steps) {
      const x = xOf(t).toFixed(1);
      const y = yOf(n);
      pts += ` ${x},${prevY.toFixed(1)} ${x},${y.toFixed(1)}`;
      prevY = y;
    }
    pts += ` ${X},${prevY.toFixed(1)}`;
    stepsAttr = ` data-sb-steps="${esc(steps.map(([t, n]) => `${t}:${n}`).join(','))}"`;
    // Assertion ticks: one 1px mute tick per moment a link started
    // being believed (deduped at tick resolution) — the snap targets,
    // made visible on the curve form where individual rows are gone.
    const seenX = new Set();
    let ticks = '';
    for (const e of edges) {
      const x = xOf(Number(e.valid_at));
      const xq = Math.round(x);
      if (seenX.has(xq)) continue;
      seenX.add(xq);
      ticks += `<line class="sb-atick" x1="${x.toFixed(1)}" x2="${x.toFixed(1)}" y1="${h - 6}" y2="${h - 1}" stroke="currentColor" stroke-width="1"/>`;
    }
    stripInner = `<svg class="chart sb-striplines" width="100%" height="${h}" viewBox="0 0 ${X} ${h}" preserveAspectRatio="none" aria-hidden="true">
      ${ticks}
      <polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1"/>
    </svg>
    <span class="sb-curvemax mono faint">peak ${esc(floor)}${num(maxC)}</span>`;
    explain = `THE LINE COUNTS BELIEFS OVER TIME — TOO MANY LINKS (${floor}${num(edges.length)}) TO DRAW ONE PER ROW`;
  }

  // What the last time-travel changed, stated in the legend: counts of
  // appearances and retirements between the two carets — always named
  // separately, never netted, '≥' floors when the fetch was clipped.
  // "THIS MOMENT" must name the caret's end of the crossed interval:
  // dragging right the caret sits at t1, dragging left it sits at t0 —
  // the other endpoint is named by its date, never misattributed.
  const shiftWhen = shift
    ? (Math.abs(caretMs - shift.t1) <= Math.abs(caretMs - shift.t0)
      ? `BETWEEN ${esc(fmtDay(shift.t0))} AND THIS MOMENT`
      : `BETWEEN THIS MOMENT AND ${esc(fmtDay(shift.t1))}`)
    : '';
  const shiftHtml = shift
    ? `<span class="sb-changeout" data-sb-changeout>↳ ${shift.appeared === 0 && shift.retired === 0
      ? 'NOTHING CHANGED'
      : `${esc(floor)}${num(shift.appeared)} APPEARED · ${esc(floor)}${num(shift.retired)} RETIRED`} ${shiftWhen}</span>`
    : '';

  return `
  <div class="sb-scrub" data-sb-scrub data-d0="${d0}" data-d1="${d1}"${stepsAttr}${eventsAttr}>
    <div class="key">BELIEF TIME · DRAG ANYWHERE ON THE STRIP — THE PAGE REPLAYS WHAT THIS BRAIN BELIEVED AT THAT MOMENT</div>
    <div class="sb-strip" data-sb-strip style="height:${h}px;">
      ${stripInner}
      <div class="sb-caret" data-sb-caret style="left:${caretPct}%;" role="slider" tabindex="0"
           aria-label="Belief time — what did this brain believe, when"
           aria-valuemin="${d0}" aria-valuemax="${d1}" aria-valuenow="${caretMs}"
           aria-valuetext="${asOf == null ? 'now' : esc(fmtTs(caretMs)) + ' UTC'}"></div>
      <div class="sb-caret-label mono ${caretAlignClass(labelPct)}" data-sb-caret-label style="left:${labelPct}%;">${asOf == null ? 'NOW' : esc(fmtTs(caretMs))}</div>
    </div>
    <div class="sb-scrub-legend mono faint">
      <span>${esc(fmtDay(d0raw))}</span>
      <span class="mid"><span data-sb-livecount>${esc(floor)}${num(liveAtCaret)} OF ${esc(floor)}${num(edges.length)}</span> BELIEVED AT THIS MOMENT<span class="sb-scrub-explain"> · ${explain}</span>${shiftHtml}</span>
      <span>${asOf != null ? `<button type="button" class="text-btn" data-sb-now>BACK TO NOW</button>` : `TODAY · ${esc(fmtDay(now))}`}</span>
    </div>
  </div>`;
};

// ----- the ledger ---------------------------------------------------

const sideCol = (groups, side, ctx, caption, emptyLabel) => {
  const inner = groups.length
    ? groups.map((g) => groupBlock(g, side, ctx)).join('')
    : `<div class="sb-sidenote mono faint">${esc(emptyLabel)}</div>`;
  return `<div class="sb-col sb-col-${side}">
    <div class="sb-sidecap key">${esc(caption)}</div>
    ${inner}
  </div>`;
};

/**
 * The three-column ego ledger + rails. The brain/focus/FIND controls
 * and the belief-time scrubber used to render inside this panel; they
 * are now panels of their own (`controls` at the top of the page, and
 * `scrub` between the map and the statistics row) so ONE caret drives
 * both the map and the ledger — same module state, same commit path.
 */
export const EgoLedger = ({ data }) => {
  const sb = data.sb || {};
  const ego = data.ego;
  const asOf = sb.asOf ?? null;
  const names = sb.names || {};
  if (!ego) {
    return `
  <div class="panel-empty mono faint">${sb.focus ? 'READING WHAT THE BRAIN BELIEVES ABOUT ' + esc(String(sb.focus).toUpperCase()) + '…' : 'NO NOTE IN FOCUS · CLICK ANY NOTE ON THIS PAGE TO OPEN ITS LEDGER'}</div>`;
  }
  const nodes = ego.nodes || {};
  const clipped = !!(ego.not_shown && ego.not_shown.edges_clipped > 0);
  const counts = hop2Counts(ego.edges, asOf);
  const ctx = {
    nodes, names, counts, asOf, clipped,
    open: sb.openEvidence,     // pinned paper trails — survive re-renders
    collapsed: sb.collapsed,   // folded link-kind groups
  };
  const inGroups = groupEdges(ego.edges, 'in');
  const outGroups = groupEdges(ego.edges, 'out');
  const hop1 = (ego.edges || []).filter((e) => e.hop === 1);

  const body = hop1.length
    ? `
  <div class="sb-ledger" data-sb-ledger>
    ${sideCol(inGroups, 'in', ctx, '← WHAT LINKS IN', 'NOTHING LINKS IN AT THIS MOMENT')}
    <div class="sb-rail" data-sb-rail="in" aria-hidden="true"></div>
    ${FocusCard({ ego, nodes, names, brain: data.brain, asOf })}
    <div class="sb-rail" data-sb-rail="out" aria-hidden="true"></div>
    ${sideCol(outGroups, 'out', ctx, 'WHERE IT LINKS OUT →', 'NO OUTBOUND LINKS AT THIS MOMENT')}
  </div>`
    : `
  <div class="sb-ledger" data-sb-ledger>
    <div class="sb-col sb-col-in"></div>
    <div class="sb-rail" data-sb-rail="in" aria-hidden="true"></div>
    ${FocusCard({ ego, nodes, names, brain: data.brain, asOf })}
    <div class="sb-rail" data-sb-rail="out" aria-hidden="true"></div>
    <div class="sb-col sb-col-out"></div>
  </div>
  <div class="panel-empty mono faint">NO LINKS TOUCH THIS NOTE AT THIS MOMENT · DRAG BELIEF TIME BACK TO NOW, OR CLICK ANOTHER NOTE BELOW</div>`;

  return body;
};

// ----- honesty panel ------------------------------------------------

const hrow = (k, v, emph, rawName) => `
  <div class="sb-hrow"${rawName ? ` title="API field: ${esc(rawName)}"` : ''}>
    <span class="sb-hk">${esc(k)}</span>
    <span class="sb-hv${emph && v !== '0' ? ' accent' : ''}">${esc(v)}</span>
  </div>`;

/**
 * Definition list over every `not_shown` counter of both endpoints
 * plus the view's own caps — in user words (the raw API field name
 * rides on hover). ALWAYS renders, zeros included — this panel exists
 * to prove absence, not just to report presence.
 */
export const HonestyList = ({ data }) => {
  const ns = (data.ego && data.ego.not_shown) || {};
  const on = (data.overview && data.overview.not_shown) || {};
  const z = (v) => num(v || 0);
  const dangling = Array.isArray(ns.dangling_ids) && ns.dangling_ids.length
    ? `<div class="sb-hids mono faint">no document behind: ${ns.dangling_ids.slice(0, 8).map(esc).join(', ')}${ns.dangling_ids.length > 8 ? ` +${ns.dangling_ids.length - 8} more` : ''}</div>`
    : '';
  // The belief strip draws EVERY link it fetched (rows or count-curve —
  // the form switches, the truth does not); what it can hide is what
  // the fetch itself clipped server-side.
  const tlns = (data.sb && data.sb.timeline && data.sb.timeline.not_shown) || {};
  return `
  <div class="sb-honesty mono">
    <div class="sb-hgroup key">THE LEDGER READ</div>
    ${hrow('links beyond the fetch cap (not drawn)', z(ns.edges_clipped), true, 'edges_clipped')}
    ${hrow('starting notes the walk had to drop', z(ns.frontier_clipped), true, 'frontier_clipped')}
    ${hrow('links hidden by the belief-time cut', z(ns.expired_excluded), false, 'expired_excluded')}
    ${hrow('links hidden by a kind filter', z(ns.type_filtered), false, 'type_filtered')}
    ${hrow('storage segments this read had to skip', z(ns.segments_without_columns), true, 'segments_without_columns')}
    ${hrow('unflushed writes checked (cost, not loss)', z(ns.memtable_docs_scanned), false, 'memtable_docs_scanned')}
    ${hrow('links pointing at notes with no document', z(ns.dangling_nodes), true, 'dangling_nodes')}
    ${dangling}
    <div class="sb-hgroup key">THE COUNTS ABOVE</div>
    ${hrow('link kinds beyond the top list', z(on.types_not_listed), true, 'types_not_listed')}
    ${hrow('detectors beyond the top list', z(on.detectors_not_listed), true, 'detectors_not_listed')}
    ${hrow('most-citing notes beyond the top list', z(on.hubs_out_not_listed), false, 'hubs_out_not_listed')}
    ${hrow('most-cited notes beyond the top list', z(on.hubs_in_not_listed), false, 'hubs_in_not_listed')}
    <div class="sb-hgroup key">CAPS THIS PAGE APPLIES</div>
    ${hrow('ledger rows per link kind (capped groups say so)', `≤ ${num(LEDGER_GROUP_CAP)}`, false)}
    ${hrow('belief-strip lifetimes beyond its fetch cap', z(tlns.edges_clipped), true, 'edges_clipped')}
  </div>`;
};

// ----- hubs (centers of gravity) ------------------------------------

const hubList = (items, notListed, focus, ctx) => {
  if (!items || !items.length) return `<div class="sb-sidenote mono faint">NOTHING RANKS YET</div>`;
  const max = Math.max(...items.map((i) => i.live_edges), 1);
  const rows = items.map((i) => {
    const frac = Math.max(0, Math.min(1, i.live_edges / max));
    return `
    <button type="button" class="sb-hubrow${i.id === focus ? ' sb-hub-active' : ''}" data-sb-focus="${esc(i.id)}"
         title="Open the ledger for ${esc(i.id)}">
      <span class="sb-hubname">${esc(nodeName(i.id, ctx))}</span>
      <span class="sb-hubn">${esc(num(i.live_edges))}</span>
      <svg class="chart sb-hubbar" height="6" viewBox="0 0 100 6" preserveAspectRatio="none" aria-hidden="true">
        <line x1="0" y1="5" x2="${(frac * 100).toFixed(1)}" y2="5" stroke="currentColor" stroke-width="1"/>
      </svg>
    </button>`;
  }).join('');
  const tail = notListed > 0
    ? `<div class="hint">+ ${num(notListed)} more beyond this list</div>` : '';
  return rows + tail;
};

export const HubsPanel = ({ data }) => {
  const o = data.overview || {};
  const hubs = o.hubs || { out: [], in: [] };
  const ns = o.not_shown || {};
  const sb = data.sb || {};
  const focus = sb.focus || null;
  const ctx = { nodes: (data.ego && data.ego.nodes) || {}, names: sb.names || {} };
  return `
  <div class="sb-hubs">
    <div>
      <div class="key">MOST CITED · LINKS POINTING AT A NOTE</div>
      ${hubList(hubs.in, ns.hubs_in_not_listed, focus, ctx)}
    </div>
    <div>
      <div class="key">CITES THE MOST · LINKS A NOTE SENDS OUT</div>
      ${hubList(hubs.out, ns.hubs_out_not_listed, focus, ctx)}
    </div>
  </div>
  <div class="hint" style="margin-top:var(--sp-1);">click any note to open its ledger above</div>`;
};

// ----- small panels -------------------------------------------------

/** 2-series comparison legend chip: a short 3px line in the series
 *  color + the number IN INK — color is identity, text is the value. */
const legChip = (cls, label) =>
  `<span class="sb-leg"><svg class="sb-legline ${cls}" width="14" height="4" viewBox="0 0 14 4" aria-hidden="true"><line x1="0" y1="2" x2="14" y2="2" stroke="currentColor" stroke-width="3"/></svg>${label}</span>`;

/**
 * BELIEVED NOW — headline number + the believed-vs-retired composition
 * bar. The ONE two-series comparison on this page: accent (believed)
 * against --z-cmp (retired), 2px surface gap, both directly labelled.
 */
const BelievedPanel = ({ data }) => {
  const o = data.overview || {};
  const ed = o.edges || { total: 0, live: 0, invalidated: 0 };
  const m = (data.metrics && data.metrics.edgesLive) || { formatted: num(ed.live), hint: '' };
  if (!ed.total) {
    return Num({ value: '0', unit: 'links', hint: 'nothing asserted yet — see below', emphasis: true });
  }
  const W = 200;
  const both = ed.live > 0 && ed.invalidated > 0;
  let liveW = Math.round((ed.live / ed.total) * W);
  // When both segments exist, neither may vanish (a 0.5% retired share
  // must still be visibly present) and neither may go negative-width.
  if (both) liveW = Math.max(4, Math.min(W - 4, liveW));
  const gap = both ? 1 : 0;
  const bar = `
  <svg class="chart sb-split" width="100%" height="8" viewBox="0 0 ${W} 8" preserveAspectRatio="none" aria-hidden="true">
    ${ed.live > 0 ? `<line class="sb-seg-live" x1="0" y1="4" x2="${liveW - gap}" y2="4" stroke="currentColor" stroke-width="3"/>` : ''}
    ${ed.invalidated > 0 ? `<line class="sb-seg-retired" x1="${liveW + gap}" y1="4" x2="${W}" y2="4" stroke="currentColor" stroke-width="3"/>` : ''}
  </svg>`;
  return `
  ${Num({ value: m.formatted, unit: 'links', hint: m.hint, emphasis: true })}
  <div class="sb-splitwrap">
    ${bar}
    <div class="sb-splitleg mono">
      ${legChip('sb-seg-live', `${num(ed.live)} BELIEVED`)}
      ${legChip('sb-seg-retired', `${num(ed.invalidated)} RETIRED`)}
    </div>
  </div>`;
};

/** EVER ASSERTED — total + the growth shape (cumulative spark). */
const AssertedPanel = ({ data }) => {
  const o = data.overview || {};
  const ed = o.edges || { total: 0 };
  const m = (data.metrics && data.metrics.edgesTotal) || { formatted: num(ed.total || 0), hint: '' };
  const buckets = o.created_over_time || [];
  let run = 0;
  const cumulative = buckets.map((b) => { run += (b.count || 0); return run; });
  const spark = cumulative.length >= 2 ? Spark(cumulative, { w: 150, h: 30 }) : '';
  return Num({ value: m.formatted, unit: 'links', spark, hint: m.hint, emphasis: false });
};

/** RETIRED — count + share of everything ever asserted + the three
 *  most recently retired links, each with the evidence it carried.
 *  The churn rows are truth-now (most recent retirements on record),
 *  regardless of where the belief caret sits. */
const RetiredPanel = ({ data }) => {
  const o = data.overview || {};
  const sb = data.sb || {};
  const ed = o.edges || { total: 0, invalidated: 0 };
  const m = (data.metrics && data.metrics.invalidated) || { formatted: num(ed.invalidated || 0), hint: '' };
  const share = ed.total > 0 ? `${((100 * ed.invalidated) / ed.total).toFixed(ed.invalidated ? 1 : 0)}% of all links` : '';
  const names = sb.names || {};
  const recent = Array.isArray(sb.recentRetired) ? sb.recentRetired : [];
  const evSnippet = (e) => {
    const kind = evidenceKind(e);
    if (kind === 'quote') return `“${e.evidence.quote}”`;
    if (kind === 'rationale') return `WHY — ${e.evidence.quote}`;
    return 'no evidence recorded';
  };
  const rows = recent.map((e) => `
    <button type="button" class="sb-retrow" data-sb-focus="${esc(e.src)}"
            title="Open the ledger for ${esc(e.src)}">
      <span class="sb-retpair">${esc(nodeName(e.src, { names }))} → ${esc(nodeName(e.dst, { names }))}</span>
      <span class="sb-retwhen">retired ${esc(fmtDay(e.invalid_at))}</span>
      <span class="sb-retev">${esc(evSnippet(e))}</span>
    </button>`).join('');
  return `
  ${Num({ value: m.formatted, unit: 'links', hint: m.hint, emphasis: false })}
  ${share ? `<div class="hint" style="margin-top:6px;">${esc(share)} · never deleted, only retired</div>` : ''}
  ${rows ? `<div class="sb-retrows">${rows}</div>` : ''}`;
};

/** WHAT TAUGHT THIS BRAIN — the detectors as identity + magnitude
 *  rows (name · count · 1px bar), not a bare count. */
const TaughtByPanel = ({ data }) => {
  const o = data.overview || {};
  const dets = o.detectors || [];
  if (!dets.length) return `<div class="panel-empty mono faint">NO DETECTOR HAS RUN YET</div>`;
  const shown = dets.slice(0, 4);
  const max = Math.max(...shown.map((d) => d.live), 1);
  const rows = shown.map((d) => `
    <div class="sb-det">
      <span class="sb-detname" title="deterministic detector ${esc(d.detector)} — same folder in, same links out">${esc(detectorName(d.detector))}</span>
      <span class="sb-detn">${esc(num(d.live))}</span>
      <svg class="chart sb-hubbar" height="6" viewBox="0 0 100 6" preserveAspectRatio="none" aria-hidden="true">
        <line x1="0" y1="5" x2="${((d.live / max) * 100).toFixed(1)}" y2="5" stroke="currentColor" stroke-width="1"/>
      </svg>
    </div>`).join('');
  const notListed = (o.not_shown && o.not_shown.detectors_not_listed) || 0;
  const tail = (dets.length > 4 || notListed > 0)
    ? `<div class="hint">+ ${num(Math.max(0, dets.length - 4) + notListed)} more</div>` : '';
  // Authored vs structural, always stated: a corpus with no written
  // links must read "0 authored · N structural", never a proud N.
  const split = authoredSplit(dets);
  return `<div class="sb-dets">${rows}${tail}</div>
  <div class="hint" style="margin-top:6px;">${esc(splitLabel(split))}</div>
  <div class="hint">deterministic · versioned · no LLM</div>`;
};

const TypeDistPanel = ({ data }) => {
  const o = data.overview || {};
  const types = o.types || [];
  if (!types.length) return `<div class="panel-empty mono faint">NO LINKS AT THIS MOMENT</div>`;
  const tail = o.not_shown && o.not_shown.types_not_listed > 0
    ? `<div class="hint">+ ${num(o.not_shown.types_not_listed)} kinds beyond this list</div>` : '';
  // The same authored/structural honesty as the detectors tile: how
  // much of this distribution is written citation vs folder position.
  const split = authoredSplit(o.detectors);
  const splitNote = `<div class="hint" style="margin-top:6px;">${esc(splitLabel(split))} — authored links are written citations; structural ones are folder position and sort order</div>`;
  return Dist({ segments: types.map((t) => ({ label: typeName(t.type), value: t.live })) }) + tail + splitNote;
};

const TimelinePanel = ({ data }) => {
  const o = data.overview || {};
  const series = (data.series && data.series.created) || [];
  const ts = (data.series && data.series.createdT) || [];
  // §0 invariant 8: the default embedder is lexical feature-hashing; any
  // surface that could imply neural semantics must say otherwise.
  const embedderNote = o.embedder === 'lexical-feature-hash'
    ? `<div class="hint" style="margin-top:var(--sp-1);">recall is lexical (feature hashing) — not neural</div>`
    : (o.embedder ? `<div class="hint" style="margin-top:var(--sp-1);">embedder · ${esc(o.embedder)}</div>` : '');
  if (series.length < 2) {
    const one = series.length === 1
      ? `<div class="mono">${esc(num(series[0]))} link${series[0] === 1 ? '' : 's'} · all asserted ${esc(fmtDay(ts[0]))}</div>` : '';
    return `${one || '<div class="panel-empty mono faint">EACH DAY’S NEW LINKS WILL DRAW HERE</div>'}${embedderNote}`;
  }
  return Series(series, {
    h: 120,
    labels: [fmtDay(ts[0]), fmtDay(ts[ts.length - 1])],
    unit: 'links',
  }) + embedderNote;
};

// ----- empty / error / disconnected states -------------------------

/**
 * The dashboard reads the LIVE graph endpoints only — no demo numbers.
 * Anything else on this page would be a fake brain.
 */
const LiveOnlyNote = () => `
  <div class="sb-empty">
    <div class="mono">THIS PAGE READS THE LIVE ENGINE · IT NEVER INVENTS A BRAIN</div>
    <div class="mono faint" style="margin-top:var(--sp-1);">start xerj, point the console at it (SETTINGS → BACKEND · XERJ), reload</div>
  </div>`;

/** Overview failed on a real brain: say so — never render zeros as truth. */
const BrainErrorNote = ({ brain, reason }) => `
  <div class="sb-empty">
    <div class="sb-ftitle">COULD NOT READ THIS BRAIN</div>
    <p class="sb-ftext">The engine answered, but the graph overview for
    <span class="mono">${esc(brain || '—')}</span> failed. Nothing on this page is a guess —
    so nothing is shown.</p>
    <div class="mono faint" style="margin:var(--sp-1) 0;">${esc(String(reason || 'unknown error').slice(0, 140))}</div>
    <button type="button" class="text-btn" data-sb-retry>RETRY</button>
  </div>`;

/** A 401/403 from the brain list is a REFUSAL, not an outage: on an
 *  auth-enabled engine a console session cannot read `.xerj-memory-*`
 *  (issue #936). Say that, and point at the views that do work. */
export const isRefusal = (error) => /HTTP 40[13]\b/.test(String(error || ''));

/** A brand-new brain: the ONE command that fills it, copyable. */
export const EmptyBrainNote = ({ brain, connected, error }) => {
  if (!connected && isRefusal(error)) {
    return `
  <div class="sb-empty">
    <div class="sb-ftitle">THE GRAPH API REFUSED THIS CONSOLE SESSION</div>
    <div class="mono faint" style="margin:var(--sp-1) 0;">${esc(String(error).slice(0, 120))}</div>
    <p class="sb-ftext">On an auth-enabled engine the brain views need an engine API key, and a
    passkey session is not one (issue #936). Nothing here is a guess, so nothing is shown.
    What was indexed is on CORPUS, and any record opens in the READER — those go through the
    session and work. On a node started with <span class="mono">--insecure</span> this page works.</p>
    <a class="text-btn" href="#/corpus">OPEN CORPUS</a>
  </div>`;
  }
  if (!connected) {
    return `
  <div class="sb-empty">
    <div class="sb-ftitle">ENGINE UNREACHABLE</div>
    ${error ? `<div class="mono faint" style="margin:var(--sp-1) 0;">${esc(String(error).slice(0, 120))}</div>` : ''}
    <p class="sb-ftext">This page reads the live graph API only — it never shows demo data.
    Start xerj, then reload:</p>
    <div class="sb-cmdrow"><pre class="sb-cmd mono" data-sb-cmd>xerj</pre><button type="button" class="text-btn" data-sb-copy>COPY</button></div>
  </div>`;
  }
  return `
  <div class="sb-empty">
    <div class="sb-ftitle">THIS BRAIN IS EMPTY — ONE COMMAND FILLS IT</div>
    <p class="sb-ftext">Point it at a folder of notes, docs or exports. Every wiki-link,
    relative link, reading sequence and folder neighborhood becomes a link that
    remembers the exact quote that taught it — then this page turns into a ledger
    of what your notes believe, replayable at any moment in time.</p>
    <div class="sb-cmdrow"><pre class="sb-cmd mono" data-sb-cmd>xerj brain ~/your-folder</pre><button type="button" class="text-btn" data-sb-copy>COPY</button></div>
    <p class="sb-ftext">The folder's name becomes the brain's name here. Detection is
    deterministic — the same folder always produces the same brain, and re-running
    only picks up what changed. Agents can assert links of their own through the
    graph API once there is something to build on.</p>
    <div class="mono faint">no upload · runs local · nothing is ever hard-deleted</div>
  </div>`;
};

// ----- composition panels: controls / map / scrub -------------------

/**
 * The header strip — brain switcher, trail crumbs, FIND — hoisted out
 * of the ledger so it sits at the top of the page and survives no
 * matter which panel below is empty or broken.
 */
const ControlsPanel = ({ data }) => {
  const sb = data.sb || {};
  if (!data.brain) {
    return `<div class="panel-empty mono faint">NO BRAIN ON THIS ENGINE YET · THE LEDGER PANEL BELOW HAS THE ONE COMMAND THAT MAKES ONE</div>`;
  }
  const nodes = (data.ego && data.ego.nodes) || {};
  return EgoControls({
    brain: data.brain, brains: sb.brains, focus: sb.focus,
    asOf: sb.asOf ?? null, trail: sb.trail, names: sb.names || {}, nodes,
  });
};

/**
 * THE MAP panel body — a MOUNT POINT, not the map itself.
 *
 * Seam contract with ux/brain-map.js (built as its own slice):
 *   - this renderer draws `[data-sb-map] > [data-sb-map-mount]` plus
 *     the permanent structural-honesty disclosure;
 *   - data/second-brain-api.js dynamic-imports ux/brain-map.js and
 *     calls its `mountBrainMap()` (idempotent) after every render;
 *   - once mounted, the module sets `data-sb-map-live` on
 *     `[data-sb-map]`; panel patching then SKIPS this body (so the
 *     canvas is never destroyed) and calls `syncBrainMap()` instead;
 *   - the module may import { sbRefocus, sbLogRead, sbPublishMapStats,
 *     sbSnapshot } from data/second-brain-api.js, and hears belief-
 *     time moves as `sb:asof-preview` / `sb:asof-commit` document
 *     events (detail.ms — epoch-ms, or null for NOW).
 */
const MapPanel = ({ data }) => {
  const hasEdges = !!(data.overview && data.overview.exists !== false
    && data.overview.edges && data.overview.edges.total > 0);
  const inner = hasEdges
    ? `<div class="panel-empty mono faint" data-sb-map-waiting>THE MAP DRAWS HERE</div>`
    : `<div class="panel-empty mono faint">THE MAP DRAWS ONCE THIS BRAIN HAS LINKS</div>`;
  return `
  <div class="sb-map" data-sb-map>
    <div class="sb-map-mount" data-sb-map-mount>${inner}</div>
    <div class="sb-map-disclosure mono faint">GROUPED BY LINK STRUCTURE — CITATIONS, FOLDER NEIGHBORHOOD AND SHARED WORDING, NOT MEANING</div>
  </div>`;
};

/** The shared belief-time scrubber: ONE caret for the whole page — the
 *  map restyles and the ledger rows restate from the same drag. */
const ScrubPanel = ({ data }) => {
  const sb = data.sb || {};
  return BeliefScrubber({ timeline: sb.timeline, asOf: sb.asOf ?? null, shift: sb.lastShift });
};

// ----- statistics row -----------------------------------------------
//
// Every number in these panels comes from a call THIS page made and can
// name. What the engine does not record (per-note read counts, other
// callers' whys, semantic similarity) is refused in as many words —
// never approximated.

/** ax_format values → short honest labels. Raw-ish on purpose: the
 *  format tag is the detector's truth, not marketing copy. */
const fmtFormat = (f) => String(f || 'unknown').replace(/[_-]+/g, ' ').toUpperCase();

/** 1px identity+magnitude row, TaughtByPanel style. */
const statRow = (label, count, max, title) => `
  <div class="sb-det">
    <span class="sb-detname"${title ? ` title="${esc(title)}"` : ''}>${esc(label)}</span>
    <span class="sb-detn">${esc(num(count))}</span>
    <svg class="chart sb-hubbar" height="6" viewBox="0 0 100 6" preserveAspectRatio="none" aria-hidden="true">
      <line x1="0" y1="5" x2="${((count / Math.max(max, 1)) * 100).toFixed(1)}" y2="5" stroke="currentColor" stroke-width="1"/>
    </svg>
  </div>`;

/**
 * WHAT THIS BRAIN HOLDS — notes total + by file type + the no-links
 * floor. Total is the size-0 tally this page ran over exactly the
 * indices whose file types are listed below it (so the headline can
 * never contradict its own rows); the overview's nodes.total is the
 * fallback when that tally failed. Both are numbers something
 * returned, never an estimate. (Live-verified 2026-07-30: on a
 * multi-index brain the overview field returns 0 while the same
 * engine counts 634 by search — preferring the tally is what keeps
 * this tile truthful until that server field handles index lists.)
 */
const NotesPanel = ({ data }) => {
  const o = data.overview || {};
  const sb = data.sb || {};
  const st = sb.nodeStats || null;
  // overview.nodes.total is live-verified to be a false 0 when the
  // brain spans several notes indices (comma-list) — only fall back to
  // it for single-index brains.
  const singleIndex = !!(o.nodes_index && !String(o.nodes_index).includes(','));
  const total = (st && Number.isFinite(st.total)) ? st.total
    : (singleIndex && o.nodes && Number.isFinite(o.nodes.total) ? o.nodes.total : null);
  if (total == null) {
    return st && st.error
      ? `<div class="panel-empty mono faint">COULD NOT COUNT THE NOTES · ${esc(String(st.error).slice(0, 60))}</div>`
      : `<div class="panel-empty mono faint">NOT COUNTED YET</div>`;
  }
  const formats = (st && st.formats) || [];
  const max = Math.max(...formats.map((f) => f.count), 1);
  const rows = formats.map((f) => statRow(fmtFormat(f.format), f.count, max)).join('');
  const other = st && st.otherFormats > 0
    ? `<div class="hint">+ ${num(st.otherFormats)} notes in file types beyond this list</div>` : '';
  // Notes with no links: the map computes it (distinct linked notes out
  // of its weight-ranked fetch), so it is a floor whenever that fetch
  // was budget-truncated — say which.
  const ms = sb.mapStats || null;
  const orphans = ms && Number.isFinite(ms.orphansFloor)
    ? `<div class="hint" style="margin-top:6px;">notes with no links: ${ms.orphansIsFloor ? '≥ ' : ''}${num(ms.orphansFloor)}${ms.orphansIsFloor ? ' — a floor; the map read the strongest links, not all of them' : ''}</div>`
    : `<div class="hint" style="margin-top:6px;">notes with no links: counted once the map has read the links</div>`;
  return `
  ${Num({ value: num(total), unit: 'notes', hint: 'each file counts once, and its sections count as their own notes', emphasis: false })}
  <div class="sb-dets" style="margin-top:var(--sp-1);">${rows}</div>
  ${other}${orphans}`;
};

/**
 * LINKS ACROSS FILE TYPES — how many links leave one file type for
 * another (md → rs, md → pdf …), from a file-type tally on the links
 * index. Brains indexed before links carried file-type stamps have
 * nothing to tally — that is a fact about the record, said plainly,
 * never guessed around.
 */
const CrossingsPanel = ({ data }) => {
  const o = data.overview || {};
  const sb = data.sb || {};
  const c = sb.crossings || null;
  // §0 invariant 8: no surface may imply the links mean anything —
  // they are citations and structure found by deterministic detectors.
  const embed = o.embedder
    ? `<div class="hint" style="margin-top:var(--sp-1);">LINKS COME FROM STRUCTURE, CITATIONS AND SHARED WORDING, NOT MEANING — embedder: ${esc(o.embedder)}</div>`
    : '';
  if (!c) return `<div class="panel-empty mono faint">NOT TALLIED YET</div>${embed}`;
  if (c.error) return `<div class="panel-empty mono faint">COULD NOT TALLY · ${esc(String(c.error).slice(0, 60))}</div>${embed}`;
  if (!c.total) {
    return `
  <div class="panel-empty mono faint">REINDEX TO SEE FILE-TYPE CROSSINGS</div>
  <div class="hint">links in this brain carry no file-type stamp on their ends — run <span class="mono">xerj brain</span> on the folder again to stamp them</div>${embed}`;
  }
  // The headline of this panel is CROSSINGS — so the list shows the
  // pairs that actually cross (md → rs, html → pdf …). Same-type pairs
  // dominate every real corpus and would drown the 8 rows this tile
  // can hold; they get one summary line instead of the whole list.
  const crossPairs = c.pairs.filter((p) => p.src !== p.dst).slice(0, 8);
  const max = Math.max(...crossPairs.map((p) => p.count), 1);
  const rows = crossPairs.map((p) => statRow(
    `${fmtFormat(p.src)} → ${fmtFormat(p.dst)}`, p.count, max,
    'links that cross from one file type to another',
  )).join('');
  const beyondCross = c.pairs.filter((p) => p.src !== p.dst).length - crossPairs.length + (c.otherPairs || 0);
  const floor = c.truncated ? '≥ ' : '';
  const tail = beyondCross > 0 ? `<div class="hint">+ ${num(beyondCross)} more pairings beyond this list</div>` : '';
  const same = c.sameCount > 0
    ? `<div class="hint">${floor}${num(c.sameCount)} more links join files of the SAME type — not listed here</div>` : '';
  return `
  ${Num({ value: `${floor}${num(c.crossCount)}`, unit: 'crossings', hint: `links joining two DIFFERENT file types · ${num(c.total)} stamped links tallied`, emphasis: false })}
  <div class="sb-dets" style="margin-top:var(--sp-1);">${rows}</div>
  ${tail}${same}${embed}`;
};

/** HH:MM:SS (UTC) for the read log — the strip's fmtTs stops at minutes. */
const fmtClock = (ms) => new Date(Number(ms)).toISOString().slice(11, 19);

/**
 * WHAT THIS VIEW READ · AND WHY — the dashboard's own attributed fetch
 * log (every call it made: what, how much, why), then the server's
 * per-index search counters. The log is the ONLY honest answer to
 * "which data was read and why": the engine does not record other
 * callers' intentions, and this panel says so instead of inventing.
 */
const ReadsPanel = ({ data }) => {
  const sb = data.sb || {};
  const reads = sb.reads || [];
  const latest = reads.slice(-10).reverse();
  const logRows = latest.map((r) => `
    <div class="sb-readrow">
      <span class="sb-readwhat">${esc(r.surface)}</span>
      <span class="sb-readhow">${esc(r.detail)}</span>
      <span class="sb-readwhy">${esc(r.reason)}</span>
      <span class="sb-readat">${r.n > 1 ? `×${num(r.n)} · ` : ''}${esc(fmtClock(r.t))}</span>
    </div>`).join('');
  const logCap = reads.length > latest.length
    ? `<div class="hint">latest ${num(latest.length)} of ${num(sb.readsTotal || reads.length)} reads this page made this session</div>`
    : (reads.length ? `<div class="hint">${num(sb.readsTotal || reads.length)} read${(sb.readsTotal || reads.length) === 1 ? '' : 's'} this page made this session</div>` : '');
  const log = reads.length
    ? `${logRows}${logCap}`
    : `<div class="panel-empty mono faint">EVERY CALL THIS PAGE MAKES LOGS ITSELF HERE</div>`;

  // The reserved links index renders in user words (raw name on
  // hover) — the LANGUAGE RULE covers identifiers we can translate.
  const idxLabel = (ix) => {
    const m = String(ix).match(/^\.xerj-memory-(.+)-edges$/);
    return m ? `links of brain “${m[1]}”` : String(ix);
  };
  const srv = sb.serverReads || null;
  let srvRows;
  if (!srv) srvRows = `<div class="panel-empty mono faint">NOT READ YET</div>`;
  else if (srv.error) srvRows = `<div class="panel-empty mono faint">SERVER COUNTERS UNREADABLE · ${esc(String(srv.error).slice(0, 40))}</div>`;
  else if (!srv.rows.length) srvRows = `<div class="panel-empty mono faint">NO SEARCHES COUNTED YET</div>`;
  else {
    const top = srv.rows.slice(0, 6);
    const max = Math.max(...top.map((r) => r.count), 1);
    srvRows = top.map((r) => statRow(idxLabel(r.index), r.count, max, `index: ${r.index}`)).join('')
      + (srv.rows.length > 6 ? `<div class="hint">+ ${num(srv.rows.length - 6)} more indices</div>` : '');
  }
  return `
  <div class="sb-reads">
    <div class="key">THIS PAGE'S OWN READS · WHAT — HOW MUCH — WHY</div>
    ${log}
    <div class="key" style="margin-top:var(--sp-2);">SEARCHES PER INDEX · SINCE SERVER START (COUNTERS RESET AT BOOT)</div>
    ${srvRows}
    <div class="hint" style="margin-top:6px;">who else read what, and why, is not recorded by the engine — showing it would be invention</div>
  </div>`;
};

// ----- panel dispatcher ---------------------------------------------
//
// One function renders the INNER body of every second-brain panel so
// the interaction controller (data/second-brain-api.js) can re-render
// any panel in place after a refocus / as-of commit without the app's
// whole-page render loop.

export function renderPanelBody(id, data) {
  if (!data || !data.sb) return LiveOnlyNote();
  const sb = data.sb;
  const hasBrain = !!(data.overview && data.overview.exists !== false);
  const hasEdges = hasBrain && data.overview.edges && data.overview.edges.total > 0;
  const broken = !!sb.overviewError && sb.connected; // engine up, brain unreadable

  if (broken) {
    // An error is not an empty brain: never render teaching copy or
    // zero-metrics over a failed read.
    switch (id) {
      case 'ego': return BrainErrorNote({ brain: data.brain, reason: sb.overviewError });
      case 'notShown': return HonestyList({ data });
      // The way OUT of a dead end is switching brains — keep the
      // controls up; the read log stays honest about what was tried.
      case 'controls': return ControlsPanel({ data });
      case 'reads': return ReadsPanel({ data });
      default: return `<div class="panel-empty mono faint">COULD NOT READ THIS BRAIN · SEE THE LEDGER PANEL</div>`;
    }
  }

  // Engine down ≠ empty brain: peripheral panels defer to the ledger
  // panel's ENGINE UNREACHABLE state instead of teaching fill-me copy.
  const offline = !sb.connected;
  const offNote = `<div class="panel-empty mono faint">${isRefusal(sb.error) ? 'GRAPH API REFUSED THIS SESSION' : 'ENGINE UNREACHABLE'} · SEE THE LEDGER PANEL</div>`;

  switch (id) {
    case 'controls':
      if (offline) return offNote;
      return ControlsPanel({ data });
    case 'map':
      if (offline) return offNote;
      return hasBrain ? MapPanel({ data }) : `<div class="panel-empty mono faint">THE MAP DRAWS ONCE A BRAIN EXISTS · SEE THE LEDGER PANEL BELOW</div>`;
    case 'scrub':
      if (offline) return offNote;
      return hasEdges ? ScrubPanel({ data }) : `<div class="panel-empty mono faint">NOTHING TO REPLAY YET · EVERY LINK WILL DRAW ITS LIFETIME HERE</div>`;
    case 'notes':
      if (offline) return offNote;
      return hasBrain ? NotesPanel({ data }) : `<div class="panel-empty mono faint">—</div>`;
    case 'crossings':
      if (offline) return offNote;
      return hasEdges ? CrossingsPanel({ data }) : `<div class="panel-empty mono faint">HOW LINKS CROSS FILE TYPES RANKS HERE · MD → RS, MD → PDF…</div>`;
    case 'reads':
      // Renders even offline — failed calls were still reads this page
      // made, and the log is the record of trying.
      return ReadsPanel({ data });
    case 'edgesLive':
      if (offline) return offNote;
      return hasBrain ? BelievedPanel({ data }) : `<div class="panel-empty mono faint">NO BRAIN YET · SEE BELOW</div>`;
    case 'edgesTotal':
      return hasBrain ? AssertedPanel({ data }) : `<div class="panel-empty mono faint">—</div>`;
    case 'invalidated':
      return hasBrain ? RetiredPanel({ data }) : `<div class="panel-empty mono faint">—</div>`;
    case 'detectors':
      return hasBrain ? TaughtByPanel({ data }) : `<div class="panel-empty mono faint">—</div>`;
    case 'typeDist':
      if (offline) return offNote;
      return hasEdges ? TypeDistPanel({ data }) : `<div class="panel-empty mono faint">LINK KINDS RANK HERE · WIKI-LINKS, FOLDER NEIGHBORS, READING ORDER…</div>`;
    case 'edgeTimeline':
      if (offline) return offNote;
      return hasEdges ? TimelinePanel({ data }) : `<div class="panel-empty mono faint">EACH DAY’S NEW LINKS WILL DRAW HERE</div>`;
    case 'ego':
      // An empty brain is not a dead end: the controls panel at the top
      // keeps the brain switcher up; this panel teaches the one command.
      if (hasEdges) return EgoLedger({ data });
      return EmptyBrainNote({ brain: data.brain, connected: sb.connected, error: sb.error });
    case 'hubs':
      if (offline) return offNote;
      return hasEdges ? HubsPanel({ data }) : `<div class="panel-empty mono faint">YOUR MOST-CITED NOTES WILL RANK HERE</div>`;
    case 'notShown':
      // Always renders — zeros prove absence.
      return HonestyList({ data });
    default:
      return `<div class="panel-empty mono faint">UNKNOWN PANEL · ${esc(id)}</div>`;
  }
}

// ----- post-paint trace drawing -------------------------------------

const crisp = (v) => Math.round(v) + 0.5;

/**
 * Draw the 1px orthogonal traces from the focus card to each group
 * header. Runs against the live DOM because the endpoints are text
 * blocks whose heights depend on final layout — this is a measure +
 * draw pass, not a simulation. Same data + same viewport ⇒ same
 * traces (deterministic by construction).
 */
export function drawLedgerTraces(scope) {
  if (typeof document === 'undefined') return;
  const root = scope || document;
  const ledger = root.querySelector('[data-sb-ledger]');
  if (!ledger) return;
  const card = ledger.querySelector('[data-sb-focuscard]');
  if (!card) return;
  const cardRect = card.getBoundingClientRect();
  for (const side of ['in', 'out']) {
    const rail = ledger.querySelector(`[data-sb-rail="${side}"]`);
    if (!rail) continue;
    const rr = rail.getBoundingClientRect();
    if (rr.width < 8 || rr.height < 8) { rail.innerHTML = ''; continue; }
    const W = rr.width;
    const H = rr.height;
    const fy = crisp(Math.max(6, Math.min(H - 6, cardRect.top + cardRect.height / 2 - rr.top)));
    const midX = crisp(W / 2);
    let d = '';
    for (const head of ledger.querySelectorAll(`[data-sb-grouphead="${side}"]`)) {
      const hr = head.getBoundingClientRect();
      const hy = crisp(Math.max(2, Math.min(H - 2, hr.top + hr.height / 2 - rr.top)));
      // Left rail: header edge at x=0, focus edge at x=W. Right rail mirrored.
      d += side === 'in'
        ? `M0 ${hy} H${midX} V${fy} H${W} `
        : `M${W} ${hy} H${midX} V${fy} H0 `;
    }
    rail.innerHTML = d
      ? `<svg class="sb-trace" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" aria-hidden="true"><path d="${d}" fill="none" stroke="currentColor" stroke-width="1"/></svg>`
      : '';
  }
}
