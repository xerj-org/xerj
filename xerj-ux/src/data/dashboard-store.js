// ============================================================
// XERJ.ai — Dashboard metadata store (backend-durable)
//
// Previously this layer wrote ONLY to localStorage['xerj.dashboards'],
// so a user-created dashboard vanished on a cache-clear or a new device
// — the durable `/_xerj-console/api/v1/dashboards` CRUD backend
// (engine/crates/xerj-console-api/src/dashboards.rs) had no caller.
//
// It is now an ASYNC FACADE over that CRUD surface with a localStorage
// write-through cache:
//
//   • boot: hydrate() = GET /dashboards → in-memory map DOCS, mirrored
//     to `xerj.dashboards` LS for instant next-boot paint. If the
//     request fails we keep the LS cache (offline/dev still works).
//   • every mutation writes optimistically to DOCS + LS, then fires the
//     matching CRUD call; a 409 (stale etag) re-GETs and re-applies.
//   • createUserDashboard POSTs a REAL dashboard doc (declarative
//     panels[]) and returns the server-assigned uuid; offline it falls
//     back to a local `user-…` id so dev keeps working.
//
// DEFAULT (code) dashboards keep their local metadata (rename / nav
// order / hidden) in the `xerj.dashboards` LS blob as before — those are
// re-derivable overrides on code, not user data. USER dashboards are the
// durable backend records.
// ============================================================

import * as api from './dashboard-api.js';

const LS_KEY = 'xerj.dashboards';
const LAYOUT_PREFIX = 'xerj.layout.';

/**
 * LS blob shape (v2):
 * {
 *   order:  ['ai-overview', …, '<uuid>'],   // nav order (code + user)
 *   names:  { '<id>': 'Custom name' },       // per-id rename override
 *   hidden: ['logs-overview'],               // hidden from nav
 *   custom: { '<uuid>': <Dashboard doc> },   // MIRROR of DOCS (offline paint)
 * }
 *
 * A user Dashboard doc (backend contract):
 *   { id, owner, visibility, name, section, group, cloned_from,
 *     panels:[<declarative Panel>], filters_default, time_default,
 *     version, created_at, updated_at, deleted_at }
 */

function load() {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return {};
    return JSON.parse(raw) || {};
  } catch { return {}; }
}
function save(m) {
  try { localStorage.setItem(LS_KEY, JSON.stringify(m)); } catch { /* quota */ }
}

// ── in-memory state ──────────────────────────────────────────────────
// Seed DOCS from the LS cache synchronously so the very first paint (in
// parseRoute / render, before hydrate resolves) shows the user's
// dashboards offline. hydrate() overwrites this from the server.
let DOCS = (() => {
  const m = load();
  return m.custom && typeof m.custom === 'object' ? { ...m.custom } : {};
})();

let onChange = null;
export function setOnChange(cb) { onChange = cb; }
function emitChange() { if (typeof onChange === 'function') { try { onChange(); } catch { /* ignore */ } } }

/** Write DOCS → LS `custom`, ensuring every user id is in `order`. */
function mirror() {
  const m = load();
  m.custom = { ...DOCS };
  m.order = Array.isArray(m.order) ? m.order.slice() : [];
  // Drop order entries for user ids no longer present, keep code ids.
  m.order = m.order.filter((id) => DOCS[id] || !isLikelyUserId(id, m));
  for (const id of Object.keys(DOCS)) if (!m.order.includes(id)) m.order.push(id);
  save(m);
}
// A user id is anything we have (or had) a custom doc for. Code ids are
// never in DOCS, so "not in DOCS and was a custom key" ⇒ removed user id.
function isLikelyUserId(id, m) {
  return !!(m.custom && Object.prototype.hasOwnProperty.call(m.custom, id) && !DOCS[id]);
}

// ── serialized per-id write queue (avoids etag races on rapid edits) ──
const queues = {};
function enqueue(id, fn) {
  const prev = queues[id] || Promise.resolve();
  const next = prev.then(fn, fn);
  queues[id] = next.catch(() => {});
  return next;
}

/** PATCH a doc, adopting the returned (version-bumped) server doc. On a
 *  409 (stale etag) re-GET and retry once with the fresh version. */
