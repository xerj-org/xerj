// ============================================================
// XERJ.ai — Dashboard metadata store
//
// This is the thin persistent layer that turns "dashboards are
// hardcoded JS files" into "dashboards have editable metadata".
//
// Default dashboards (the ones shipped in src/dashboards/*.js)
// are still code — diffable, reviewable, version-controlled.
// But users can NOW:
//   • rename them
//   • reorder them in the nav
//   • hide them
//   • clone them to create a new user-owned dashboard
//   • delete user dashboards (defaults can only be hidden, never deleted)
//
// Everything lives under the `xerj.dashboards` localStorage key
// so it survives reloads and round-trips through export/import.
// ============================================================

const LS_KEY = 'xerj.dashboards';

/**
 * Full shape of the store:
 *
 * {
 *   order:  ['ai-overview', 'rag-quality', 'user-1abc', ...],  // nav order
 *   names:  { 'ai-overview': 'AI Overview (mine)' },           // per-id rename
 *   hidden: ['logs-overview'],                                 // hidden from nav
 *   custom: {
 *     'user-1abc': {
 *       name:       'Prod API Traffic',
 *       clonedFrom: 'search-discover',                         // default template id
 *       createdAt:  '2026-04-14T18:20:00Z',
 *       dataSource: { cluster: 'prod-us', index: 'logs-prod' },
 *     }
 *   }
 * }
 */

function load() {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return {};
    return JSON.parse(raw);
  } catch {
    return {};
  }
}
function save(m) {
  localStorage.setItem(LS_KEY, JSON.stringify(m));
}

// ---------- reads ----------------------------------------

/**
 * Merge default dashboards with user metadata. Returns an array
 * of `{ id, name, isUser, clonedFrom?, dataSource?, render }` in
 * the user-defined nav order. Hidden items are excluded. Pass
 * `{ includeHidden: true }` to get them too (used by MANAGE view).
 */
export function mergedDashboards(defaults, { includeHidden = false } = {}) {
  const m = load();
  const hidden = new Set(m.hidden || []);
  const custom = m.custom || {};
  const names  = m.names  || {};

  // Build a map for quick lookup.
  const byId = Object.fromEntries(defaults.map((d) => [d.id, d]));
  const orderList = Array.isArray(m.order) && m.order.length
    ? m.order
    : defaults.map((d) => d.id);

  const out = [];
  const seen = new Set();
  for (const id of orderList) {
    if (hidden.has(id) && !includeHidden) { seen.add(id); continue; }
    const def = byId[id];
    const user = custom[id];
    if (def) {
      out.push({
        ...def,
        name: names[id] || def.name,
        isUser: false,
        hidden: hidden.has(id),
      });
    } else if (user) {
      // A user-created dashboard — borrow the render from its template.
      const source = user.clonedFrom ? byId[user.clonedFrom] : null;
      out.push({
        id,
        name: names[id] || user.name,
        render: source?.render || blankRender(user.name),
        isUser: true,
        clonedFrom: user.clonedFrom,
        dataSource: user.dataSource,
        hidden: hidden.has(id),
      });
    }
    seen.add(id);
  }
  // Append defaults the user hasn't yet placed (new ship adds).
  for (const def of defaults) {
    if (seen.has(def.id)) continue;
    if (hidden.has(def.id) && !includeHidden) continue;
    out.push({
      ...def,
      name: names[def.id] || def.name,
      isUser: false,
      hidden: hidden.has(def.id),
    });
  }
  return out;
}

/** Lightweight fallback render used for a user-created dashboard whose
 *  clonedFrom template was deleted out from under it. */
function blankRender(name) {
  return () => ({
    title: (name || 'BLANK').toUpperCase(),
    kicker: 'USER DASHBOARD',
    meta: ['NO TEMPLATE'],
    panels: [
      {
        id: 'empty',
        eyebrow: 'NOTHING HERE YET',
        cols: 12,
        render: () =>
          '<div class="mono faint">This user dashboard was cloned from a template that no longer exists. Open the MANAGE view to re-point it at a different template, or delete and recreate.</div>',
      },
    ],
  });
}

// ---------- mutations ------------------------------------

export function renameDashboard(id, name) {
  const m = load();
  m.names = m.names || {};
  if (name && name.trim()) m.names[id] = name.trim();
  else delete m.names[id];
  save(m);
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
 * Create a user-owned dashboard. If `fromId` is provided, it
 * becomes a clone of that default (render inherited). Otherwise
 * it's a blank shell. Returns the new id.
 */
export function createUserDashboard({ name, fromId = null, dataSource = null } = {}) {
  const m = load();
  m.custom = m.custom || {};
  const id = 'user-' + Math.random().toString(36).slice(2, 8) + Date.now().toString(36).slice(-4);
  m.custom[id] = {
    name: name || 'Untitled',
    clonedFrom: fromId,
    createdAt: new Date().toISOString(),
    dataSource,
  };
  m.order = m.order || [];
  m.order.push(id);
  save(m);
  return id;
}

export function deleteUserDashboard(id) {
  const m = load();
  if (m.custom) delete m.custom[id];
  if (m.order) m.order = m.order.filter((x) => x !== id);
  if (m.names) delete m.names[id];
  if (m.hidden) m.hidden = m.hidden.filter((x) => x !== id);
  save(m);
  // Also wipe any layout overrides tied to this id
  localStorage.removeItem('xerj.layout.' + id);
}

/** Wipe every metadata edit. Defaults reappear in their original order. */
export function resetAll() {
  localStorage.removeItem(LS_KEY);
}

/** Is the given id a user-created dashboard (as opposed to a default)? */
export function isUserDash(id) {
  const m = load();
  return !!(m.custom && m.custom[id]);
}

/** Raw read of the store — used by MANAGE view to show what's persisted. */
export function readStore() { return load(); }
