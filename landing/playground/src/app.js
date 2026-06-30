// ============================================================
// XERJ.ai — Bootstrap + router + layout engine
//
// One render loop. Edit mode layers drag/resize/remove/add on
// top of the same render — no separate editor routes.
// ============================================================

import { registry, defaults, SECTIONS, DASHBOARD_GROUPS, dashboardsInSection } from './dashboards/registry.js';
import { Nav, SceneHeader, TimeCtrl, RefreshCtrl, FilterBar, ClusterCtrl, SavedViews, Footer, MobileCtrl } from './ux/chrome.js';
import { query, dataSourceStatus } from './data/query.js';
import { chartTypes, chartTypeList } from './ux/chart-types.js';
import { esc } from './ux/text.js';
import { mockSearch } from './data/mock.js';
import { hitsToCsv, downloadText, svgToPng } from './data/export.js';
import {
  mergedDashboards, renameDashboard, reorderDashboards, setHidden,
  createUserDashboard, deleteUserDashboard, isUserDash, resetAll as resetDashboards,
} from './data/dashboard-store.js';
import {
  listClusters, listIndices, listFields, listClustersSync,
  defaultClusterId, setDefaultCluster,
} from './data/data-sources.js';

// ---------- state -----------------------------------------
const LS = {
  theme:   'xerj.theme',
  time:    'xerj.time',
  edit:    'xerj.edit',
  search:  'xerj.search',
  refresh: 'xerj.refresh',
  cluster: 'xerj.cluster',
  views:   'xerj.views',
  filters: (dashId) => 'xerj.filters.' + dashId,
  layout:  (dashId) => 'xerj.layout.' + dashId,
};

function loadJSON(key, fallback) {
  try { const raw = localStorage.getItem(key); return raw ? JSON.parse(raw) : fallback; }
  catch { return fallback; }
}
function loadSearch() { return loadJSON(LS.search, null); }
function loadAllFilters() {
  const out = {};
  for (const k of Object.keys(localStorage)) {
    if (k.startsWith('xerj.filters.')) {
      const id = k.slice('xerj.filters.'.length);
      try { out[id] = JSON.parse(localStorage.getItem(k)) || {}; } catch {}
    }
  }
  return out;
}

const _initial = parseRoute();
const _urlState = parseUrlState();
const state = {
  section: _initial.section,
  route:   _initial.route,
  time:    _urlState.time    || localStorage.getItem(LS.time)    || '24H',
  timeCustom: {
    from: _urlState.tfrom || localStorage.getItem('xerj.timeFrom') || '',
    to:   _urlState.tto   || localStorage.getItem('xerj.timeTo')   || '',
  },
  cluster: _urlState.cluster || defaultClusterId(),
  refresh: parseInt(localStorage.getItem(LS.refresh) || '0', 10),  // ms; 0 = off
  refreshTimer: null,
  fetchedAt: null,                // ISO string of last successful query
  fetchMs:   null,                // last fetch duration in ms
  fetchErr:  null,                // last fetch error message
  loading:   false,
  filters:   loadAllFilters(),    // { [dashId]: { field: value } }
  theme: document.documentElement.getAttribute('data-theme') || 'night',
  edit:  localStorage.getItem(LS.edit) === '1',
  mobile: localStorage.getItem('xerj.mobile') === '1',
  layouts: loadAllLayouts(),
  search: (() => {
    const loaded = loadSearch() || {};
    return {
      q: loaded.q || '',
      type: loaded.type || 'match',
      index: loaded.index || '*',
      filters: loaded.filters || {},
      sort: loaded.sort || { field: '_score', dir: 'desc' },
      showTime: loaded.showTime !== false,
      result: null,
    };
  })(),
  _focusSelector: null, // set by handlers that need focus restored
};
// Merge any URL-seeded filters into the current dashboard's filter set.
if (_urlState.filters) {
  state.filters[_initial.route] = { ...(state.filters[_initial.route] || {}), ..._urlState.filters };
  saveFilters(_initial.route);
}

// ---------- filters, URL state, refresh -------------------
function currentFilters() {
  return state.filters[state.route] || {};
}
function saveFilters(dashId) {
  const v = state.filters[dashId];
  if (v && Object.keys(v).length) localStorage.setItem(LS.filters(dashId), JSON.stringify(v));
  else localStorage.removeItem(LS.filters(dashId));
}
// Toggle a filter value. Multi-value semantics: clicking the same
// value twice removes it; clicking a new value on an existing field
// appends to the OR list.
function setFilter(field, value) {
  const dashId = state.route;
  const cur = { ...(state.filters[dashId] || {}) };
  const existing = cur[field];
  if (existing == null) {
    cur[field] = value;
  } else if (Array.isArray(existing)) {
    const idx = existing.indexOf(value);
    if (idx >= 0) {
      const next = existing.filter((v) => v !== value);
      if (next.length === 0) delete cur[field];
      else if (next.length === 1) cur[field] = next[0];
      else cur[field] = next;
    } else {
      cur[field] = [...existing, value];
    }
  } else {
    if (existing === value) delete cur[field];
    else cur[field] = [existing, value];
  }
  state.filters[dashId] = cur;
  saveFilters(dashId);
  writeUrlState();
}

// Remove one value from a field (used by pill ✕ click on a specific
// chip inside a multi-value filter).
function removeFilterValue(field, value) {
  const dashId = state.route;
  const cur = { ...(state.filters[dashId] || {}) };
  const existing = cur[field];
  if (existing == null) return;
  if (Array.isArray(existing)) {
    const next = existing.filter((v) => v !== value);
    if (next.length === 0) delete cur[field];
    else if (next.length === 1) cur[field] = next[0];
    else cur[field] = next;
  } else if (existing === value) {
    delete cur[field];
  }
  state.filters[dashId] = cur;
  saveFilters(dashId);
  writeUrlState();
}

