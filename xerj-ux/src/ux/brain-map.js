// ============================================================
// XERJ.ai — SECOND BRAIN · THE MAP — canvas view + controller
//
// The all-in-one picture above the ego ledger. The ledger's anti-
// hairball rationale (ego-ledger.js header) is answered, not ignored:
//
//   - node count is bounded BY CONSTRUCTION (≤ 13 grouped bodies at
//     helicopter altitude; worst case ever drawn = 13 bodies + 150
//     expanded members + 1 satellite), never by corpus luck;
//   - there is NO live simulation: layout is a frozen pure function of
//     the link set (brain-map-pipeline.js), same brain → same picture;
//   - the map is a navigation instrument: every terminal click lands
//     in the ledger below, which stays the only evidence surface.
//
// Altitudes: helicopter (grouped bodies) → ONE expanded group at a
// time (golden-angle spiral of members) → the ledger (leaf). Depth
// beyond that is explored by repeated 2-hop walks — the engine caps
// each walk at 2 hops and this view says so, never pretending
// variable-depth traversal.
//
// SEAM (owned by data/second-brain-api.js, honoured here exactly):
//   exports  mountBrainMap() — idempotent; draws into
//            `[data-sb-map-mount]`, sets `data-sb-map-live`
//            syncBrainMap()  — restyle from state after a panel patch
//   imports  sbRefocus / sbLogRead / sbPublishMapStats / sbSnapshot
//   hears    `sb:asof-preview` / `sb:asof-commit` document events
//            (detail.ms = epoch-ms | null for NOW) — ONE belief caret
//            drives the whole page.
//
// LANGUAGE RULE: user words only — link / believed / retired / what
// taught this. HONESTY RULE: every cap is stated in place; every
// number on the rail comes from a call this view made and logged.
//
// House chart law: 1px strokes + text only — a grouped body is a TEXT
// BLOCK (name + count line), never a filled bubble.
// ============================================================

import { esc, num } from './text.js';
import { typeName, detectorName, evidenceKind, displayId } from './ego-ledger.js';
import { sbRefocus, sbLogRead, sbPublishMapStats, sbSnapshot, sbVocabNow } from '../data/second-brain-api.js';
import {
  buildMap, restateAt, memberOrder, bundleLiveAt, phyllotaxis, edgeStateAtI,
  MAP_PAGE, MAP_EDGE_BUDGET, MAP_EXPAND_CAP,
} from './brain-map-pipeline.js';

// Draw caps this view applies on top of the pipeline's (each is said
// on the rail whenever it bites).
const MAP_INTRA_EDGE_DRAW = 600; // member↔member links drawn on expand
const MAP_BUNDLE_INSPECT = 100; // constituent links listed per line
const MAP_SATELLITE_LIST = 100; // satellite members listed in the rail
const MAP_EXPLORE_SEEDS = 64; // starting notes per 2-hop walk (server clamp)

const enc = encodeURIComponent;

// ----- module state -------------------------------------------------

const M = {
  baseUrl: '', brain: null, asOf: null, overview: null,
  status: 'idle', // idle | loading | ready | error
  statusLine: '', error: null,
  gen: 0, // fetch generation guard
  model: null,
  idIndex: null, // note id → interned index
  serverTotal: 0, // hits.total at fetch time (for the budget sentence)
  nodesTotal: null, // note count (overview or own size-0 tally)
  edgeSig: '', // brain + edge counts — reload key
  names: new Map(), // note id → { title, path, format }
  expanded: -1,
  memberDraw: null, // { ci, files, px, py, satellite, satX, satY, total }
  sel: null, // { kind: 'cluster'|'member'|'bundle'|'satellite', ... }
  hover: null,
  cam: { x: 0.5, y: 0.5, s: 1 },
  anim: null,
  spread: 0, // 0 = members folded into the body, 1 = fully spiralled
  previewT: undefined, // mid-drag caret (undefined = none, null = NOW)
  highlightFiles: null, // Set(file idx) from FIND
  highlightMiss: 0, // FIND matches with no links (not on the map)
  explore: null, // { from, hops, reached, reachedFiles, calls, error }
  canvas: null, ctx: null, ro: null,
  drag: null,
  hits: [], // screen-space hit targets, rebuilt every draw
};

// ----- mount + sync (the seam) --------------------------------------

const SHELL = `
  <div class="sb-map-stage" data-sb-map-stage>
    <canvas class="sb-map-canvas" data-sb-map-canvas aria-label="The map — this brain's topics and the links between them, grouped by link structure"></canvas>
    <div class="sb-map-status mono faint" data-sb-map-status>READING THE LINK GRAPH…</div>
    <div class="sb-map-tools mono" data-sb-map-tools>
      <button type="button" class="text-btn" data-sb-map-fit title="Fit the whole map">FIT</button>
      <button type="button" class="text-btn" data-sb-map-collapse hidden title="Fold the open group away">FOLD AWAY</button>
    </div>
  </div>
  <div class="sb-map-rail mono" data-sb-map-rail></div>`;

/** Idempotent: called after every render/patch by the controller.
 *  The controller may call this BEFORE a whole-page re-render commits
 *  its new DOM (render() builds strings first) — so every call also
 *  queues one post-paint re-run, which lands on the final DOM. */
let mountRaf = 0;
export function mountBrainMap() {
  if (typeof document === 'undefined') return;
  if (typeof requestAnimationFrame !== 'undefined' && !mountRaf) {
    mountRaf = requestAnimationFrame(() => { mountRaf = 0; mountNow(); });
  }
  mountNow();
}

function mountNow() {
  const mapEl = document.querySelector('[data-sb-map]');
  const mount = document.querySelector('[data-sb-map-mount]');
  if (!mapEl || !mount) return;
  const snap = sbSnapshot();
  const o = snap.overview && snap.overview.exists !== false ? snap.overview : null;
  const hasEdges = !!(o && o.edges && o.edges.total > 0);
  if (!hasEdges) {
    // The panel body's own honest empty state stays; nothing to mount.
    M.canvas = null;
    mapEl.removeAttribute('data-sb-map-live');
    return;
  }
  if (!mount.querySelector('[data-sb-map-canvas]')) {
    mount.innerHTML = SHELL;
    M.canvas = null; // fresh DOM — remount below
  }
  mapEl.setAttribute('data-sb-map-live', '');
  wireOnce();
  mountCanvas(mount);
  watchFind();
  applySnapshot(snap);
}

/** Restyle from current state after a panel patch (same entry). */
export function syncBrainMap() {
  mountBrainMap();
}

/** Read-only introspection for headless verification (and honest bug
 *  reports): what the map believes it has drawn. Never mutates. */
export function sbMapState() {
  return {
    status: M.status,
    error: M.error,
    statusLine: M.statusLine,
    mode: M.model ? M.model.mode : null,
    fileNodes: M.model ? M.model.fg.F : 0,
    clusters: M.model && M.model.clusters ? M.model.clusters.length : 0,
    bundles: M.model && M.model.bundles ? M.model.bundles.length : 0,
    expanded: M.expanded,
    spread: M.spread,
    sel: M.sel ? { kind: M.sel.kind, ci: M.sel.ci, file: M.sel.file, bi: M.sel.bi, rows: M.sel.rows ? M.sel.rows.length : null } : null,
    hits: M.hits.map((h) => ({ kind: h.kind, ci: h.ci, file: h.file, bi: h.bi, x: h.x, y: h.y, w: h.w, h: h.h, seg: h.seg })),
  };
}

function applySnapshot(snap) {
  M.baseUrl = (snap.baseUrl || M.baseUrl || '').replace(/\/+$/, '');
  M.overview = snap.overview && snap.overview.exists !== false ? snap.overview : null;
  const ed = (M.overview && M.overview.edges) || { total: 0, invalidated: 0 };
  const sig = `${snap.brain}|${ed.total}|${ed.invalidated}`;
  const asOfChanged = (snap.asOf ?? null) !== M.asOf;
  M.brain = snap.brain || null;
  M.asOf = snap.asOf ?? null;

  if (sig !== M.edgeSig) {
    M.edgeSig = sig;
    if (ed.total > 0) load();
    return;
  }
  if (M.status === 'ready' && M.model) {
    if (asOfChanged) {
      M.previewT = undefined;
      restateAt(M.model, M.asOf);
      renderRail();
    }
    scheduleDraw();
  } else if (M.status !== 'loading') {
    setStatus(M.status === 'error'
      ? `COULD NOT READ THE LINK GRAPH · ${String(M.error).slice(0, 80)}`
      : M.statusLine);
    renderRail();
  }
}

// ----- one-time wiring ----------------------------------------------