async function pushPatch(id, partial) {
  const doc = DOCS[id];
  if (!doc || doc.__local) return;                 // offline-only doc: nothing to push
  try {
    const { doc: fresh } = await api.patch(id, partial, doc.version);
    DOCS[id] = fresh; mirror();
  } catch (e) {
    if (e && e.status === 409) {
      try {
        const { doc: server } = await api.get(id);
        // Re-apply our field(s) on top of the server's fresh version.
        const merged = { ...server, ...partial };
        const { doc: fresh } = await api.patch(id, partial, server.version);
        DOCS[id] = fresh || merged; mirror(); emitChange();
      } catch { /* give up; optimistic local state stands */ }
    }
    // network/other error: keep optimistic local state (offline fallback)
  }
}

// ── reads ────────────────────────────────────────────────────────────

/**
 * Merge default (code) dashboards with user metadata + backend user
 * dashboards. Returns render-ready entries in nav order. User entries
 * carry `{ isUser:true, declarative, panels, doc, version, render }`.
 */
export function mergedDashboards(defaults, { includeHidden = false } = {}) {
  const m = load();
  const hidden = new Set(m.hidden || []);
  const names = m.names || {};
  const byId = Object.fromEntries(defaults.map((d) => [d.id, d]));
  const order = Array.isArray(m.order) && m.order.length ? m.order : defaults.map((d) => d.id);

  const emitUser = (id) => {
    const doc = DOCS[id] || (m.custom && m.custom[id]);
    if (!doc) return null;
    const clonedFrom = doc.cloned_from || doc.clonedFrom || null;
    const template = clonedFrom ? byId[clonedFrom] : null;
    const panels = Array.isArray(doc.panels) ? doc.panels : [];
    // Declarative = net-new (no live template) — renders from panels[]
    // via the builder. A clone of a still-present code template borrows
    // that template's render (durable, but not panel-editable).
    const declarative = !template;
    return {
      id,
      name: names[id] || doc.name,
      isUser: true,
      declarative,
      clonedFrom,
      panels,
      doc,
      version: doc.version,
      section: doc.section || 'dashboards',
      group: doc.group || 'user',
      hidden: hidden.has(id),
      render: declarative
        ? makeDeclarativeRender(doc, names[id])
        : (template.render || blankRender(doc.name)),
    };
  };

  const out = [];
  const seen = new Set();
  for (const id of order) {
    if (hidden.has(id) && !includeHidden) { seen.add(id); continue; }
    if (byId[id]) {
      out.push({ ...byId[id], name: names[id] || byId[id].name, isUser: false, hidden: hidden.has(id) });
      seen.add(id);
    } else {
      const u = emitUser(id);
      if (u) { out.push(u); seen.add(id); }
    }
  }
  for (const def of defaults) {
    if (seen.has(def.id)) continue;
    if (hidden.has(def.id) && !includeHidden) continue;
    out.push({ ...def, name: names[def.id] || def.name, isUser: false, hidden: hidden.has(def.id) });
    seen.add(def.id);
  }
  // Freshly hydrated user docs not yet placed in `order`.
  for (const id of Object.keys(DOCS)) {
    if (seen.has(id)) continue;
    const u = emitUser(id);
    if (u && (!u.hidden || includeHidden)) { out.push(u); seen.add(id); }
  }
  return out;
}

/** Render fn for a declarative (net-new) user dashboard. */
function makeDeclarativeRender(doc, nameOverride) {
  return () => ({
    title: (nameOverride || doc.name || 'UNTITLED').toUpperCase(),
    kicker: 'USER DASHBOARD',
    meta: [((doc.panels && doc.panels.length) || 0) + ' PANELS'],
    isDeclarative: true,
    panels: (Array.isArray(doc.panels) ? doc.panels : []).map((p) => ({ ...p, source: 'user' })),
  });
}

/** Fallback for a clone whose template was deleted out from under it. */
function blankRender(name) {
  return () => ({
    title: (name || 'BLANK').toUpperCase(),
    kicker: 'USER DASHBOARD',
    meta: ['NO TEMPLATE'],
    isDeclarative: true,
    panels: [],
  });
}

/** The live backend doc for a user dashboard (panels + version). */
export function getDoc(id) { return DOCS[id] || null; }

/** Is the given id a user-created dashboard (vs. a default)? */
export function isUserDash(id) { return !!DOCS[id]; }

/** Raw read of the LS blob — used by the MANAGE view. */
export function readStore() { return load(); }

// ── boot ─────────────────────────────────────────────────────────────

/** Pull the durable dashboard set from the backend, merge into DOCS,
 *  mirror to LS. Idempotent; safe to call before app.js first paint. */