// KQL-lite parser: tokenize `field:value field:"multi word" foo`.
// Free-text tokens become a synthetic `q` filter; repeat fields
// turn into OR arrays.
function parseKql(text) {
  const out = {};
  const add = (k, v) => {
    if (!v) return;
    if (out[k] == null) out[k] = v;
    else if (Array.isArray(out[k])) out[k].push(v);
    else out[k] = [out[k], v];
  };
  // Match field:"quoted value"  OR  field:value  OR  loose token
  const re = /([a-zA-Z_][\w.]*):"([^"]*)"|([a-zA-Z_][\w.]*):(\S+)|"([^"]+)"|(\S+)/g;
  let m;
  while ((m = re.exec(text)) !== null) {
    if (m[1] != null)      add(m[1], m[2]);
    else if (m[3] != null) add(m[3], m[4]);
    else if (m[5] != null) add('q', m[5]);
    else if (m[6] != null) add('q', m[6]);
  }
  return out;
}

// Inverse of parseKql — render a filter object as a KQL string that
// re-parses to the same shape. Used to keep the input box in sync.
function serializeKql(filters) {
  const parts = [];
  for (const [k, v] of Object.entries(filters || {})) {
    const values = Array.isArray(v) ? v : [v];
    for (const x of values) {
      const needsQuote = /\s/.test(String(x));
      if (k === 'q') {
        parts.push(needsQuote ? `"${x}"` : String(x));
      } else {
        parts.push(needsQuote ? `${k}:"${x}"` : `${k}:${x}`);
      }
    }
  }
  return parts.join(' ');
}

function applyKqlInput(text) {
  const dashId = state.route;
  state.filters[dashId] = parseKql(text);
  saveFilters(dashId);
  writeUrlState();
}
function clearFilters() {
  const dashId = state.route;
  state.filters[dashId] = {};
  saveFilters(dashId);
  writeUrlState();
}

// URL state: ?t=24H&f=service:auth,level:error&c=prod-us
// Filters are encoded as repeated `f` params so URLSearchParams can
// handle escaping cleanly: ?f=service:nginx&f=level:error. Multi-value
// (OR) filters repeat the same key: ?f=service:auth&f=service:web.
function parseUrlState() {
  const qs = location.search.slice(1);
  if (!qs) return {};
  const out = {};
  const params = new URLSearchParams(qs);
  if (params.get('t')) out.time = params.get('t').toUpperCase();
  if (params.get('tfrom')) out.tfrom = params.get('tfrom');
  if (params.get('tto'))   out.tto   = params.get('tto');
  if (params.get('c')) out.cluster = params.get('c');
  const all = params.getAll('f');
  if (all.length) {
    out.filters = {};
    for (const pair of all) {
      const idx = pair.indexOf(':');
      if (idx > 0) {
        const k = pair.slice(0, idx);
        const v = pair.slice(idx + 1);
        const existing = out.filters[k];
        if (existing == null) out.filters[k] = v;
        else if (Array.isArray(existing)) existing.push(v);
        else out.filters[k] = [existing, v];
      }
    }
  }
  return out;
}
function writeUrlState() {
  const params = new URLSearchParams();
  if (state.time && state.time !== '24H') params.set('t', state.time);
  if (state.time === 'CUSTOM' && state.timeCustom) {
    if (state.timeCustom.from) params.set('tfrom', state.timeCustom.from);
    if (state.timeCustom.to)   params.set('tto',   state.timeCustom.to);
  }
  if (state.cluster) params.set('c', state.cluster);
  for (const [k, v] of Object.entries(currentFilters())) {
    if (Array.isArray(v)) for (const x of v) params.append('f', `${k}:${x}`);
    else params.append('f', `${k}:${v}`);
  }
  const qs = params.toString();
  const url = location.pathname + (qs ? '?' + qs : '') + location.hash;
  history.replaceState(null, '', url);
}

function setRefreshInterval(ms) {
  state.refresh = ms;
  localStorage.setItem(LS.refresh, String(ms));
  if (state.refreshTimer) { clearInterval(state.refreshTimer); state.refreshTimer = null; }
  if (ms > 0) {
    state.refreshTimer = setInterval(() => {
      if (state.section === 'dashboards' || state.section === 'discover') render();
    }, ms);
  }
}

// ---------- saved views -----------------------------------
// A saved view is a snapshot of (dashId, time, cluster, filters).
// Click it to land on that dashboard with the same context.
function loadViews() { return loadJSON(LS.views, []); }
function saveViews(views) { localStorage.setItem(LS.views, JSON.stringify(views)); }
function saveCurrentView() {
  const dashId = state.route;
  const all = loadViews();
  const name = prompt('Save view as:', 'View ' + (all.length + 1));
  if (!name) return;
  const id = 'v-' + Date.now().toString(36);
  all.push({
    id,
    name,
    dashId,
    time: state.time,
    cluster: state.cluster,
    filters: currentFilters(),
    savedAt: new Date().toISOString(),
  });
  saveViews(all);
}
function deleteView(id) {
  saveViews(loadViews().filter((v) => v.id !== id));
}
function applyView(id) {
  const v = loadViews().find((x) => x.id === id);
  if (!v) return;
  state.time = v.time;
  state.cluster = v.cluster || state.cluster;
  state.filters[v.dashId] = { ...(v.filters || {}) };
  saveFilters(v.dashId);
  localStorage.setItem(LS.time, state.time);
  location.hash = '#/dashboards/' + v.dashId;
}

function relTime(iso) {
  if (!iso) return '';
  const delta = Date.now() - new Date(iso).getTime();
  if (delta < 2000) return 'just now';
  if (delta < 60_000) return Math.round(delta / 1000) + 's ago';
  if (delta < 3600_000) return Math.round(delta / 60_000) + 'm ago';
  return Math.round(delta / 3600_000) + 'h ago';
}

function navStatus() {
  if (state.loading) return 'LOADING…';
  if (state.fetchErr) return 'ERROR · ' + state.fetchErr.slice(0, 40);
  const bits = [];
  bits.push(dataSourceStatus);
  if (state.fetchedAt) bits.push('UPDATED ' + relTime(state.fetchedAt).toUpperCase());
  if (state.fetchMs != null) bits.push(state.fetchMs + 'MS');
  return bits.join(' · ');
}

function runSearchNow() {
  state.search.result = mockSearch({
    q: state.search.q,
    type: state.search.type,
    index: state.search.index,
    filters: state.search.filters,
    sort: state.search.sort,
  });
  // Persist the inputs (not the result — it rebuilds on demand)
  localStorage.setItem(LS.search, JSON.stringify({
    q: state.search.q,
    type: state.search.type,
    index: state.search.index,
    filters: state.search.filters,
    sort: state.search.sort,
    showTime: state.search.showTime,
  }));
}

/**
 * Parse the hash into { section, route }. Routes come in three shapes:
 *
 *   #/                     → section=dashboards, route=<first dashboard>
 *   #/dashboards           → section=dashboards, route=<first dashboard>
 *   #/dashboards/:id       → section=dashboards, route=:id
 *   #/<section>            → section=<section>,  route=<view id for that section>
 *
 * Backward compat: legacy `#/ai-overview` (bare dashboard id) gets
 * auto-routed into `#/dashboards/ai-overview`, and legacy `#/manage`
 * redirects to `#/settings`.
 */
function parseRoute() {
  const raw = (location.hash || '').replace(/^#\/?/, '');
  const parts = raw.split('/').filter(Boolean);
  const merged = mergedDashboards(defaults, { includeHidden: true });

  // Helper: find the default view id for a section.
  const firstOfSection = (sectionId) => {
    const list = dashboardsInSection(sectionId, merged);
    return list[0]?.id;
  };

  // Empty hash → dashboards section, first dashboard
  if (!parts.length) {
    const first = firstOfSection('dashboards') || 'ai-overview';
    return { section: 'dashboards', route: first };
  }

  // Legacy: #/manage → settings
  if (parts[0] === 'manage') return { section: 'settings', route: 'settings' };

  // Section-level route: #/section or #/section/:id
  const sectionIds = new Set(SECTIONS.map((s) => s.id));
  if (sectionIds.has(parts[0])) {
    if (parts[0] === 'dashboards') {
      const id = parts[1] || firstOfSection('dashboards') || 'ai-overview';
      // Only accept real dashboards in this section
      const known = merged.find((d) => d.id === id && (d.section || 'dashboards') === 'dashboards');
      if (known) return { section: 'dashboards', route: id };
      return { section: 'dashboards', route: firstOfSection('dashboards') || 'ai-overview' };
    }
    // Non-dashboards sections have exactly one view: firstOfSection
    const first = firstOfSection(parts[0]);
    if (first) return { section: parts[0], route: first };
  }

  // Legacy bare route: #/ai-overview → #/dashboards/ai-overview
  if (merged.some((d) => d.id === parts[0])) {
    const d = merged.find((x) => x.id === parts[0]);
    const section = d.section || 'dashboards';
    return { section, route: parts[0] };
  }

  // Fallback
  return { section: 'dashboards', route: firstOfSection('dashboards') || 'ai-overview' };
}

// Build the data context that the section views (DATA / SETTINGS)
// need. Async because real API calls will replace these mocks in one
// step once the engine ships /v1/clusters etc.
async function buildSectionData(sectionId) {
  if (sectionId === 'data') {
    const clusters = await listClusters();
    const indicesByCluster = {};
    const fieldsByIndex = {};
    for (const c of clusters) {
      indicesByCluster[c.id] = await listIndices(c.id);
    }
    const active = defaultClusterId();
    for (const i of indicesByCluster[active] || []) {
      fieldsByIndex[i.name] = await listFields(i.name);
    }
    return {
      clusters,
      indicesByCluster,
      fieldsByIndex,
      activeCluster: active,
      focusIndex: state._focusIndex || (indicesByCluster[active] || [])[0]?.name,
    };
  }
  if (sectionId === 'settings') {
    return {
      dashboards: mergedDashboards(defaults, { includeHidden: true }),
      views: loadViews(),
    };
  }
  return {};
}

function loadAllLayouts() {
  const out = {};
  for (const id of Object.keys(registry)) {
    try {
      const raw = localStorage.getItem(LS.layout(id));
      if (raw) out[id] = JSON.parse(raw);
    } catch { /* ignore corrupt */ }
  }
  return out;
}
function saveLayout(dashId) {
  const v = state.layouts[dashId];
  if (!v) localStorage.removeItem(LS.layout(dashId));
  else    localStorage.setItem(LS.layout(dashId), JSON.stringify(v));
}
function applyTheme(t) {
  state.theme = t;
  document.documentElement.setAttribute('data-theme', t);
  localStorage.setItem(LS.theme, t);
}

// ---------- layout engine ---------------------------------
/**
 * Merge a dashboard's default panels with the user's stored override.
 * Returns a flat ordered array of panel objects ready to render.
 */
function mergeLayout(defaultPanels, override) {
  if (!override) return defaultPanels.map((p) => ({ ...p, source: 'default' }));
  const byId = Object.fromEntries(defaultPanels.map((p) => [p.id, p]));
  const hidden = new Set(override.hidden || []);
  const out = [];
  const seen = new Set();

  const order = override.order || defaultPanels.map((p) => p.id);
  for (const id of order) {
    if (hidden.has(id)) { seen.add(id); continue; }
    const base = byId[id];
    if (base) {
      const cols = override.cols?.[id] ?? base.cols;
      out.push({ ...base, cols, source: 'default' });
      seen.add(id);
    } else {
      // Maybe a user-added panel.
      const added = (override.added || []).find((a) => a.id === id);
      if (added) {
        const cols = override.cols?.[id] ?? added.cols;
        out.push({ ...added, cols, source: 'added' });
        seen.add(id);
      }
    }
  }
  // Append defaults missing from order (new panels added since override was saved).
  for (const p of defaultPanels) {
    if (!seen.has(p.id) && !hidden.has(p.id)) {
      const cols = override.cols?.[p.id] ?? p.cols;
      out.push({ ...p, cols, source: 'default' });
    }
  }
  // Append user-added panels missing from order (freshly added).
  for (const added of (override.added || [])) {
    if (!seen.has(added.id)) {
      const cols = override.cols?.[added.id] ?? added.cols;
      out.push({ ...added, cols, source: 'added' });
    }
  }
  return out;
}

function ensureOverride(dashId, defaultPanels) {
  if (state.layouts[dashId]) return state.layouts[dashId];
  const fresh = {
    order: defaultPanels.map((p) => p.id),
    cols:  {},
    hidden: [],
    added:  [],
  };
  state.layouts[dashId] = fresh;
  return fresh;
}

function mutate(dashId, defaultPanels, fn) {
  const ov = ensureOverride(dashId, defaultPanels);
  fn(ov);
  saveLayout(dashId);
}

// ---------- render ----------------------------------------
const SIZES = [2, 3, 4, 6, 8, 12];

function renderEditChrome(p) {
  const sizes = SIZES.map((s) =>
    `<button type="button" data-panel="${esc(p.id)}" data-size="${s}" class="${p.cols === s ? 'active' : ''}" aria-pressed="${p.cols === s}">${s}</button>`
  ).join('<span class="sep">·</span>');
  const frac = Math.max(0, Math.min(12, p.cols)) / 12;
  const meterFill = (frac * 96).toFixed(1);
  const meter = `
    <svg class="meter" viewBox="0 0 96 6" preserveAspectRatio="none" aria-hidden="true">
      <line x1="0" y1="5" x2="96" y2="5" stroke="currentColor" stroke-width="1" stroke-opacity="0.25"/>
      <line x1="0" y1="5" x2="${meterFill}" y2="5" stroke="var(--z-accent)" stroke-width="1"/>
    </svg>`;
  return `
  <div class="panel-edit" aria-label="Edit panel">
    <span class="colsLabel"><span class="max">COL</span> ${p.cols}<span class="slash">/</span><span class="max">12</span></span>
    ${meter}
    <span class="sizes">${sizes}</span>
    <button type="button" class="remove" data-panel="${esc(p.id)}" data-remove aria-label="Remove">✕</button>
  </div>`;
}

function renderPanel(p, data, editMode) {
  let inner = '';
  try {
    if (p.source === 'added') {
      const t = chartTypes[p.type] || chartTypes.markdown;
      inner = t.render(data);
    } else if (typeof p.render === 'function') {
      inner = p.render({ data });
    } else {
      inner = '';
    }
    // Empty-state fallback: if the render produced nothing meaningful,
    // show a typography-first placeholder so users see WHY the slot is
    // blank (e.g. filtered everything out) rather than a broken panel.
    const stripped = String(inner || '').replace(/<[^>]+>/g, '').trim();
    if (!stripped) {
      inner = `<div class="panel-empty mono faint">NO DATA · ADJUST FILTERS OR TIME RANGE</div>`;
    }
  } catch (err) {
    inner = `<div class="panel-empty mono faint">PANEL ERROR · ${esc((err.message || err) + '').slice(0, 80)}</div>`;
  }
  const editAttrs = editMode ? ' draggable="true"' : '';
  const editChrome = editMode ? renderEditChrome(p) : '';
  // A panel that declares `drilldown: { to: <dashId> }` adds a
  // data attribute the global click handler uses to route clicks
  // to the target dashboard with the clicked filter preserved.
  const drillAttr = p.drilldown?.to ? ` data-drilldown-to="${esc(p.drilldown.to)}"` : '';
  const drillHint = p.drilldown?.to ? `<span class="drill-hint mono faint">→ ${esc(p.drilldown.to.toUpperCase())}</span>` : '';
  return `
  <section class="panel${editMode ? ' edit' : ''}" data-panel="${esc(p.id)}" style="grid-column: span ${p.cols};"${editAttrs}${drillAttr}>
    ${editChrome}
    ${p.eyebrow ? `<div class="key">${esc(p.eyebrow)}${drillHint}</div>` : ''}
    ${inner}
  </section>`;
}

function renderAddPicker() {
  const items = chartTypeList.map((t) =>
    `<button type="button" data-add="${esc(t.id)}" title="${esc(t.describe || '')}">${esc(t.name)}</button>`
  ).join('<span class="sep">·</span>');
  return `
  <div class="add-picker">
    <span class="key" style="color:var(--z-accent);">+ ADD PANEL</span>
    <span class="types">${items}</span>
  </div>`;
}

async function render() {
  const app = document.getElementById('app');
  // Resolve the active dashboard from either the default registry or
  // the user metadata store. User-cloned dashboards inherit their
  // render from their clonedFrom template.
  const allDash = mergedDashboards(defaults, { includeHidden: true });
  const dash = allDash.find((d) => d.id === state.route)
    || dashboardsInSection(state.section, allDash)[0]
    || registry['ai-overview'];
  const navDash = mergedDashboards(defaults);
  const dashboardsForSection = dashboardsInSection('dashboards', navDash);

  // For the SEARCH dashboard, eagerly run the search so panels see hits.
  if (dash.id === 'search-discover' && !state.search.result) {
    runSearchNow();
  }

  const activeFilters = currentFilters();
  let data;
  let fetchErr = null;
  // Section-level views (data/settings/users/alerts) get their own
  // context builder. Everything else goes through the regular query.
  if (state.section === 'data' || state.section === 'settings') {
    try {
      data = await buildSectionData(state.section);
    } catch (err) {
      data = {};
      fetchErr = err;
    }
  } else {
    state.loading = true;
    try {
      const result = await query({
        dashId: dash.id,
        range: state.time,
        customRange: state.time === 'CUSTOM' ? state.timeCustom : null,
        cluster: state.cluster,
        filters: activeFilters,
      });
      data = result.data;
      state.fetchedAt = result.meta.fetchedAt;
      state.fetchMs = result.meta.durationMs;
      state.fetchErr = null;
    } catch (err) {
      state.fetchErr = err.message || String(err);
      fetchErr = err;
    } finally {
      state.loading = false;
    }
    if (fetchErr) {
      app.innerHTML = `
        ${Nav({ sections: SECTIONS, activeSection: state.section, dashboards: dashboardsForSection, groups: DASHBOARD_GROUPS, activeDash: dash.id, theme: state.theme, edit: state.edit, status: navStatus() })}
        <div class="scene"><div class="key" style="margin-bottom:12px;">ERROR</div><h1 class="h-scene">CANNOT LOAD</h1></div>
        <pre class="mono faint" style="white-space:pre-wrap; font-size:var(--fs-13);">${esc((fetchErr && fetchErr.stack) || fetchErr)}</pre>
        <div style="margin-top:var(--sp-6);"><button type="button" data-retry class="text-btn">RETRY</button></div>`;
      return;
    }
  }

  const view = dash.render({ data, time: state.time, search: state.search });
  // Let a user rename override the scene title. User-cloned dashboards
  // always use the user-chosen name; defaults only override if the user
  // has explicitly set an xerj.dashboards.names entry.
  try {
    const store = JSON.parse(localStorage.getItem('xerj.dashboards') || '{}');
    if (dash.isUser || (store.names && store.names[dash.id])) {
      view.title = (dash.name || view.title || '').toUpperCase();
    }
  } catch {}
  const merged = mergeLayout(view.panels, state.layouts[dash.id]);
  const panelsHtml = merged.length
    ? merged.map((p) => renderPanel(p, data, state.edit)).join('')
    : `<div class="mono faint" style="grid-column: span 12; padding:var(--sp-6) 0;">All panels hidden. Click RESET to restore defaults.</div>`;

  // Police-strip frame: a fixed-position overlay that lives in the page's
  // existing margins, so entering edit mode never shifts content. The frame
  // itself is a 1px dashed border around the viewport; the top strip carries
  // the EDIT MODE label and tips; the bottom strip carries the column index.
  const editFrame = state.edit ? `
    <div class="edit-frame" aria-hidden="true"></div>
    <div class="edit-strip-top" role="status">
      <span class="marker">EDIT MODE</span>
      <span class="tips">
        DRAG PANEL TO REORDER ·
        CLICK A <span class="key-bind">NUMBER</span> TO RESIZE ·
        <span class="key-bind">✕</span> TO REMOVE ·
        SCROLL FOR <span class="key-bind">+ ADD</span>
      </span>
      <span class="meta">${esc(dash.name.toUpperCase())} · ${merged.length} PANELS</span>
    </div>
    <div class="edit-strip-bottom" aria-hidden="true">${
      Array.from({ length: 12 }, (_, i) => `<span>${String(i + 1).padStart(2, '0')}</span>`).join('')
    }</div>
  ` : '';
  const gridOverlay = state.edit
    ? `<div class="edit-grid" aria-hidden="true">${'<span></span>'.repeat(12)}</div>`
    : '';
  const addHtml = state.edit ? renderAddPicker() : '';

  // Section-level views (alerts, data, users, settings) don't show the
  // inline time control — the time range is a dashboards-section concept.
  const hideTimeCtrl = state.section !== 'dashboards' && state.section !== 'discover';
  const showFilterBar = state.section === 'dashboards';
  const filterBarHtml = showFilterBar
    ? FilterBar({ filters: activeFilters, kql: serializeKql(activeFilters) })
    : '';
  const refreshHtml = hideTimeCtrl
    ? ''
    : RefreshCtrl({ active: state.refresh });
  const clusterHtml = hideTimeCtrl
    ? ''
    : ClusterCtrl({ clusters: listClustersSync(), active: state.cluster });
  const savedViewsHtml = showFilterBar
    ? SavedViews({ views: loadViews(), dashId: dash.id })
    : '';

  app.innerHTML = `
    ${Nav({
      sections: SECTIONS,
      activeSection: state.section,
      dashboards: dashboardsForSection,
      groups: DASHBOARD_GROUPS,
      activeDash: dash.id,
      theme: state.theme,
      edit: state.edit,
      mobile: state.mobile,
      status: navStatus(),
    })}
    ${state.mobile ? '<div class="iphone-frame"><div class="iphone-notch"></div><div class="iphone-screen">' : ''}
    ${SceneHeader({
      title: view.title, kicker: view.kicker || 'OBSERVE', meta: view.meta || [state.time],
      editable: state.edit, dashId: dash.id,
    })}
    ${view.caption ? `<p class="caption">${esc(view.caption)}</p>` : ''}
    ${hideTimeCtrl ? '' : `<div class="dash-ctrls">${TimeCtrl({ active: state.time, custom: state.timeCustom })}${refreshHtml}${clusterHtml}</div>`}
    ${filterBarHtml}
    ${savedViewsHtml}
    <main class="canvas${state.edit ? ' edit' : ''}" aria-label="${esc(dash.name)}">${gridOverlay}${panelsHtml}</main>
    ${addHtml}
    ${state.mobile ? '</div><div class="iphone-home-bar"></div></div>' : ''}
    ${Footer()}
    ${editFrame}
  `;
  app.setAttribute('aria-busy', 'false');

  // Restore focus on the search input (if we're on the search dashboard).
  if (state._focusSelector) {
    const el = document.querySelector(state._focusSelector);
    if (el) {
      el.focus();
      if (el.tagName === 'INPUT') {
        const v = el.value;
        el.setSelectionRange(v.length, v.length);
      }
    }
    state._focusSelector = null;
  }
}

// ---------- event delegation ------------------------------
let dragSrcId = null;

document.addEventListener('click', (e) => {
  // Primary nav: top-level product section
  const secA = e.target.closest('[data-section]');
  if (secA) {
    e.preventDefault();
    const sid = secA.getAttribute('data-section');
    if (sid === 'dashboards') {
      location.hash = '#/dashboards';
    } else {
      location.hash = '#/' + sid;
    }
    return;
  }
  // Secondary nav: a specific dashboard inside the dashboards section
  const dashA = e.target.closest('[data-dash]');
  if (dashA) {
    e.preventDefault();
    const id = dashA.getAttribute('data-dash');
    location.hash = '#/dashboards/' + id;
    return;
  }
  // Secondary nav: a collapsed group tab — switch groups by landing
  // on the group's first dashboard.
  const groupBtn = e.target.closest('[data-dash-group]');
  if (groupBtn) {
    e.preventDefault();
    const firstId = groupBtn.getAttribute('data-dash-group-first');
    if (firstId) location.hash = '#/dashboards/' + firstId;
    return;
  }
  // Time range
  const tb = e.target.closest('[data-time]');
  if (tb) {
    state.time = tb.getAttribute('data-time');
    localStorage.setItem(LS.time, state.time);
    // When switching INTO custom and we have no stored endpoints,
    // seed a sensible default (now − 24h → now).
    if (state.time === 'CUSTOM' && !state.timeCustom.from) {
      const now = new Date();
      const prev = new Date(now.getTime() - 24 * 3600 * 1000);
      const fmt = (d) => d.toISOString().slice(0, 16);
      state.timeCustom = { from: fmt(prev), to: fmt(now) };
      localStorage.setItem('xerj.timeFrom', state.timeCustom.from);
      localStorage.setItem('xerj.timeTo',   state.timeCustom.to);
    }
    writeUrlState();
    render();
    return;
  }
  // Refresh interval
  const rf = e.target.closest('[data-refresh]');
  if (rf) {
    setRefreshInterval(parseInt(rf.getAttribute('data-refresh'), 10));
    render();
    return;
  }
  // Manual retry on error
  if (e.target.closest('[data-retry]')) {
    render();
    return;
  }
  // Global filter bar: click-to-filter (used by FilterBar pills and
  // any click-to-filter affordance inside a panel — TopN rows, Dist
  // segments, heatmap cells).
  const fAdd = e.target.closest('[data-filter-add]');
  if (fAdd) {
    const raw = fAdd.getAttribute('data-filter-add');
    const idx = raw.indexOf(':');
    if (idx > 0) {
      const field = raw.slice(0, idx);
      const value = raw.slice(idx + 1);
      // Drilldown takes precedence when the panel declares one.
      const drillHost = fAdd.closest('[data-drilldown-to]');
      if (drillHost) {
        const toId = drillHost.getAttribute('data-drilldown-to');
        // Seed the target dashboard's filters with this value before
        // navigating so the destination renders pre-filtered. The
        // search-discover dashboard has its own filter store
        // (state.search.filters) — seed that one instead.
        if (toId === 'search-discover') {
          state.search.filters = { ...(state.search.filters || {}), [field]: value };
          state.search.result = null;  // force re-run on render
        } else {
          state.filters[toId] = { ...(state.filters[toId] || {}), [field]: value };
          try { localStorage.setItem(LS.filters(toId), JSON.stringify(state.filters[toId])); } catch {}
        }
        // Resolve the target's actual section — some targets (like
        // search-discover) live outside the 'dashboards' section and
        // need a different hash shape (#/discover rather than
        // #/dashboards/search-discover).
        const all = mergedDashboards(defaults, { includeHidden: true });
        const target = all.find((d) => d.id === toId);
        const sectionId = target?.section || 'dashboards';
        location.hash = sectionId === 'dashboards' ? `#/dashboards/${toId}` : `#/${sectionId}`;
        return;
      }
      setFilter(field, value);
      render();
    }
    return;
  }
  // Remove one chip from a filter pill (multi-value OR filters).
  const fRem = e.target.closest('[data-filter-remove]');
  if (fRem) {
    const raw = fRem.getAttribute('data-filter-remove');
    const idx = raw.indexOf(':');
    if (idx > 0) {
      removeFilterValue(raw.slice(0, idx), raw.slice(idx + 1));
      render();
    }
    return;
  }
  if (e.target.closest('[data-filter-clear]')) {
    clearFilters();
    render();
    return;
  }
  // Cluster selector
  const cset = e.target.closest('[data-cluster-set]');
  if (cset) {
    state.cluster = cset.getAttribute('data-cluster-set');
    setDefaultCluster(state.cluster);
    writeUrlState();
    render();
    return;
  }
  // Save current view
  if (e.target.closest('[data-view-save]')) {
    saveCurrentView();
    render();
    return;
  }
  // Apply a saved view
  const vApply = e.target.closest('[data-view-apply]');
  if (vApply) {
    applyView(vApply.getAttribute('data-view-apply'));
    return;
  }
  // Delete a saved view
  const vDel = e.target.closest('[data-view-delete]');
  if (vDel) {
    const id = vDel.getAttribute('data-view-delete');
    if (confirm('Delete this view?')) {
      deleteView(id);
      render();
    }
    return;
  }
  // Theme
  const th = e.target.closest('[data-theme-set]');
  if (th) {
    applyTheme(th.getAttribute('data-theme-set'));
    render();
    return;
  }
  // Mobile preview toggle
  if (e.target.closest('[data-mobile-toggle]')) {
    state.mobile = !state.mobile;
    localStorage.setItem('xerj.mobile', state.mobile ? '1' : '0');
    document.documentElement.classList.toggle('mobile-preview', state.mobile);
    render();
    return;
  }
  // Edit toggle
  if (e.target.closest('[data-edit-toggle]')) {
    state.edit = !state.edit;
    localStorage.setItem(LS.edit, state.edit ? '1' : '0');
    render();
    return;
  }
  // Reset layout
  if (e.target.closest('[data-reset-layout]')) {
    const id = state.route;
    delete state.layouts[id];
    saveLayout(id);
    render();
    return;
  }
  // Resize (click a size number)
  const sz = e.target.closest('[data-size]');
  if (sz) {
    const pid = sz.getAttribute('data-panel');
    const cols = parseInt(sz.getAttribute('data-size'), 10);
    const dash = registry[state.route];
    const view = dash.render({ data: {}, time: state.time });
    mutate(state.route, view.panels, (ov) => { ov.cols[pid] = cols; });
    render();
    return;
  }
  // Remove panel
  const rm = e.target.closest('[data-remove]');
  if (rm) {
    const pid = rm.getAttribute('data-panel');
    const dash = registry[state.route];
    const view = dash.render({ data: {}, time: state.time });
    mutate(state.route, view.panels, (ov) => {
      if (!ov.hidden.includes(pid)) ov.hidden.push(pid);
      const idx = (ov.added || []).findIndex((a) => a.id === pid);
      if (idx >= 0) ov.added.splice(idx, 1);
    });
    render();
    return;
  }
  // Search: query type
  const qt = e.target.closest('[data-query-type]');
  if (qt) {
    state.search.type = qt.getAttribute('data-query-type');
    runSearchNow();
    state._focusSelector = '[data-search-input]';
    render();
    return;
  }
  // Search: index
  const si = e.target.closest('[data-search-index]');
  if (si) {
    state.search.index = si.getAttribute('data-search-index');
    runSearchNow();
    state._focusSelector = '[data-search-input]';
    render();
    return;
  }
  // Facet apply (click-to-filter) — also drives the click-to-filter on Hit
  // row values and the ✕ on filter pills (clicking an active value removes).
  const fa = e.target.closest('[data-facet-apply]');
  if (fa) {
    const raw = fa.getAttribute('data-facet-apply');
    const colonIdx = raw.indexOf(':');
    const field = raw.slice(0, colonIdx);
    const value = raw.slice(colonIdx + 1);
    const cur = state.search.filters[field];
    if (cur === value) delete state.search.filters[field];
    else state.search.filters[field] = value;
    runSearchNow();
    render();
    return;
  }
  // Clear all filters
  if (e.target.closest('[data-facet-clear]')) {
    state.search.filters = {};
    runSearchNow();
    render();
    return;
  }
  // Sort a Hits column (GH#696 / GH#737)
  const sortBtn = e.target.closest('[data-sort-field]');
  if (sortBtn) {
    state.search.sort = {
      field: sortBtn.getAttribute('data-sort-field'),
      dir:   sortBtn.getAttribute('data-sort-dir'),
    };
    runSearchNow();
    render();
    return;
  }
  // Toggle time-column visibility (GH#3319)
  if (e.target.closest('[data-toggle-time]')) {
    state.search.showTime = !state.search.showTime;
    runSearchNow();
    render();
    return;
  }
  // Export Hits as CSV (GH#1992 — the #1 ask of all time)
  if (e.target.closest('[data-export-csv]')) {
    const hits = state.search.result?.hits || [];
    const cols = state.search.showTime
      ? ['_index', '_id', '_score', '_ts', '_source']
      : ['_index', '_id', '_score', '_source'];
    downloadText('xerj-search-' + new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-') + '.csv',
      'text/csv', hitsToCsv(hits, cols));
    return;
  }
  // Export a panel chart as PNG (GH#1366 — 178 reactions)
  const pngBtn = e.target.closest('[data-export-png]');
  if (pngBtn) {
    const sec = pngBtn.closest('.panel');
    const svg = sec?.querySelector('svg.chart, svg.series');
    if (svg) {
      const id = sec.getAttribute('data-panel') || 'panel';
      svgToPng(svg, { filename: 'xerj-' + id + '.png' }).catch((err) =>
        console.error('PNG export failed', err));
    }
    return;
  }
  // --- MANAGE view click handlers -------------------------
  const mgUp = e.target.closest('[data-mg-up]');
  if (mgUp) {
    const id = mgUp.getAttribute('data-mg-up');
    const list = mergedDashboards(defaults, { includeHidden: true });
    const i = list.findIndex((d) => d.id === id);
    if (i > 0) {
      const order = list.map((d) => d.id);
      [order[i - 1], order[i]] = [order[i], order[i - 1]];
      reorderDashboards(order);
      render();
    }
    return;
  }
  const mgDown = e.target.closest('[data-mg-down]');
  if (mgDown) {
    const id = mgDown.getAttribute('data-mg-down');
    const list = mergedDashboards(defaults, { includeHidden: true });
    const i = list.findIndex((d) => d.id === id);
    if (i >= 0 && i < list.length - 1) {
      const order = list.map((d) => d.id);
      [order[i + 1], order[i]] = [order[i], order[i + 1]];
      reorderDashboards(order);
      render();
    }
    return;
  }
  const mgRename = e.target.closest('[data-mg-rename]');
  if (mgRename) {
    const id = mgRename.getAttribute('data-mg-rename');
    const current = mergedDashboards(defaults, { includeHidden: true }).find((d) => d.id === id);
    const name = prompt('Rename "' + (current?.name || id) + '":', current?.name || '');
    if (name != null) {
      renameDashboard(id, name);
      render();
    }
    return;
  }
  const mgHide = e.target.closest('[data-mg-hide]');
  if (mgHide) {
    const id = mgHide.getAttribute('data-mg-hide');
    const list = mergedDashboards(defaults, { includeHidden: true });
    const cur = list.find((d) => d.id === id);
    setHidden(id, !cur?.hidden);
    render();
    return;
  }
  const mgClone = e.target.closest('[data-mg-clone]');
  if (mgClone) {
    const id = mgClone.getAttribute('data-mg-clone');
    const current = mergedDashboards(defaults, { includeHidden: true }).find((d) => d.id === id);
    const name = prompt('Clone "' + (current?.name || id) + '" as:', (current?.name || 'Untitled') + ' (copy)');
    if (name) {
      const newId = createUserDashboard({ name, fromId: id });
      location.hash = '#/dashboards/' + newId;
    }
    return;
  }
  const mgDelete = e.target.closest('[data-mg-delete]');
  if (mgDelete) {
    const id = mgDelete.getAttribute('data-mg-delete');
    if (isUserDash(id) && confirm('Delete user dashboard "' + id + '"? This cannot be undone.')) {
      deleteUserDashboard(id);
      if (state.route === id) {
        location.hash = '#/settings';
      } else {
        render();
      }
    }
    return;
  }
  const mgNew = e.target.closest('[data-mg-new]');
  if (mgNew) {
    const fromId = mgNew.getAttribute('data-mg-new');
    const name = prompt(fromId ? 'New dashboard from "' + fromId + '" · Name:' : 'New blank dashboard · Name:', 'Untitled');
    if (name) {
      const newId = createUserDashboard({ name, fromId: fromId || null });
      location.hash = '#/dashboards/' + newId;
    }
    return;
  }
  const mgCluster = e.target.closest('[data-mg-cluster]');
  if (mgCluster) {
    setDefaultCluster(mgCluster.getAttribute('data-mg-cluster'));
    state._focusIndex = null;
    render();
    return;
  }
  const mgIndex = e.target.closest('[data-mg-index]');
  if (mgIndex) {
    state._focusIndex = mgIndex.getAttribute('data-mg-index');
    render();
    return;
  }
  if (e.target.closest('[data-mg-reset-all]')) {
    if (confirm('Reset ALL saved state? This wipes dashboards, layouts, theme, filters.')) {
      for (const k of Object.keys(localStorage)) {
        if (k.startsWith('xerj.')) localStorage.removeItem(k);
      }
      resetDashboards();
      location.reload();
    }
    return;
  }

  // Add panel (from ADD picker)
  const add = e.target.closest('[data-add]');
  if (add) {
    const typeId = add.getAttribute('data-add');
    const t = chartTypes[typeId];
    if (!t) return;
    const dash = registry[state.route];
    const view = dash.render({ data: {}, time: state.time });
    mutate(state.route, view.panels, (ov) => {
      ov.added = ov.added || [];
      const newId = typeId + '-' + Date.now().toString(36);
      ov.added.push({
        id: newId,
        type: typeId,
        eyebrow: t.name + ' · NEW',
        cols: t.cols || 6,
      });
      ov.order = ov.order || view.panels.map((p) => p.id);
      ov.order.push(newId);
    });
    render();
    // scroll to bottom so the new panel is visible
    requestAnimationFrame(() => window.scrollTo({ top: document.body.scrollHeight, behavior: 'smooth' }));
    return;
  }
});

// ---------- search input (Enter to run) -------------------
document.addEventListener('keydown', (e) => {
  // KQL-lite input on dashboards
  const kq = e.target.closest && e.target.closest('[data-kql-input]');
  if (kq) {
    if (e.key === 'Enter') {
      e.preventDefault();
      applyKqlInput(kq.value);
      state._focusSelector = '[data-kql-input]';
      render();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      kq.value = '';
      applyKqlInput('');
      state._focusSelector = '[data-kql-input]';
      render();
    }
    return;
  }
  const si = e.target.closest && e.target.closest('[data-search-input]');
  if (si) {
    if (e.key === 'Enter') {
      e.preventDefault();
      state.search.q = si.value;
      runSearchNow();
      state._focusSelector = '[data-search-input]';
      render();
    } else if (e.key === 'Escape') {
      si.value = '';
      state.search.q = '';
      state.search.filters = {};
      runSearchNow();
      state._focusSelector = '[data-search-input]';
      render();
    }
    return;
  }
  // Inline rename of the scene title in edit mode (contenteditable h1)
  const rn = e.target.closest && e.target.closest('[data-rename-dash]');
  if (rn) {
    if (e.key === 'Enter') {
      e.preventDefault();
      const id = rn.getAttribute('data-rename-dash');
      const name = rn.textContent.trim();
      renameDashboard(id, name);
      rn.blur();
      render();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      rn.blur();
      render();
    }
  }
});
// Custom time range — FROM/TO datetime-local inputs
document.addEventListener('change', (e) => {
  const tf = e.target.closest && e.target.closest('[data-time-from]');
  const tt = e.target.closest && e.target.closest('[data-time-to]');
  if (tf || tt) {
    state.timeCustom = {
      from: document.querySelector('[data-time-from]')?.value || '',
      to:   document.querySelector('[data-time-to]')?.value   || '',
    };
    localStorage.setItem('xerj.timeFrom', state.timeCustom.from);
    localStorage.setItem('xerj.timeTo',   state.timeCustom.to);
    writeUrlState();
    render();
  }
});

// blur-save for inline rename (clicking outside the title also commits)
document.addEventListener('blur', (e) => {
  const rn = e.target.closest && e.target.closest('[data-rename-dash]');
  if (!rn) return;
  const id = rn.getAttribute('data-rename-dash');
  const newName = rn.textContent.trim();
  renameDashboard(id, newName);
}, true);

// ---------- drag and drop ---------------------------------
document.addEventListener('dragstart', (e) => {
  if (!state.edit) return;
  const sec = e.target.closest('.panel.edit');
  if (!sec) return;
  dragSrcId = sec.getAttribute('data-panel');
  sec.classList.add('dragging');
  try {
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', dragSrcId);
  } catch {}
});
document.addEventListener('dragover', (e) => {
  if (!state.edit || !dragSrcId) return;
  const sec = e.target.closest('.panel.edit');
  if (!sec) return;
  e.preventDefault();
  document.querySelectorAll('.drop-before, .drop-after').forEach((el) => el.classList.remove('drop-before', 'drop-after'));
  const rect = sec.getBoundingClientRect();
  const after = e.clientX > rect.left + rect.width / 2;
  sec.classList.add(after ? 'drop-after' : 'drop-before');
});
document.addEventListener('dragleave', (e) => {
  const sec = e.target.closest('.panel.edit');
  if (sec && !sec.contains(e.relatedTarget)) sec.classList.remove('drop-before', 'drop-after');
});
document.addEventListener('drop', (e) => {
  if (!state.edit || !dragSrcId) return;
  const sec = e.target.closest('.panel.edit');
  if (!sec) return;
  e.preventDefault();
  const targetId = sec.getAttribute('data-panel');
  const insertAfter = sec.classList.contains('drop-after');
  document.querySelectorAll('.drop-before, .drop-after').forEach((el) => el.classList.remove('drop-before', 'drop-after'));
  if (!targetId || targetId === dragSrcId) return;

  const dash = registry[state.route];
  const view = dash.render({ data: {}, time: state.time });
  mutate(state.route, view.panels, (ov) => {
    const fullOrder = ov.order && ov.order.length ? ov.order.slice() : view.panels.map((p) => p.id);
    // Make sure both src and target are in the order list (they should be).
    const have = new Set(fullOrder);
    if (!have.has(dragSrcId)) fullOrder.push(dragSrcId);
    if (!have.has(targetId))  fullOrder.push(targetId);
    const from = fullOrder.indexOf(dragSrcId);
    fullOrder.splice(from, 1);
    let to = fullOrder.indexOf(targetId);
    if (insertAfter) to += 1;
    fullOrder.splice(to, 0, dragSrcId);
    ov.order = fullOrder;
  });
  render();
});
document.addEventListener('dragend', () => {
  document.querySelectorAll('.dragging').forEach((el) => el.classList.remove('dragging'));
  document.querySelectorAll('.drop-before, .drop-after').forEach((el) => el.classList.remove('drop-before', 'drop-after'));
  dragSrcId = null;
});

// ---------- route changes ---------------------------------
window.addEventListener('hashchange', () => {
  const parsed = parseRoute();
  state.section = parsed.section;
  state.route   = parsed.route;
  // A URL with query string (e.g. from a share link) should reapply.
  const urlState = parseUrlState();
  if (urlState.time)    state.time    = urlState.time;
  if (urlState.cluster) state.cluster = urlState.cluster;
  if (urlState.filters) {
    state.filters[state.route] = { ...(state.filters[state.route] || {}), ...urlState.filters };
    saveFilters(state.route);
  }
  writeUrlState();
  render();
});

// Boot the refresh interval if the user had one saved.
if (state.refresh > 0) setRefreshInterval(state.refresh);

// Restore mobile preview class if it was active.
if (state.mobile) document.documentElement.classList.add('mobile-preview');

render();