let wired = false;
function wireOnce() {
  if (wired) return;
  wired = true;
  // ONE belief caret drives the whole page: previews restyle the
  // canvas mid-drag; the commit lands via the controller's re-render
  // (patch → syncBrainMap → restateAt) — positions never move.
  document.addEventListener('sb:asof-preview', (e) => {
    if (!M.model || M.status !== 'ready') return;
    M.previewT = e.detail ? e.detail.ms : null;
    scheduleDraw();
  });
  document.addEventListener('sb:asof-commit', () => {
    M.previewT = undefined;
    scheduleDraw();
  });
  document.addEventListener('click', (e) => {
    const t = e.target;
    if (!t || !t.closest) return;
    if (t.closest('[data-sb-map-fit]')) {
      if (M.expanded >= 0) collapseCluster();
      else { animateTo({ x: 0.5, y: 0.5, s: 1 }, M.spread); }
      return;
    }
    if (t.closest('[data-sb-map-collapse]')) { collapseCluster(); return; }
    if (t.closest('[data-sb-map-explore]')) { exploreStep(); return; }
    const open = t.closest('[data-sb-map-open]');
    if (open) sbRefocus(open.getAttribute('data-sb-map-open'));
  });
}

// ----- FIND → map highlight -----------------------------------------
//
// The FIND strip is owned by the controller; its rendered results
// (`[data-sb-findresults]` rows carrying `data-sb-focus` ids) are the
// contract. A childList observer maps every visible hit onto the map
// and dims everything else; clearing FIND clears the light.

const observedSlots = new WeakSet();
function watchFind() {
  if (typeof MutationObserver === 'undefined') return;
  const slot = document.querySelector('[data-sb-findresults]');
  if (!slot || observedSlots.has(slot)) return;
  observedSlots.add(slot);
  const apply = () => {
    const ids = [...slot.querySelectorAll('[data-sb-focus]')]
      .map((b) => b.getAttribute('data-sb-focus')).filter(Boolean);
    highlightNotes(slot.hidden ? [] : ids);
  };
  M.findApply = apply; // re-run after a late model load (see load())
  new MutationObserver(apply).observe(slot, { childList: true, subtree: true, attributes: true, attributeFilter: ['hidden'] });
  apply(); // results rendered before the map mounted still light up
}

function highlightNotes(ids) {
  if (!M.model || M.status !== 'ready') { M.highlightFiles = null; return; }
  if (!ids || !ids.length) {
    if (!M.highlightFiles) return;
    M.highlightFiles = null; M.highlightMiss = 0;
    scheduleDraw(); renderRail();
    return;
  }
  const { fg } = M.model;
  const files = new Set();
  let miss = 0;
  for (const id of ids) {
    const oi = M.idIndex.get(id);
    if (oi === undefined) { miss += 1; continue; }
    files.add(fg.fileOf[oi]);
  }
  M.highlightFiles = files;
  M.highlightMiss = miss;
  scheduleDraw();
  renderRail();
}

// ----- data plumbing ------------------------------------------------

async function fetchEdgePages(gen) {
  const rows = [];
  let after = null;
  let total = 0;
  let totalIsFloor = false;
  let exhausted = false;
  let pages = 0;
  const idx = enc(`.xerj-memory-${M.brain}-edges`);
  while (rows.length < MAP_EDGE_BUDGET) {
    const body = {
      query: { exists: { field: 'src' } },
      sort: [{ weight: 'desc' }, { edge_id: 'asc' }],
      size: Math.min(MAP_PAGE, MAP_EDGE_BUDGET - rows.length),
      _source: ['src', 'dst', 'type', 'weight', 'valid_at', 'invalid_at', 'src_file'],
    };
    if (after) body.search_after = after;
    const r = await fetch(`${M.baseUrl}/${idx}/_search`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!r.ok) throw new Error(`link list read failed · HTTP ${r.status}`);
    if (gen !== M.gen) return null; // superseded
    const j = await r.json();
    const hits = (j.hits && j.hits.hits) || [];
    if (!pages) {
      // hits.total may itself be a capped floor ('gte') — treat it as
      // one, never as an exact count (live-verified at 10k links).
      const tot = j.hits && j.hits.total;
      total = (tot && typeof tot === 'object' ? tot.value : tot) || hits.length;
      totalIsFloor = !!(tot && typeof tot === 'object' && tot.relation === 'gte');
    }
    pages += 1;
    for (const h of hits) {
      const s = h._source || {};
      rows.push({
        id: h._id, src: s.src, dst: s.dst, type: s.type, weight: s.weight,
        validAt: s.valid_at, invalidAt: s.invalid_at ?? null, srcFile: s.src_file,
      });
    }
    setStatus(`READING EVERY LINK · PAGE ${pages} · ${num(rows.length)} SO FAR`);
    if (hits.length < MAP_PAGE) { exhausted = true; break; }
    after = hits[hits.length - 1].sort;
  }
  sbLogRead('links index', `${num(rows.length)} links · ${pages} page${pages === 1 ? '' : 's'}, strongest first`, 'building the map');
  // capped = the BUDGET stopped us, not the data. The reported total
  // can undercount (floor); the fetched count is a hard lower bound.
  return {
    rows,
    total: Math.max(total, rows.length),
    totalIsFloor: totalIsFloor || (!exhausted && total <= rows.length),
    capped: !exhausted,
  };
}

/** Bounded ids → titles/paths. Best-effort and honest: an id that
 *  resolves nowhere keeps rendering shortened — never guessed. Misses
 *  are cached as empty (so the render→hydrate→render chain terminates
 *  even on ids the notes index has never heard of). Returns how many
 *  ids were newly settled — 0 means a re-render would learn nothing. */
async function hydrateNames(ids, why) {
  const want = [...new Set(ids)].filter((id) => id && !M.names.has(id)).slice(0, 200);
  if (!want.length || !M.overview || !M.overview.nodes_index) return 0;
  try {
    const r = await fetch(`${M.baseUrl}/${enc(M.overview.nodes_index)}/_search`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        query: { ids: { values: want } }, size: want.length,
        _source: ['title', 'text', 'body', 'ax_path', 'ax_format', 'path'],
      }),
    });
    if (!r.ok) return 0;
    const j = await r.json();
    for (const h of (j.hits && j.hits.hits) || []) {
      const s = h._source || {};
      const path = typeof s.ax_path === 'string' ? s.ax_path : (typeof s.path === 'string' ? s.path : null);
      let title = typeof s.title === 'string' && s.title.trim() ? s.title.trim()
        : (typeof s.text === 'string' && s.text.trim() ? s.text.trim().slice(0, 60)
          : (typeof s.body === 'string' && s.body.trim() ? s.body.trim().slice(0, 60) : null));
      if (title) title = title.replace(/^#+\s*/, '');
      M.names.set(h._id, { title, path, format: s.ax_format || null });
    }
    // Negative-cache the misses: rendering falls through to the
    // shortened id, and the next hydrate pass is a true no-op.
    for (const id of want) if (!M.names.has(id)) M.names.set(id, { title: null, path: null, format: null });
    sbLogRead('notes index', `${num(want.length)} names`, why);
    return want.length;
  } catch { return 0; /* names are sugar; the ids remain the truth */ }
}

/** Note total: one size-0 tally over the brain's nodes indices — a
 *  ran number, never a guess. The map's own count outranks
 *  `overview.nodes.total` (fallback only) because the server field is
 *  live-verified to return 0 when `nodes_index` is a comma-list
 *  (multi-dataset brains), and a false total here would print a false
 *  "notes with no links: 0" on two surfaces. */
async function countNotes() {
  const o = M.overview;
  // The overview field is only trusted for single-index brains (it is
  // live-verified to be a false 0 on comma-list brains), and only as a
  // fallback when our own tally cannot run.
  const singleIndex = !!(o && o.nodes_index && !o.nodes_index.includes(','));
  const fallback = singleIndex && o.nodes && Number.isFinite(o.nodes.total) ? o.nodes.total : null;
  if (!o || !o.nodes_index) { M.nodesTotal = fallback; return; }
  try {
    const r = await fetch(`${M.baseUrl}/${enc(o.nodes_index)}/_search`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      // track_total_hits: without it the engine caps the count at a
      // 10,000 'gte' floor — and a FLOOR minus the linked-note count
      // printed "notes with no links: 0" on a 2.1M-note brain
      // (live-caught on `repo`, 2026-07-30). Exact or nothing.
      body: JSON.stringify({ size: 0, track_total_hits: true }),
    });
    if (!r.ok) { M.nodesTotal = fallback; return; }
    const j = await r.json();
    const t = j.hits && j.hits.total;
    const own = t == null ? null : (typeof t === 'object' ? t.value : t);
    const isFloor = !!(t && typeof t === 'object' && t.relation === 'gte');
    // A floor would understate orphans as far as 0 — refuse it; the
    // rail then says "could not be counted" instead of a false number.
    M.nodesTotal = Number.isFinite(own) && !isFloor ? own : fallback;
    if (M.nodesTotal != null) sbLogRead('notes index', 'one size-0 count', 'counting notes with no links');
  } catch { M.nodesTotal = fallback; }
}

