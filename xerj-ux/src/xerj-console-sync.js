// xerj-console-sync.js — durable persistence for the SPA.
//
// The existing SPA (`app.js`) reads and writes a handful of well-known
// keys under localStorage (`xerj.theme`, `xerj.time`, `xerj.views`,
// …).  This module makes those values durable across browsers and
// machines by mirroring them to /_xerj-console/api/v1/prefs and
// /_xerj-console/api/v1/views.
//
// It is **non-invasive**: app.js is not modified.  Instead we
//
//   1. on boot, **pull** the server-side values and write them into
//      localStorage *before* app.js reads them at module load, and
//   2. periodically (every 1.5s) **push** any local changes back to
//      the server, debounced so a slider drag does not spam writes.
//
// Both halves are best-effort.  An unreachable backend (offline dev,
// hot-reload mid-flight, network blip) silently falls back to plain
// localStorage so the SPA never gets stuck.

import { api } from './xerj-console-auth.js';

// ── Keys we mirror as a single `/prefs` document. ────────────────────────────
// `xerj.search` is intentionally excluded — it's the search box's last
// query, which is per-tab ephemera, not "user prefs".
// `xerj.dashboards` is NO LONGER mirrored here: user dashboards now live
// in the durable `/dashboards` CRUD surface (see data/dashboard-store.js),
// not the /prefs blob. Mirroring it here caused a double-write AND the
// diff-baseline bug below silently dropped it anyway.
// Per-dashboard panel layouts (`xerj.layout.<id>`) — DEFAULT-dashboard
// column/order/hidden overrides only — and per-dashboard filters
// (`xerj.filters.<id>`) are aggregated into one `layouts` and `filters`
// map respectively, since the count is unbounded and the /prefs schema
// doesn't accept dynamic key prefixes.
const PREF_KEYS = [
  'xerj.theme',
  'xerj.time',
  'xerj.timeFrom',
  'xerj.timeTo',
  'xerj.cluster',
  'xerj.refresh',
  'xerj.mobile',
  'xerj.edit',
];

const VIEWS_KEY = 'xerj.views';

// Aggregate per-dashboard keys under prefs.layouts and prefs.filters so
// the user's panel arrangement and saved filters travel with them.
const LAYOUT_PREFIX  = 'xerj.layout.';
const FILTERS_PREFIX = 'xerj.filters.';

// ─── Pull (boot) ─────────────────────────────────────────────────────────────

/** Fetch /prefs and /views, seed localStorage. Idempotent. */
export async function pullAll() {
  await Promise.all([pullPrefs(), pullViews()]);
}

async function pullPrefs() {
  let server;
  try { server = await api('GET', '/prefs'); }
  catch (e) { console.warn('[xerj-console-sync] /prefs unreachable:', e.message); return; }
  if (!server || typeof server !== 'object') return;
  // Capture the raw server doc so startPush() can baseline against what
  // the SERVER actually holds — not against current localStorage — which
  // is what fixes the "locally-present, server-absent key never pushes"
  // diff bug.
  lastServerPrefs = server;
  // Server doc shape mirrors a flat key/value map. Translate `theme`,
  // `time`, `cluster`… back into `xerj.theme`, `xerj.time`,
  // `xerj.cluster`… so the existing SPA reads pick them up.
  for (const fullKey of PREF_KEYS) {
    const short = fullKey.replace(/^xerj\./, '');
    if (server[short] !== undefined && server[short] !== null) {
      const val = typeof server[short] === 'string'
        ? server[short]
        : JSON.stringify(server[short]);
      // String-coerce numbers/bools to match the SPA's expectations
      // (most reads use raw `localStorage.getItem` which returns a string).
      try { localStorage.setItem(fullKey, val); } catch {}
    }
  }
  // Per-dashboard layouts: { dashId: '<JSON>' } → xerj.layout.<dashId>
  if (server.layouts && typeof server.layouts === 'object') {
    for (const [id, raw] of Object.entries(server.layouts)) {
      if (raw == null) continue;
      const v = typeof raw === 'string' ? raw : JSON.stringify(raw);
      try { localStorage.setItem(LAYOUT_PREFIX + id, v); } catch {}
    }
  }
  // Per-dashboard filters: same shape under prefs.filters.
  if (server.filters && typeof server.filters === 'object') {
    for (const [id, raw] of Object.entries(server.filters)) {
      if (raw == null) continue;
      const v = typeof raw === 'string' ? raw : JSON.stringify(raw);
      try { localStorage.setItem(FILTERS_PREFIX + id, v); } catch {}
    }
  }
}

async function pullViews() {
  let server;
  try { server = await api('GET', '/views'); }
  catch (e) { console.warn('[xerj-console-sync] /views unreachable:', e.message); return; }
  if (!server || !Array.isArray(server.views)) return;
  // app.js stores views as a flat array under `xerj.views`. Map the
  // server shape (which has `dashboard_id`, `time`, `filters`,
  // `updated_at`) into whatever the SPA expects — the SPA accepts any
  // serialisable view object so a thin pass-through works.
  try {
    localStorage.setItem(VIEWS_KEY, JSON.stringify(server.views));
  } catch {}
}

// ─── Push (debounced) ────────────────────────────────────────────────────────

let lastPushedPrefs = null;
let lastPushedViewsKey = '';
let lastServerPrefs = null;    // raw /prefs doc captured by pullPrefs
let pushTimer = null;