export async function hydrate() {
  try {
    const { dashboards } = await api.list();
    const next = {};
    for (const d of dashboards || []) {
      if (!d || !d.id || d.deleted_at) continue;
      if (d.visibility === 'default') continue;   // managed/code-owned, read-only
      next[d.id] = d;
    }
    DOCS = next;
    mirror();
    emitChange();
    return true;
  } catch {
    // Offline: keep the DOCS seeded from the LS cache at module init.
    return false;
  }
}

// ── mutations ────────────────────────────────────────────────────────

export function renameDashboard(id, name) {
  const m = load();
  m.names = m.names || {};
  const nm = (name || '').trim();
  if (nm) m.names[id] = nm; else delete m.names[id];
  save(m);
  if (DOCS[id]) {
    DOCS[id] = { ...DOCS[id], name: nm || DOCS[id].name };
    mirror();
    if (!DOCS[id].__local) enqueue(id, () => pushPatch(id, { name: DOCS[id].name }));
  }
}

export function reorderDashboards(orderIds) {
  const m = load();
  m.order = orderIds.slice();
  save(m);
}

export function setHidden(id, hidden) {
  const m = load();
  m.hidden = m.hidden || [];
  const has = m.hidden.includes(id);
  if (hidden && !has) m.hidden.push(id);
  if (!hidden && has) m.hidden = m.hidden.filter((x) => x !== id);
  save(m);
}

/**
 * Create a user-owned dashboard as a REAL backend doc.
 *   fromId   → clone (borrows that code template's render; durable name)
 *   no fromId → net-new declarative dashboard (empty panels[] + builder)
 * Returns the server uuid (or a local `user-…` id when offline).
 */
export async function createUserDashboard({ name, fromId = null, dataSource = null } = {}) {
  const clonedFrom = fromId || null;
  const body = {
    name: name || 'Untitled',
    visibility: 'private',
    section: 'dashboards',
    group: 'user',
    cloned_from: clonedFrom,
    panels: [],
    filters_default: {},
    time_default: null,
  };
  try {
    const { doc } = await api.create(body);
    DOCS[doc.id] = doc;
    pushOrder(doc.id);
    mirror();
    emitChange();
    return doc.id;
  } catch (e) {
    // Offline fallback — local id, localStorage only (old behaviour).
    const id = 'user-' + Math.random().toString(36).slice(2, 8) + Date.now().toString(36).slice(-4);
    const now = new Date().toISOString();
    DOCS[id] = {
      id, owner: 'local', org_id: 'default', visibility: 'private',
      name: body.name, section: 'dashboards', group: 'user',
      cloned_from: clonedFrom, panels: [], filters_default: {}, time_default: null,
      version: 0, created_at: now, updated_at: now, deleted_at: null, __local: true,
    };
    pushOrder(id);
    mirror();
    emitChange();
    return id;
  }
}

function pushOrder(id) {
  const m = load();
  m.order = Array.isArray(m.order) ? m.order : [];
  if (!m.order.includes(id)) m.order.push(id);
  save(m);
}

export async function deleteUserDashboard(id) {
  const doc = DOCS[id];
  delete DOCS[id];
  const m = load();
  if (m.order) m.order = m.order.filter((x) => x !== id);
  if (m.names) delete m.names[id];
  if (m.hidden) m.hidden = m.hidden.filter((x) => x !== id);
  if (m.custom) delete m.custom[id];
  save(m);
  try { localStorage.removeItem(LAYOUT_PREFIX + id); } catch { /* ignore */ }
  emitChange();
  if (doc && !doc.__local) {
    try { await api.remove(id, doc.version); } catch { /* server prune best-effort */ }
  }
}

/**
 * Replace a user dashboard's panels[] (add / move / resize / remove).
 * Optimistic: DOCS + LS update immediately; PATCH fires (serialised).
 */
export function patchPanels(id, panels) {
  if (!DOCS[id]) return;
  DOCS[id] = { ...DOCS[id], panels };
  mirror();
  if (!DOCS[id].__local) enqueue(id, () => pushPatch(id, { panels }));
}

/** Patch arbitrary top-level fields on a user doc (e.g. time_default). */
export function patchDashboard(id, partial) {
  if (!DOCS[id]) return;
  DOCS[id] = { ...DOCS[id], ...partial };
  mirror();
  if (!DOCS[id].__local) enqueue(id, () => pushPatch(id, partial));
}

/** Wipe LOCAL metadata + cache. Backend user dashboards are durable and
 *  re-appear on the next hydrate() — that is the point of the rework. */
export function resetAll() {
  DOCS = {};
  try { localStorage.removeItem(LS_KEY); } catch { /* ignore */ }
}