function setStatus(text) {
  M.statusLine = text;
  const el = document.querySelector('[data-sb-map-status]');
  if (el) el.textContent = text;
}

// ----- names for bodies ---------------------------------------------

function fileRepId(f) {
  const { g, fg } = M.model;
  return g.ids[fg.fileRep[f]];
}

function fileLabel(f, max = 22) {
  const meta = M.names.get(fileRepId(f));
  let s;
  if (meta && meta.path) s = meta.path.split('/').pop();
  else if (meta && meta.title) s = meta.title;
  else s = displayId(fileRepId(f));
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

function clusterName(ci) {
  const c = M.model.clusters[ci];
  if (c.isElse) return '…and everything else';
  if (c.dirLabel) {
    // Two groups can honestly share a folder name — tell them apart by
    // each one's busiest note (folder name alone said nothing).
    if (c.dirDupe) {
      const meta = M.names.get(fileRepId(c.hubFile));
      const hub = meta && meta.path ? meta.path.split('/').pop() : (meta && meta.title) || null;
      if (hub) return `${c.dirLabel} ${hub.length > 16 ? `${hub.slice(0, 15)}…` : hub}`;
    }
    return c.dirLabel;
  }
  const meta = M.names.get(fileRepId(c.hubFile));
  if (meta && (meta.title || meta.path)) {
    const t = meta.path ? meta.path.split('/').pop() : meta.title;
    return `around: ${t.length > 24 ? `${t.slice(0, 23)}…` : t}`;
  }
  return `around: ${displayId(fileRepId(c.hubFile))}`;
}

// ----- as-of aware counts -------------------------------------------

const caretT = () => (M.previewT !== undefined ? M.previewT : M.asOf);

function clusterLiveAt(ci, t) {
  const { g, fg, clusterOfFile } = M.model;
  let live = 0;
  for (let m = 0; m < fg.fa.length; m++) {
    if (clusterOfFile[fg.fa[m]] !== ci || clusterOfFile[fg.fb[m]] !== ci) continue;
    for (const i of fg.fEdges[m]) if (edgeStateAtI(g, i, t) === 'live') live += 1;
  }
  return live;
}

// ----- load orchestration -------------------------------------------

async function load() {
  const gen = ++M.gen;
  M.status = 'loading';
  M.model = null; M.expanded = -1; M.sel = null; M.memberDraw = null;
  M.explore = null; M.spread = 0; M.cam = { x: 0.5, y: 0.5, s: 1 };
  renderRail();
  try {
    const res = await fetchEdgePages(gen);
    if (!res || gen !== M.gen) return;
    M.serverTotal = res.total;
    M.serverTotalIsFloor = res.totalIsFloor;
    M.capped = res.capped;
    const t0 = performance.now();
    M.model = buildMap(res.rows, { brainName: M.brain, asOf: M.asOf });
    M.idIndex = new Map(M.model.g.ids.map((id, i) => [id, i]));
    const buildMs = performance.now() - t0;
    M.status = 'ready';
    // Names: every document in direct mode; hub nominees when grouped.
    if (M.model.mode === 'direct') {
      const ids = [];
      for (let f = 0; f < M.model.fg.F; f++) ids.push(fileRepId(f));
      await hydrateNames(ids, 'naming the notes you see');
    } else {
      const ids = M.model.clusters.filter((c) => !c.isElse && (!c.dirLabel || c.dirDupe))
        .map((c) => fileRepId(c.hubFile)).slice(0, 26);
      if (ids.length) await hydrateNames(ids, 'naming the groups');
    }
    await countNotes();
    if (gen !== M.gen) return;
    setStatus(M.capped
      ? `MAP BUILT FROM THE ${num(M.model.g.n)} STRONGEST LINKS OF ${M.serverTotalIsFloor ? '≥ ' : ''}${num(M.serverTotal)}`
      : `${num(M.model.g.n)} LINKS · ${num(M.model.fg.F)} DOCUMENTS${M.model.mode === 'clustered' ? ` · GROUPED INTO ${num(M.model.clusters.length)}` : ''} · BUILT IN ${buildMs < 1 ? '<1' : num(Math.round(buildMs))}MS`);
    // Publish what the map actually read/derived for the stats row.
    const orphansFloor = M.nodesTotal != null
      ? Math.max(0, M.nodesTotal - M.model.distinctNotes) : null;
    sbPublishMapStats({
      edgesFetched: M.model.g.n,
      edgesTotal: M.serverTotal,
      edgesTotalIsFloor: M.serverTotalIsFloor,
      linkedNotes: M.model.distinctNotes,
      documents: M.model.fg.F,
      groups: M.model.mode === 'clustered' ? M.model.clusters.length : null,
      mode: M.model.mode,
      ...(orphansFloor != null ? { orphansFloor, orphansIsFloor: M.capped } : {}),
    });
    scheduleDraw();
    renderRail();
    toolsState();
    // FIND results that arrived while links were still loading were
    // dropped by highlightNotes' not-ready guard — apply them now.
    if (M.findApply) M.findApply();
  } catch (e) {
    if (gen !== M.gen) return;
    M.status = 'error';
    M.error = String((e && e.message) || e);
    setStatus(`COULD NOT READ THE LINK GRAPH · ${M.error.slice(0, 80)}`);
    renderRail();
  }
}

// ----- canvas mount / camera ----------------------------------------

function mountCanvas(mount) {
  const canvas = mount.querySelector('[data-sb-map-canvas]');
  if (!canvas || canvas === M.canvas) return;
  M.canvas = canvas;
  M.ctx = canvas.getContext('2d');
  resizeCanvas();
  if (M.ro) M.ro.disconnect();
  if (typeof ResizeObserver !== 'undefined') {
    M.ro = new ResizeObserver(() => { resizeCanvas(); scheduleDraw(); });
    M.ro.observe(canvas.parentElement);
  }
  canvas.addEventListener('pointerdown', onCanvasDown);
  canvas.addEventListener('pointermove', onCanvasMove);
  canvas.addEventListener('pointerup', onCanvasUp);
  canvas.addEventListener('pointerleave', () => { M.hover = null; scheduleDraw(); });
  canvas.addEventListener('wheel', onWheel, { passive: false });
  canvas.style.cursor = 'grab';
  if (M.status === 'ready') { scheduleDraw(); renderRail(); toolsState(); }
  else setStatus(M.statusLine || 'READING THE LINK GRAPH…');
}

function resizeCanvas() {
  const c = M.canvas;
  if (!c || !c.parentElement) return;
  const r = c.parentElement.getBoundingClientRect();
  const dpr = (typeof devicePixelRatio === 'number' && devicePixelRatio) || 1;
  c.width = Math.max(1, Math.round(r.width * dpr));
  c.height = Math.max(1, Math.round(r.height * dpr));
  c.style.width = `${r.width}px`;
  c.style.height = `${r.height}px`;
}

function view() {
  const dpr = (typeof devicePixelRatio === 'number' && devicePixelRatio) || 1;
  const w = M.canvas.width / dpr;
  const h = M.canvas.height / dpr;
  // Per-axis base: the unit world square maps to the WHOLE canvas, not
  // to a centered min(w,h) square — a wide panel earns a wide picture
  // (live-seen: half the canvas empty while labels crowded the middle).
  // Everything drawn in world space is either a cluster position or a
  // member offset pre-divided by these same factors (expandCluster),
  // so member spirals stay circular on screen.
  const bx = w * 0.92;
  const by = h * 0.92;
  return { w, h, bx, by, dpr };
}

function toScreen(x, y, v) {
  return [
    (x - M.cam.x) * v.bx * M.cam.s + v.w / 2,
    (y - M.cam.y) * v.by * M.cam.s + v.h / 2,
  ];
}

function toWorld(sx, sy, v) {
  return [
    (sx - v.w / 2) / (v.bx * M.cam.s) + M.cam.x,
    (sy - v.h / 2) / (v.by * M.cam.s) + M.cam.y,
  ];
}

// ----- drawing ------------------------------------------------------

let drawRaf = 0;
function scheduleDraw() {
  if (typeof requestAnimationFrame === 'undefined') { draw(); return; }
  if (drawRaf) return;
  drawRaf = requestAnimationFrame(() => { drawRaf = 0; draw(); });
}

function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function bodyFont(px, weight = 700) {
  return `${weight} ${px}px ${cssVar('--font-data') || 'monospace'}`;
}

/** Rank → font-size step within the house 7-size scale. */
function rankFontPx(rank) {
  return rank === 0 ? 20 : rank <= 2 ? 16 : rank <= 6 ? 13 : 11;
}

function elbow(ctx, x1, y1, x2, y2) {
  // 1px orthogonal trace — pixel-snapped like the ledger rails.
  const mx = Math.round((x1 + x2) / 2) + 0.5;
  const sy1 = Math.round(y1) + 0.5;
  const sy2 = Math.round(y2) + 0.5;
  ctx.beginPath();
  ctx.moveTo(x1, sy1);
  ctx.lineTo(mx, sy1);
  ctx.lineTo(mx, sy2);
  ctx.lineTo(x2, sy2);
  ctx.stroke();
  return [mx, Math.round((sy1 + sy2) / 2)];
}

function draw() {
  if (!M.canvas || !M.ctx || !M.model || M.status !== 'ready') return;
  if (!M.canvas.isConnected) return; // stale canvas from a replaced render
  const v = view();
  const ctx = M.ctx;
  ctx.setTransform(v.dpr, 0, 0, v.dpr, 0, 0);
  ctx.clearRect(0, 0, v.w, v.h);
  const colors = {
    ink: cssVar('--z-ink') || '#e0e0e0',
    mute: cssVar('--z-mute') || '#555',
    accent: cssVar('--z-accent') || '#eebb00',
    cmp: cssVar('--z-cmp') || '#7f93aa',
  };
  M.hits = [];
  const t = caretT();
  const dimAll = !!M.highlightFiles;

  if (M.model.mode === 'direct') drawDirect(ctx, v, t, colors, dimAll);
  else drawClustered(ctx, v, t, colors, dimAll);

  if (M.anim) stepAnim();
}

function bundleStyle(ctx, live, count, cmax, colors) {
  if (live === 0) {
    ctx.strokeStyle = colors.cmp;
    ctx.setLineDash([3, 2]);
    ctx.globalAlpha = 0.5;
  } else {
    ctx.strokeStyle = colors.ink;
    ctx.setLineDash([]);
    ctx.globalAlpha = 0.18 + 0.55 * (Math.log2(count + 1) / Math.log2(cmax + 1));
  }
  ctx.lineWidth = 1;
}

function labelBlock(ctx, x, y, name, countLine, px, color, alpha, hitRef) {
  ctx.textAlign = 'center';
  ctx.textBaseline = 'alphabetic';
  // Knockout: text owns its pixels — a paper-colored rect under each
  // label keeps the 1px bundle traces from running through the words
  // (the traces are drawn first, so they visibly pass BEHIND a group).
  ctx.font = bodyFont(px);
  const kw1 = ctx.measureText(name).width;
  ctx.font = bodyFont(11, 400);
  const kw = Math.max(kw1, countLine ? ctx.measureText(countLine).width : 0) + 8;
  const kh = (countLine ? 14 : 0) + px + 8;
  ctx.globalAlpha = 1;
  ctx.fillStyle = cssVar('--z-paper') || '#0d0d0d';
  ctx.fillRect(x - kw / 2, y - px - 3, kw, kh);
  ctx.globalAlpha = alpha;
  ctx.fillStyle = color;
  ctx.font = bodyFont(px);
  ctx.fillText(name, x, y);
  const w1 = ctx.measureText(name).width;
  let w2 = 0;
  if (countLine) {
    ctx.font = bodyFont(11, 400);
    ctx.globalAlpha = Math.min(1, alpha * 0.9);
    ctx.fillText(countLine, x, y + 14);
    w2 = ctx.measureText(countLine).width;
  }
  ctx.globalAlpha = 1;
  const w = Math.max(w1, w2) + 10;
  const h = (countLine ? 14 : 0) + px + 8;
  if (hitRef) M.hits.push({ ...hitRef, x: x - w / 2, y: y - px - 4, w, h });
}

/**
 * Label layout for the grouped bodies: measure every text block, then
 * run a fixed number of deterministic pairwise separation passes
 * (vertical push-apart, fixed pair order) so body labels never overlap
 * — same data + same viewport ⇒ same nudges. The expanded group's
 * label moves above its member spiral instead of sitting on it.
 */
function layoutBodyLabels(ctx, v, t) {
  const { clusters, px, py } = M.model;
  const ex = M.expanded;
  const out = clusters.map((c, ci) => {
    let [x, y] = toScreen(px[ci], py[ci], v);
    if (ex === ci && M.memberDraw && M.spread > 0.01) {
      // Above the spiral, clear of the members.
      y -= M.memberDraw.radWorldY * v.by * M.cam.s * M.spread + 26;
    }
    const fpx = c.isElse ? 13 : rankFontPx(ci); // clusters arrive rank-ordered
    const live = M.previewT !== undefined ? clusterLiveAt(ci, t) : c.linkLive;
    const countLine = `${num(c.noteCount)} ${sbVocabNow().items} · ${num(live)} link${live === 1 ? '' : 's'}`;
    const name = ex === ci ? `▾ ${clusterName(ci)}` : clusterName(ci);
    ctx.font = bodyFont(fpx);
    const w1 = ctx.measureText(name).width;
    ctx.font = bodyFont(11, 400);
    const w = Math.max(w1, ctx.measureText(countLine).width) + 12;
    const h = fpx + 14 + 10;
    return { ci, x, y, w, h, fpx, name, countLine };
  });
  for (let pass = 0; pass < 24; pass++) {
    let moved = false;
    for (let i = 0; i < out.length; i++) {
      for (let j = i + 1; j < out.length; j++) {
        const a = out[i];
        const b = out[j];
        const ox = Math.min(a.x + a.w / 2, b.x + b.w / 2) - Math.max(a.x - a.w / 2, b.x - b.w / 2);
        const oy = Math.min(a.y + 10, b.y + 10) - Math.max(a.y - a.h + 10, b.y - b.h + 10);
        if (ox <= 0 || oy <= 0) continue;
        const push = (oy / 2) + 1;
        if (a.y <= b.y) { a.y -= push; b.y += push; } else { a.y += push; b.y -= push; }
        moved = true;
      }
    }
    if (!moved) break;
  }
  // At the default camera no label may clip the canvas edge (a body at
  // world 0.06 could otherwise hang half its name off-screen). Only at
  // rest — a user who pans a label off the edge meant to.
  if (M.cam.s === 1 && M.cam.x === 0.5 && M.cam.y === 0.5) {
    for (const L of out) {
      L.x = Math.max(L.w / 2 + 4, Math.min(v.w - L.w / 2 - 4, L.x));
      L.y = Math.max(L.h + 4, Math.min(v.h - 8, L.y));
    }
  }
  return out;
}

function drawClustered(ctx, v, t, colors, dimAll) {
  const { clusters, bundles } = M.model;
  const cmax = bundles.reduce((a, b) => Math.max(a, b.count), 0) || 1;
  const ex = M.expanded;

  const hlClusters = new Set();
  if (M.highlightFiles) {
    for (const f of M.highlightFiles) hlClusters.add(M.model.clusterOfFile[f]);
  }

  // Nudged label positions first — bundles anchor to the nudged
  // centers so a trace always meets the words it belongs to.
  const labels = layoutBodyLabels(ctx, v, t);
  const cxOf = (ci) => labels[ci].x;
  const cyOf = (ci) => labels[ci].y - labels[ci].fpx / 2;
  const bodyRectsEarly = labels.map((L) => ({ x: L.x - L.w / 2, y: L.y - L.h + 10, w: L.w, h: L.h }));
  const countRects = []; // placed bundle-count labels (collision-skipped)

  // -- bundles between bodies (1px orthogonal elbows) --
  for (let bi = 0; bi < bundles.length; bi++) {
    const bu = bundles[bi];
    const live = bundleLiveAt(M.model.g, M.model.fg, bu, t);
    const x1 = cxOf(bu.a);
    const y1 = cyOf(bu.a);
    const x2 = cxOf(bu.b);
    const y2 = cyOf(bu.b);
    bundleStyle(ctx, live, bu.count, cmax, colors);
    if (dimAll && !(hlClusters.has(bu.a) && hlClusters.has(bu.b))) ctx.globalAlpha *= 0.15;
    const hovered = M.hover && M.hover.kind === 'bundle' && M.hover.bi === bi;
    if (hovered) { ctx.strokeStyle = colors.accent; ctx.globalAlpha = 0.9; }
    const [mx, my] = elbow(ctx, x1, y1, x2, y2);
    ctx.setLineDash([]);
    if (bundles.length <= 20 || hovered) {
      // The count label yields to text that already owns the spot —
      // hover always shows it (drawn last, in accent).
      ctx.font = bodyFont(11, 400);
      const label = `${num(live)} link${live === 1 ? '' : 's'}`;
      const lw = ctx.measureText(label).width;
      const rect = { x: mx - lw / 2, y: my - 15, w: lw, h: 14 };
      const blocked = [...bodyRectsEarly, ...countRects].some((r) =>
        rect.x < r.x + r.w && r.x < rect.x + rect.w && rect.y < r.y + r.h && r.y < rect.y + rect.h);
      if (!blocked || hovered) {
        countRects.push(rect);
        ctx.fillStyle = hovered ? colors.accent : colors.mute;
        ctx.globalAlpha = hovered ? 1 : 0.8;
        ctx.textAlign = 'center';
        ctx.fillText(label, mx, my - 4);
        ctx.globalAlpha = 1;
      }
    }
    M.hits.push({ kind: 'bundle', bi, seg: [x1, y1, x2, y2] });
  }

  // -- expanded members (under the body labels) --
  if (ex >= 0 && M.memberDraw && M.spread > 0.01) drawMembers(ctx, v, t, colors, bodyRectsEarly);

  // -- grouped bodies: TEXT blocks, never bubbles --
  const maxScore = clusters.reduce((a, c) => Math.max(a, c.score), 0) || 1;
  for (const L of labels) {
    const c = clusters[L.ci];
    const selected = M.sel && M.sel.kind === 'cluster' && M.sel.ci === L.ci;
    const hovered = M.hover && M.hover.kind === 'cluster' && M.hover.ci === L.ci;
    let alpha = c.isElse ? 0.75 : 0.35 + 0.65 * Math.sqrt(c.score / maxScore);
    if (dimAll && !hlClusters.has(L.ci)) alpha = 0.15;
    const color = selected || hovered || (dimAll && hlClusters.has(L.ci))
      ? colors.accent
      : c.isElse ? colors.mute : colors.ink;
    labelBlock(ctx, L.x, L.y, L.name, L.countLine, L.fpx, color, alpha, { kind: 'cluster', ci: L.ci });
  }
}

function drawMembers(ctx, v, t, colors, bodyRects) {
  const md = M.memberDraw;
  const { g, fg, clusterOfFile, px, py } = M.model;
  const ci = md.ci;
  const spread = M.spread;
  const [ccx, ccy] = [px[ci], py[ci]];

  const pos = md.files.map((f, i) => toScreen(
    ccx + (md.px[i] - ccx) * spread,
    ccy + (md.py[i] - ccy) * spread, v));
  const fileSlot = new Map();
  md.files.forEach((f, i) => fileSlot.set(f, i));

  // -- member↔member links (strongest first, capped-and-said) --
  const intra = [];
  for (let m = 0; m < fg.fa.length; m++) {
    const sa = fileSlot.get(fg.fa[m]);
    const sb = fileSlot.get(fg.fb[m]);
    if (sa === undefined || sb === undefined) continue;
    intra.push([fg.fWeight[m], sa, sb, m]);
  }
  intra.sort((a, b) => b[0] - a[0] || a[3] - b[3]);
  md.intraTotal = intra.length;
  ctx.lineWidth = 1;
  for (const [, sa, sb, m] of intra.slice(0, MAP_INTRA_EDGE_DRAW)) {
    let live = 0;
    for (const i of fg.fEdges[m]) if (edgeStateAtI(g, i, t) === 'live') live += 1;
    ctx.strokeStyle = live ? colors.ink : colors.cmp;
    ctx.setLineDash(live ? [] : [3, 2]);
    ctx.globalAlpha = 0.16 * spread;
    ctx.beginPath();
    ctx.moveTo(pos[sa][0], pos[sa][1]);
    ctx.lineTo(pos[sb][0], pos[sb][1]);
    ctx.stroke();
  }
  ctx.setLineDash([]);

  // -- external links: member → the OTHER group's body, aggregated --
  const C = M.model.clusters.length;
  const extAgg = new Set();
  for (let m = 0; m < fg.fa.length; m++) {
    const ca = clusterOfFile[fg.fa[m]];
    const cb = clusterOfFile[fg.fb[m]];
    if ((ca === ci) === (cb === ci)) continue;
    const inFile = ca === ci ? fg.fa[m] : fg.fb[m];
    const other = ca === ci ? cb : ca;
    const slot = fileSlot.get(inFile);
    if (slot === undefined || other < 0) continue;
    extAgg.add(slot * C + other);
  }
  ctx.strokeStyle = colors.ink;
  ctx.globalAlpha = 0.22 * spread;
  for (const key of extAgg) {
    const slot = Math.floor(key / C);
    const other = key % C;
    const [ox, oy] = toScreen(px[other], py[other], v);
    elbow(ctx, pos[slot][0], pos[slot][1], ox, oy);
  }
  ctx.globalAlpha = 1;

  // -- member marks + collision-aware labels (text, never dots) --
  // Body labels claim their space first: a member name never prints
  // through a group name.
  const placed = (bodyRects || []).slice();
  const deg = M.model.clusters[ci].degLive || new Map();
  md.files.forEach((f, i) => {
    const [sx, sy] = pos[i];
    const selected = M.sel && M.sel.kind === 'member' && M.sel.file === f;
    const hovered = M.hover && M.hover.kind === 'member' && M.hover.file === f;
    const hl = M.highlightFiles && M.highlightFiles.has(f);
    const reached = M.explore && M.explore.reachedFiles && M.explore.reachedFiles.has(f);
    ctx.strokeStyle = selected || hovered || hl || reached ? colors.accent : colors.ink;
    ctx.globalAlpha = (M.highlightFiles && !hl ? 0.2 : 0.9) * spread;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(sx - 3, sy); ctx.lineTo(sx + 3, sy);
    ctx.moveTo(sx, sy - 3); ctx.lineTo(sx, sy + 3);
    ctx.stroke();
    if (reached) { // explore frontier — a 1px ring stroke
      ctx.beginPath();
      ctx.arc(sx, sy, 6, 0, 2 * Math.PI);
      ctx.stroke();
    }
    M.hits.push({ kind: 'member', file: f, x: sx - 6, y: sy - 6, w: 12, h: 12 });
    if (spread > 0.85) {
      const name = fileLabel(f, 18);
      ctx.font = bodyFont(11, selected || hovered ? 700 : 400);
      const tw = ctx.measureText(name).width;
      const rect = { x: sx + 6, y: sy - 7, w: tw + 4, h: 14 };
      const collides = placed.some((r) =>
        rect.x < r.x + r.w && r.x < rect.x + rect.w && rect.y < r.y + r.h && r.y < rect.y + rect.h);
      if (!collides || selected || hovered) {
        placed.push(rect);
        ctx.fillStyle = selected || hovered || hl || reached ? colors.accent : colors.ink;
        ctx.globalAlpha = M.highlightFiles && !hl ? 0.25 : ((deg.get(f) || 0) > 0 ? 0.95 : 0.6);
        ctx.textAlign = 'left';
        ctx.fillText(name, sx + 6, sy + 4);
        ctx.globalAlpha = 1;
        M.hits.push({ kind: 'member', file: f, ...rect });
      }
    }
  });
  md.labelled = placed.length - (bodyRects ? bodyRects.length : 0);

  // -- the satellite: everything past the expand cap, as text --
  if (md.satellite > 0 && spread > 0.85) {
    const [sx, sy] = toScreen(md.satX, md.satY, v);
    const selected = M.sel && M.sel.kind === 'satellite';
    labelBlock(ctx, sx, sy, `+ ${num(md.satellite)} MORE ${sbVocabNow().Items}`, null, 13,
      selected ? colors.accent : colors.mute, 0.9, { kind: 'satellite' });
  }
  ctx.globalAlpha = 1;
}

function drawDirect(ctx, v, t, colors, dimAll) {
  const { g, fg, px, py } = M.model;
  const cmax = fg.fCount.reduce((a, c) => Math.max(a, c), 0) || 1;
  const degLive = new Map();
  const liveOf = [];
  for (let m = 0; m < fg.fa.length; m++) {
    let live = 0;
    for (const i of fg.fEdges[m]) if (edgeStateAtI(g, i, t) === 'live') live += 1;
    liveOf.push(live);
    if (!live) continue;
    degLive.set(fg.fa[m], (degLive.get(fg.fa[m]) || 0) + live);
    degLive.set(fg.fb[m], (degLive.get(fg.fb[m]) || 0) + live);
  }
  const maxDeg = Math.max(...degLive.values(), 1);
  // Same deterministic separation pass as the grouped view: labels
  // measure themselves, then push apart vertically in fixed order.
  const labels = [];
  for (let f = 0; f < fg.F; f++) {
    const [x, y] = toScreen(px[f], py[f], v);
    const d = degLive.get(f) || 0;
    const fpx = d >= maxDeg * 0.8 ? 16 : d > 0 ? 13 : 11;
    const notes = fg.fileSize[f];
    const countLine = notes > 1
      ? `${num(notes)} ${sbVocabNow().items} · ${num(d)} link${d === 1 ? '' : 's'}`
      : `${num(d)} link${d === 1 ? '' : 's'}`;
    const name = fileLabel(f);
    ctx.font = bodyFont(fpx);
    const w1 = ctx.measureText(name).width;
    ctx.font = bodyFont(11, 400);
    const w = Math.max(w1, ctx.measureText(countLine).width) + 12;
    labels.push({ f, x, y, w, h: fpx + 14 + 10, fpx, name, countLine, d });
  }
  for (let pass = 0; pass < 24; pass++) {
    let moved = false;
    for (let i = 0; i < labels.length; i++) {
      for (let j = i + 1; j < labels.length; j++) {
        const a = labels[i];
        const b = labels[j];
        const ox = Math.min(a.x + a.w / 2, b.x + b.w / 2) - Math.max(a.x - a.w / 2, b.x - b.w / 2);
        const oy = Math.min(a.y + 10, b.y + 10) - Math.max(a.y - a.h + 10, b.y - b.h + 10);
        if (ox <= 0 || oy <= 0) continue;
        const push = (oy / 2) + 1;
        if (a.y <= b.y) { a.y -= push; b.y += push; } else { a.y += push; b.y -= push; }
        moved = true;
      }
    }
    if (!moved) break;
  }
  const cxOf = new Map(labels.map((L) => [L.f, L.x]));
  const cyOf = new Map(labels.map((L) => [L.f, L.y - L.fpx / 2]));
  for (let m = 0; m < fg.fa.length; m++) {
    const x1 = cxOf.get(fg.fa[m]);
    const y1 = cyOf.get(fg.fa[m]);
    const x2 = cxOf.get(fg.fb[m]);
    const y2 = cyOf.get(fg.fb[m]);
    bundleStyle(ctx, liveOf[m], fg.fCount[m], cmax, colors);
    if (dimAll && !(M.highlightFiles.has(fg.fa[m]) || M.highlightFiles.has(fg.fb[m]))) ctx.globalAlpha *= 0.15;
    const hovered = M.hover && M.hover.kind === 'bundle' && M.hover.bi === m;
    if (hovered) { ctx.strokeStyle = colors.accent; ctx.globalAlpha = 0.9; }
    elbow(ctx, x1, y1, x2, y2);
    ctx.setLineDash([]);
    M.hits.push({ kind: 'bundle', bi: m, seg: [x1, y1, x2, y2] });
  }
  for (const L of labels) {
    const selected = M.sel && M.sel.kind === 'member' && M.sel.file === L.f;
    const hovered = M.hover && M.hover.kind === 'member' && M.hover.file === L.f;
    const hl = M.highlightFiles && M.highlightFiles.has(L.f);
    const reached = M.explore && M.explore.reachedFiles && M.explore.reachedFiles.has(L.f);
    let alpha = L.d > 0 ? 0.55 + 0.45 * (L.d / maxDeg) : 0.45;
    if (dimAll && !hl) alpha = 0.15;
    labelBlock(ctx, L.x, L.y, L.name, L.countLine, L.fpx,
      selected || hovered || hl || reached ? colors.accent : colors.ink, alpha, { kind: 'member', file: L.f });
  }
}

// ----- animation (240ms ease-out, positions precomputed) ------------

const easeOut = (u) => 1 - Math.pow(1 - u, 3);

function stepAnim() {
  const a = M.anim;
  if (!a) return;
  const u = Math.min(1, (performance.now() - a.t0) / a.dur);
  const e = easeOut(u);
  M.spread = a.spreadFrom + (a.spreadTo - a.spreadFrom) * e;
  M.cam = {
    x: a.camFrom.x + (a.camTo.x - a.camFrom.x) * e,
    y: a.camFrom.y + (a.camTo.y - a.camFrom.y) * e,
    s: a.camFrom.s + (a.camTo.s - a.camFrom.s) * e,
  };
  if (u >= 1) {
    M.anim = null;
    if (a.then) a.then();
  }
  scheduleDraw();
}

function animateTo(camTo, spreadTo, then) {
  M.anim = {
    t0: performance.now(), dur: 240,
    camFrom: { ...M.cam }, camTo,
    spreadFrom: M.spread, spreadTo,
    then,
  };
  scheduleDraw();
}

// ----- expand / collapse --------------------------------------------

function expandCluster(ci) {
  const model = M.model;
  if (!model || model.mode !== 'clustered' || M.expanded === ci) return;
  const c = model.clusters[ci];
  const order = memberOrder(c);
  const files = order.slice(0, MAP_EXPAND_CAP);
  const satellite = order.length - files.length;
  const v = view();
  // Spiral sized so neighbouring notes sit ~20px apart on screen at
  // the target camera (a Vogel spiral's nearest-neighbour distance ≈
  // its constant): 150 members → ~245px screen radius, always fits.
  const targetS = 2.2;
  // Per-axis world step (the view maps x and y through different
  // scales): dividing a 20px screen step by each axis' own scale keeps
  // the spiral circular on screen.
  const cWx = 20 / (v.bx * targetS);
  const cWy = 20 / (v.by * targetS);
  const cx = model.px[ci];
  const cy = model.py[ci];
  const mpx = [];
  const mpy = [];
  files.forEach((f, i) => {
    const [ox, oy] = phyllotaxis(i + 1, 0, 0, 1);
    mpx.push(cx + ox * cWx); mpy.push(cy + oy * cWy);
  });
  const radY = cWy * Math.sqrt(files.length + 1);
  M.memberDraw = {
    ci, files, px: mpx, py: mpy, satellite,
    satX: cx, satY: cy + radY + cWy * 2,
    total: order.length, radWorldY: radY,
  };
  M.expanded = ci;
  M.sel = { kind: 'cluster', ci };
  M.spread = 0;
  animateTo({ x: cx, y: cy, s: targetS }, 1);
  hydrateNames(files.map(fileRepId), 'naming the notes you spread open')
    .then(() => { scheduleDraw(); renderRail(); });
  toolsState();
  renderRail();
}

function collapseCluster() {
  if (M.expanded < 0) return;
  M.sel = null;
  M.explore = null;
  animateTo({ x: 0.5, y: 0.5, s: 1 }, 0, () => {
    M.expanded = -1;
    M.memberDraw = null;
    renderRail();
    toolsState();
  });
  renderRail();
}

function toolsState() {
  const btn = document.querySelector('[data-sb-map-collapse]');
  if (btn) btn.hidden = M.expanded < 0;
}

// ----- pointer interaction ------------------------------------------

function hitTest(sx, sy) {
  // Text blocks first (topmost), line traces after.
  for (let i = M.hits.length - 1; i >= 0; i--) {
    const h = M.hits[i];
    if (h.seg) continue;
    if (sx >= h.x && sx <= h.x + h.w && sy >= h.y && sy <= h.y + h.h) return h;
  }
  for (let i = M.hits.length - 1; i >= 0; i--) {
    const h = M.hits[i];
    if (!h.seg) continue;
    const [x1, y1, x2, y2] = h.seg;
    const mx = (x1 + x2) / 2;
    if (ptSeg(sx, sy, x1, y1, mx, y1) < 5 || ptSeg(sx, sy, mx, y1, mx, y2) < 5
      || ptSeg(sx, sy, mx, y2, x2, y2) < 5) return h;
  }
  return null;
}

function ptSeg(qx, qy, x1, y1, x2, y2) {
  const dx = x2 - x1;
  const dy = y2 - y1;
  const l2 = dx * dx + dy * dy;
  const u = l2 ? Math.max(0, Math.min(1, ((qx - x1) * dx + (qy - y1) * dy) / l2)) : 0;
  return Math.hypot(qx - (x1 + u * dx), qy - (y1 + u * dy));
}

function canvasXY(e) {
  const r = M.canvas.getBoundingClientRect();
  return [e.clientX - r.left, e.clientY - r.top];
}

function onCanvasDown(e) {
  if (!M.model || M.status !== 'ready') return;
  const [sx, sy] = canvasXY(e);
  const hit = hitTest(sx, sy);
  M.drag = { sx, sy, cam: { ...M.cam }, moved: false, hit };
  if (!hit) {
    try { M.canvas.setPointerCapture(e.pointerId); } catch { /* older engines */ }
  }
}

function onCanvasMove(e) {
  if (!M.model || M.status !== 'ready') return;
  const [sx, sy] = canvasXY(e);
  if (M.drag) {
    const ddx = sx - M.drag.sx;
    const ddy = sy - M.drag.sy;
    if (Math.abs(ddx) + Math.abs(ddy) > 4) M.drag.moved = true;
    if (M.drag.moved && !M.drag.hit) {
      const v = view();
      M.cam.x = M.drag.cam.x - ddx / (v.bx * M.cam.s);
      M.cam.y = M.drag.cam.y - ddy / (v.by * M.cam.s);
      scheduleDraw();
    }
    return;
  }
  const hit = hitTest(sx, sy);
  const key = (h) => (h ? `${h.kind}:${h.ci ?? ''}:${h.file ?? ''}:${h.bi ?? ''}` : '');
  const changed = key(hit) !== key(M.hover);
  M.hover = hit;
  M.canvas.style.cursor = hit ? 'pointer' : 'grab';
  if (changed) scheduleDraw();
}

function onCanvasUp() {
  const d = M.drag;
  M.drag = null;
  if (!d || d.moved) return;
  const hit = d.hit;
  if (!hit) { // background click clears selection, keeps altitude
    M.sel = null;
    renderRail();
    scheduleDraw();
    return;
  }
  if (hit.kind === 'cluster') {
    if (M.expanded === hit.ci) collapseCluster();
    else expandCluster(hit.ci);
    return;
  }
  if (hit.kind === 'member') {
    M.sel = { kind: 'member', file: hit.file };
    M.explore = null;
    hydrateNames([fileRepId(hit.file)], 'naming the note you selected').then(renderRail);
    renderRail();
    scheduleDraw();
    return;
  }
  if (hit.kind === 'satellite') {
    M.sel = { kind: 'satellite' };
    renderRail();
    scheduleDraw();
    return;
  }
  if (hit.kind === 'bundle') {
    M.sel = { kind: 'bundle', bi: hit.bi };
    inspectBundle(hit.bi);
  }
}

function onWheel(e) {
  if (!M.model || M.status !== 'ready') return;
  e.preventDefault();
  const v = view();
  const [sx, sy] = canvasXY(e);
  const [wx, wy] = toWorld(sx, sy, v);
  const s = Math.max(0.4, Math.min(6, M.cam.s * Math.exp(-e.deltaY * 0.0015)));
  M.cam.x = wx - (sx - v.w / 2) / (v.bx * s);
  M.cam.y = wy - (sy - v.h / 2) / (v.by * s);
  M.cam.s = s;
  scheduleDraw();
}

// ----- bundle inspection / explore ----------------------------------

async function inspectBundle(bi) {
  const model = M.model;
  const merged = model.mode === 'direct' ? [bi] : model.bundles[bi].merged;
  const idxs = [];
  for (const m of merged) for (const i of model.fg.fEdges[m]) idxs.push(i);
  idxs.sort((a, b) => (model.g.weight[b] - model.g.weight[a]) || (a - b));
  const top = idxs.slice(0, MAP_BUNDLE_INSPECT);
  M.sel = { kind: 'bundle', bi, total: idxs.length, rows: null };
  renderRail();
  scheduleDraw();
  try {
    const ids = top.map((i) => model.g.edgeIds[i]);
    const r = await fetch(`${M.baseUrl}/${enc(`.xerj-memory-${M.brain}-edges`)}/_search`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        query: { ids: { values: ids } }, size: ids.length,
        _source: ['src', 'dst', 'type', 'detector', 'evidence', 'weight', 'valid_at', 'invalid_at', 'src_file'],
      }),
    });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const j = await r.json();
    sbLogRead('links index', `${num(ids.length)} of ${num(idxs.length)} links, with their evidence`, 'the line you opened');
    const byId = new Map();
    for (const h of (j.hits && j.hits.hits) || []) byId.set(h._id, { edge_id: h._id, ...(h._source || {}) });
    if (M.sel && M.sel.kind === 'bundle' && M.sel.bi === bi) {
      M.sel.rows = ids.map((id) => byId.get(id)).filter(Boolean);
      renderRail();
    }
  } catch (e) {
    if (M.sel && M.sel.kind === 'bundle' && M.sel.bi === bi) {
      M.sel.rows = [];
      M.sel.error = String((e && e.message) || e);
      renderRail();
    }
  }
}