/** Start the periodic push tick. Idempotent. */
export function startPush() {
  if (pushTimer) return;
  // Baseline against the SERVER's values only — NOT current localStorage.
  // The old code snapshotted localStorage here, so any key already present
  // locally at boot was treated as "already pushed" and never sent (the
  // reason the user's dashboards/layouts silently failed to persist). By
  // baselining from `lastServerPrefs`, every local key the server is
  // missing diffs dirty and pushes on the first tick.
  lastPushedPrefs = serverBaselinePrefs();
  lastPushedViewsKey = readViewsRaw();
  pushTimer = setInterval(() => { pushIfDirty().catch(() => {}); }, 1500);
}

// Stable, order-independent JSON so two snapshots with the same content
// but different key insertion order still compare equal (no spurious push).
function sortDeep(v) {
  if (Array.isArray(v)) return v.map(sortDeep);
  if (v && typeof v === 'object') {
    const out = {};
    for (const k of Object.keys(v).sort()) out[k] = sortDeep(v[k]);
    return out;
  }
  return v;
}
function stableStringify(obj) { return JSON.stringify(sortDeep(obj)); }

function snapshotPrefs() {
  return stableStringify(buildPrefsFromLocalStorage());
}

function buildPrefsFromLocalStorage() {
  const out = {};
  for (const fullKey of PREF_KEYS) {
    const short = fullKey.replace(/^xerj\./, '');
    const v = localStorage.getItem(fullKey);
    if (v !== null) out[short] = v;
  }
  // Aggregate per-dashboard layouts + filters.
  const layouts = {};
  const filters = {};
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (!k) continue;
      if (k.startsWith(LAYOUT_PREFIX)) {
        layouts[k.slice(LAYOUT_PREFIX.length)] = localStorage.getItem(k);
      } else if (k.startsWith(FILTERS_PREFIX)) {
        filters[k.slice(FILTERS_PREFIX.length)] = localStorage.getItem(k);
      }
    }
  } catch {}
  if (Object.keys(layouts).length) out.layouts = layouts;
  if (Object.keys(filters).length) out.filters = filters;
  return out;
}

// Rebuild the "already-pushed" baseline from the SERVER doc, coercing to
// the same string shape snapshotPrefs uses. Keys the server didn't return
// are OMITTED — so a locally-present-but-server-absent key diffs dirty.
function serverBaselinePrefs() {
  const server = lastServerPrefs || {};
  const out = {};
  for (const fullKey of PREF_KEYS) {
    const short = fullKey.replace(/^xerj\./, '');
    if (server[short] !== undefined && server[short] !== null) {
      out[short] = typeof server[short] === 'string' ? server[short] : JSON.stringify(server[short]);
    }
  }
  const coerceMap = (m) => {
    const o = {};
    for (const [id, raw] of Object.entries(m || {})) {
      if (raw == null) continue;
      o[id] = typeof raw === 'string' ? raw : JSON.stringify(raw);
    }
    return o;
  };
  if (server.layouts && typeof server.layouts === 'object') {
    const l = coerceMap(server.layouts);
    if (Object.keys(l).length) out.layouts = l;
  }
  if (server.filters && typeof server.filters === 'object') {
    const f = coerceMap(server.filters);
    if (Object.keys(f).length) out.filters = f;
  }
  return stableStringify(out);
}

function readViewsRaw() {
  return localStorage.getItem(VIEWS_KEY) || '';
}

async function pushIfDirty() {
  // Prefs
  const cur = snapshotPrefs();
  if (cur !== lastPushedPrefs) {
    try {
      await api('PUT', '/prefs', JSON.parse(cur));
      lastPushedPrefs = cur;
    } catch (e) {
      // Silently retry on the next tick; don't spam the console.
    }
  }
  // Views — naïve full-array sync. Cheap because the array is small
  // (low tens) and the SPA writes it as one JSON blob anyway. We
  // diff against the local raw to skip no-op pushes.
  const v = readViewsRaw();
  if (v !== lastPushedViewsKey) {
    lastPushedViewsKey = v;
    try {
      const arr = v ? JSON.parse(v) : [];
      // The /views surface is collection-style: figure out which views
      // are new vs deleted vs unchanged, then POST/DELETE accordingly.
      // For v1.0 we keep it dirt simple: re-create everything from
      // scratch.  Acceptable because views are <10 per user; if this
      // ever bites, replace with a proper diff.
      const server = await api('GET', '/views').catch(() => ({ views: [] }));
      const serverIds = new Set((server.views || []).map(s => s.id));
      const localIds  = new Set(arr.map(it => it.id).filter(Boolean));
      // Delete server-side views that no longer exist locally.
      for (const id of serverIds) {
        if (!localIds.has(id)) await api('DELETE', `/views/${encodeURIComponent(id)}`).catch(() => {});
      }
      // POST new ones.
      for (const item of arr) {
        if (item.id && serverIds.has(item.id)) continue;
        await api('POST', '/views', {
          dashboard_id: item.dashboard_id || item.dashboardId || 'default',
          name:         item.name || 'Untitled',
          time:         item.time || null,
          filters:      item.filters || null,
        }).catch(() => {});
      }
    } catch {}
  }
}

// ─── Public bootstrapper ─────────────────────────────────────────────────────

/** One-shot: pull then start periodic push. Call once at SPA load. */
export async function init() {
  await pullAll();
  startPush();
}