/**
 * Drill-down past the map, honestly: each click is ONE walk of ≤ 2
 * hops from the seeds (the engine's cap — never a depth slider). The
 * next click seeds from what the last walk reached — the iteration the
 * engine's cap prescribes. The HTTP response spells the reached set as
 * `seeds` (admitted) + `neighbors[].id` (everything new — the spec's
 * §4.3 shape; there is no `reachable` field on the wire). Reached
 * notes light up wherever the map draws them.
 */
async function exploreStep() {
  const sel = M.sel;
  if (!sel || sel.kind !== 'member') return;
  const startId = fileRepId(sel.file);
  if (!M.explore) {
    M.explore = {
      from: sel.file, hops: 0, calls: 0, error: null, running: false,
      reached: new Set([startId]), reachedFiles: new Set([sel.file]), clipped: 0,
    };
  }
  const ex = M.explore;
  if (ex.running) return;
  ex.running = true;
  ex.error = null;
  renderRail();
  const seeds = [...ex.reached].slice(0, MAP_EXPLORE_SEEDS);
  const clipped = ex.reached.size - seeds.length;
  try {
    const qs = new URLSearchParams({
      hops: '2', direction: 'both', limit: '1000',
      include_expired: 'false', include_nodes: 'false', include_evidence: 'false',
    });
    if (M.asOf != null) qs.set('as_of', String(M.asOf));
    if (seeds.length === 1) qs.set('node', seeds[0]);
    else qs.set('nodes', seeds.join(','));
    const r = await fetch(`${M.baseUrl}/_graph/${enc(M.brain)}/ego?${qs}`);
    const j = await r.json().catch(() => null);
    if (!r.ok) {
      throw new Error(j && j.error && j.error.reason ? j.error.reason : `HTTP ${r.status}`);
    }
    ex.calls += 1;
    ex.hops += 2;
    ex.clipped = clipped;
    // Reached = admitted seeds + every newly discovered neighbor.
    for (const s of j.seeds || []) if (typeof s === 'string') ex.reached.add(s);
    for (const nb of j.neighbors || []) if (nb && typeof nb.id === 'string') ex.reached.add(nb.id);
    const { fg } = M.model;
    ex.reachedFiles = new Set();
    for (const id of ex.reached) {
      const oi = M.idIndex.get(id);
      if (oi !== undefined) ex.reachedFiles.add(fg.fileOf[oi]);
    }
    sbLogRead('graph walk', `2 hops from ${num(seeds.length)} note${seeds.length === 1 ? '' : 's'} · ${num(ex.reached.size)} reached so far`, 'you asked to explore further');
  } catch (e) {
    ex.error = String((e && e.message) || e);
  } finally {
    ex.running = false;
    renderRail();
    scheduleDraw();
  }
}

// ----- the rail -----------------------------------------------------

function railEl() {
  return document.querySelector('[data-sb-map-rail]');
}

const railRow = (k, v) => `<div class="sb-hrow"><span class="sb-hk">${esc(k)}</span><span class="sb-hv">${esc(v)}</span></div>`;

function kindChip(e) {
  const kind = evidenceKind(e);
  if (kind === 'quote') return '<span class="sb-eq">QUOTE</span>';
  if (kind === 'rationale') return '<span class="sb-eq sb-eq-why">WHY</span>';
  return '<span class="sb-eq sb-eq-none">NO EVIDENCE</span>';
}

/** The open-in-ledger affordance: routed through sbRefocus so every
 *  terminal click lands in the ledger, scrolled into sight. */
const openBtn = (id, label) =>
  `<button type="button" class="text-btn sb-map-act" data-sb-map-open="${esc(id)}">${esc(label)}</button>`;

function railHome() {
  const model = M.model;
  if (M.status === 'loading') return `<div class="panel-empty mono faint">READING EVERY LINK…</div>`;
  if (M.status === 'error') return `<div class="panel-empty mono faint">COULD NOT READ THE LINK GRAPH · ${esc(String(M.error).slice(0, 90))}</div>`;
  if (!model) return '';
  const voc = sbVocabNow();
  const capped = !!M.capped;
  const orphanRow = M.nodesTotal != null
    ? railRow(`${voc.items} with no links`, `${capped ? '≥ ' : ''}${num(Math.max(0, M.nodesTotal - model.distinctNotes))}`)
    : railRow(`${voc.items} with no links`, 'could not be counted');
  const hl = M.highlightFiles
    ? `<div class="sb-map-note accent">FIND: ${num(M.highlightFiles.size)} document${M.highlightFiles.size === 1 ? '' : 's'} lit up${M.highlightMiss ? ` · ${num(M.highlightMiss)} match${M.highlightMiss === 1 ? '' : 'es'} have no links — not on the map` : ''}</div>`
    : '';
  return `
    ${hl}
    <div class="sb-hgroup key">WHAT THE MAP DID NOT SHOW</div>
    ${railRow('links beyond the map budget', capped ? `${M.serverTotalIsFloor ? '≥ ' : ''}${num(Math.max(0, M.serverTotal - model.g.n))}` : '0')}
    ${railRow('links inside single documents', num(model.fg.selfLoops))}
    ${orphanRow}
    ${railRow(`${voc.items} drawn per opened group`, `≤ ${num(MAP_EXPAND_CAP)} — the rest wait in a satellite`)}
    <div class="sb-hgroup key">HOW TO READ IT</div>
    <div class="sb-map-note faint">each name is a group of documents that cite each other or share a folder · line thickness is how many links cross · click a group to spread it · click a line to see the quotes that taught those links · click a ${voc.item} to open its ledger below</div>`;
}

function railCluster(ci) {
  const model = M.model;
  const voc = sbVocabNow();
  const c = model.clusters[ci];
  const t = caretT();
  const live = M.previewT !== undefined ? clusterLiveAt(ci, t) : c.linkLive;
  const notNow = c.linkTotal - live; // retired or not yet believed at the caret
  const kindCount = new Map();
  for (let m = 0; m < model.fg.fa.length; m++) {
    if (model.clusterOfFile[model.fg.fa[m]] !== ci || model.clusterOfFile[model.fg.fb[m]] !== ci) continue;
    for (const [ord, n] of model.fg.fKind[m]) kindCount.set(ord, (kindCount.get(ord) || 0) + n);
  }
  const domKind = [...kindCount.entries()].sort((a, b) => b[1] - a[1] || a[0] - b[0])[0];
  const md = M.memberDraw && M.memberDraw.ci === ci ? M.memberDraw : null;
  const capNote = md && md.satellite > 0
    ? `<div class="sb-map-note">TOP ${num(md.files.length)} OF ${num(md.total)} DOCUMENTS DRAWN · the satellite holds the other ${num(md.satellite)}</div>` : '';
  const intraNote = md && md.intraTotal > MAP_INTRA_EDGE_DRAW
    ? railRow('links drawn inside this group', `strongest ${num(MAP_INTRA_EDGE_DRAW)} of ${num(md.intraTotal)}`) : '';
  return `
    <div class="sb-map-selname">${esc(clusterName(ci))}</div>
    ${railRow(voc.items, num(c.noteCount))}
    ${railRow('documents', num(c.files.length))}
    ${railRow(voc.linksAtMoment, num(live))}
    ${notNow > 0 ? railRow(voc.linksNotNow, num(notNow)) : ''}
    ${domKind ? railRow('mostly connected by', typeName(model.g.typeNames[domKind[0]]).toLowerCase()) : ''}
    ${c.isElse
    ? `<div class="sb-map-note faint">small groups and loose ${voc.items} pooled so the picture stays readable — nothing is hidden, it is all here</div>`
    : (c.dirLabel
      ? railRow('named after its folder', `holds ≥ 60% of its links`)
      : railRow(`named after its busiest ${voc.item}`, 'no folder holds 60% of its links'))}
    ${capNote}${intraNote}
    ${openBtn(fileRepId(c.hubFile), `OPEN ITS BUSIEST ${voc.Item} IN THE LEDGER`)}
    <div class="sb-map-note faint">${M.expanded === ci ? 'click the group name again to fold it away' : `click the group on the map to spread its ${voc.items}`}</div>`;
}

function railMember(f) {
  const id = fileRepId(f);
  const voc = sbVocabNow();
  const meta = M.names.get(id) || {};
  const model = M.model;
  const notes = model.fg.fileSize[f];
  const t = caretT();
  let live = 0;
  let total = 0;
  for (let m = 0; m < model.fg.fa.length; m++) {
    if (model.fg.fa[m] !== f && model.fg.fb[m] !== f) continue;
    for (const i of model.fg.fEdges[m]) {
      total += 1;
      if (edgeStateAtI(model.g, i, t) === 'live') live += 1;
    }
  }
  const ex = M.explore;
  const exploreBlock = ex
    ? (ex.error
      ? `<div class="sb-map-note">COULD NOT WALK FURTHER · ${esc(ex.error.slice(0, 120))}</div>`
      : `<div class="sb-map-note accent">${num(ex.reached.size)} ${ex.reached.size === 1 ? voc.item : voc.items} within ${num(ex.hops)} hop${ex.hops === 1 ? '' : 's'} · ${num(ex.calls)} walk${ex.calls === 1 ? '' : 's'} of 2 hops each${ex.clipped > 0 ? ` · ${num(ex.clipped)} starting ${voc.items} past the per-walk cap wait for the next walk` : ''}</div>
         <div class="sb-map-note faint">depth comes from repeated 2-hop walks — the engine caps each walk at 2 hops, and this view says so instead of pretending otherwise</div>`)
    : '';
  return `
    <div class="sb-map-selname">${esc(meta.path ? meta.path.split('/').pop() : (meta.title || displayId(id)))}</div>
    ${meta.path ? `<div class="sb-map-note faint">${esc(meta.path)}</div>` : ''}
    ${meta.title && meta.path ? `<div class="sb-map-note">${esc(meta.title.slice(0, 90))}</div>` : ''}
    ${notes > 1 ? railRow(`${voc.items} inside (reading order)`, num(notes)) : ''}
    ${railRow(voc.linksAtMoment, num(live))}
    ${total > live ? railRow(voc.linksNotNow, num(total - live)) : ''}
    ${openBtn(id, 'OPEN THE LEDGER · EVERY LINK, WITH WHAT TAUGHT IT')}
    <button type="button" class="text-btn sb-map-act" data-sb-map-explore${ex && ex.running ? ' disabled' : ''}>${ex ? (ex.running ? 'WALKING…' : 'WALK 2 MORE HOPS') : 'EXPLORE · WALK 2 HOPS FROM HERE'}</button>
    ${exploreBlock}`;
}

function railSatellite() {
  const md = M.memberDraw;
  if (!md || md.satellite <= 0) return railHome();
  const model = M.model;
  const order = memberOrder(model.clusters[md.ci]);
  const rest = order.slice(md.files.length);
  const listed = rest.slice(0, MAP_SATELLITE_LIST);
  // Re-render only when names actually landed; hydrateNames settles
  // every asked-for id (hit or miss) and returns 0 once nothing is
  // new, so the render→hydrate→render chain cannot loop.
  hydrateNames(listed.map(fileRepId), `naming the ${sbVocabNow().items} in the satellite`)
    .then((added) => { if (added) renderRail(); });
  const rows = listed.map((f) =>
    `<button type="button" class="sb-clink sb-map-satrow" data-sb-map-open="${esc(fileRepId(f))}">${esc(fileLabel(f, 40))}</button>`).join('');
  return `
    <div class="sb-map-selname">+ ${num(md.satellite)} MORE ${sbVocabNow().Items}</div>
    <div class="sb-map-note">${listed.length < rest.length ? `FIRST ${num(listed.length)} OF ${num(rest.length)} · ` : ''}each opens its ledger below</div>
    <div class="sb-map-satlist">${rows}</div>`;
}

function railBundle() {
  const sel = M.sel;
  const model = M.model;
  let title;
  if (model.mode === 'clustered') {
    const bu = model.bundles[sel.bi];
    title = `${clusterName(bu.a)} ↔ ${clusterName(bu.b)}`;
  } else {
    title = `${fileLabel(model.fg.fa[sel.bi], 18)} ↔ ${fileLabel(model.fg.fb[sel.bi], 18)}`;
  }
  const head = `<div class="sb-map-selname">${esc(title)}</div>
    <div class="sb-map-note">${sel.total > MAP_BUNDLE_INSPECT ? `TOP ${num(MAP_BUNDLE_INSPECT)} OF ${num(sel.total)} · strongest first` : `${num(sel.total)} link${sel.total === 1 ? '' : 's'}`}</div>`;
  if (sel.rows == null) return `${head}<div class="panel-empty mono faint">READING WHAT TAUGHT THESE LINKS…</div>`;
  if (sel.error) return `${head}<div class="panel-empty mono faint">COULD NOT READ THE EVIDENCE · ${esc(sel.error.slice(0, 80))}</div>`;
  const t = caretT();
  const now = t == null ? Date.now() : t;
  const rows = sel.rows.map((e) => {
    const kind = evidenceKind(e);
    const q = e.evidence && e.evidence.quote ? e.evidence.quote : null;
    const retired = e.invalid_at != null && Number(e.invalid_at) <= now;
    const line = kind === 'quote' ? `“${esc(q)}”`
      : kind === 'rationale' ? `WHY — ${esc(q)}`
        : 'no evidence recorded — asserted, not detected';
    return `
    <button type="button" class="sb-map-evrow${retired ? ' sb-expired' : ''}" data-sb-map-open="${esc(e.src)}"
            title="Open the ledger for the note this link starts from">
      <span class="sb-map-evline${kind === 'quote' ? '' : ' faint'}">${line}</span>
      <span class="sb-map-evmeta faint">${kindChip(e)} taught by ${esc(detectorName(e.detector).toLowerCase())}${e.src_file ? ` · from ${esc(e.src_file)}` : ''}${retired ? ' · RETIRED AT THIS MOMENT' : ''}</span>
    </button>`;
  }).join('');
  return `${head}<div class="sb-map-evlist">${rows}</div>`;
}

function renderRail() {
  const el = railEl();
  if (!el) return;
  const sel = M.sel;
  let html;
  if (!sel) html = railHome();
  else if (sel.kind === 'cluster') html = railCluster(sel.ci);
  else if (sel.kind === 'member') html = railMember(sel.file);
  else if (sel.kind === 'satellite') html = railSatellite();
  else if (sel.kind === 'bundle') html = railBundle();
  else html = railHome();
  el.innerHTML = html;
}
