// ============================================================
// XERJ.ai — Chrome: nav, scene header, time control, footer
// Pure text. Buttons are `<button>` for a11y but styled as text.
// ============================================================

import { esc } from './text.js';

/**
 * ThemeCtrl — text-only day/night toggle. Emits `[data-theme]` clicks
 * handled by the app router.
 */
export const ThemeCtrl = ({ active = 'night' } = {}) => `
<span class="theme" role="group" aria-label="Theme">
  <button type="button" data-theme-set="day"   class="${active === 'day'   ? 'active' : ''}" aria-pressed="${active === 'day'}">DAY</button>
  <span class="dash">·</span>
  <button type="button" data-theme-set="night" class="${active === 'night' ? 'active' : ''}" aria-pressed="${active === 'night'}">NIGHT</button>
</span>`;

export const MobileCtrl = ({ active = false } = {}) => `
<span class="mobile-ctrl" role="group" aria-label="Mobile preview">
  <button type="button" data-mobile-toggle class="${active ? 'active' : ''}" aria-pressed="${active}">MOBILE</button>
</span>`;

/**
 * EditCtrl — text-only edit-mode toggle. When active also shows RESET.
 */
export const EditCtrl = ({ active = false } = {}) => `
<span class="edit-ctrl" role="group" aria-label="Edit mode">
  <button type="button" data-edit-toggle class="${active ? 'active' : ''}" aria-pressed="${active}">EDIT</button>
  ${active ? `<span class="dash">·</span><button type="button" data-reset-layout>RESET</button>` : ''}
</span>`;

/**
 * Nav — the top bar. Two rows:
 *   Row 1 (always): brand + primary sections + edit/theme/status
 *   Row 2 (only when section='dashboards'): grouped dashboards list
 *
 * The secondary row is "folder tabs": each `group` is shown as a
 * label, and the ACTIVE group's members render inline next to it.
 * Other groups collapse to just their label — click them to switch
 * groups (lands on the group's first dashboard). This handles
 * screens too narrow to fit every dashboard on one line.
 *
 *   sections:       [{ id, label }]  — primary product sections
 *   activeSection:  id
 *   dashboards:     [{ id, name, group }] — dashboards under the active section
 *   groups:         [{ id, label }] — ordered group list for the sub-nav
 *   activeDash:     id               — currently-selected dashboard
 */
export const Nav = ({
  sections = [],
  activeSection = 'dashboards',
  dashboards = [],
  groups = [],
  activeDash = '',
  theme = 'night',
  edit = false,
  mobile = false,
  status = '',
} = {}) => {
  const primaryLinks = sections.map((s) => {
    const href = s.id === 'dashboards' ? '#/dashboards' : '#/' + s.id;
    return `<a href="${href}" data-section="${esc(s.id)}" class="${s.id === activeSection ? 'active' : ''}">${esc(s.label)}</a>`;
  }).join('');

  const showSecondary = activeSection === 'dashboards' && dashboards.length > 0;
  let secondary = '';
  if (showSecondary) {
    // If we have groups, bucket dashboards by group; otherwise fall
    // back to a flat list (backward compatibility with any future
    // dashboard that omits a group).
    const byGroup = {};
    for (const d of dashboards) {
      const g = d.group || 'other';
      (byGroup[g] = byGroup[g] || []).push(d);
    }
    // Determine the active group from the active dashboard.
    const activeDashObj = dashboards.find((d) => d.id === activeDash);
    const activeGroup = activeDashObj?.group || (groups[0]?.id || 'other');

    // Stable order: the `groups` registry. Any group that exists in
    // data but not in the registry is appended at the end.
    const orderedGroups = [];
    const seen = new Set();
    for (const g of groups) {
      if (byGroup[g.id]) { orderedGroups.push({ ...g, members: byGroup[g.id] }); seen.add(g.id); }
    }
    for (const gid of Object.keys(byGroup)) {
      if (!seen.has(gid)) orderedGroups.push({ id: gid, label: gid.toUpperCase(), members: byGroup[gid] });
    }

    const groupHtml = orderedGroups.map((g) => {
      const isActive = g.id === activeGroup;
      const members = isActive
        ? g.members.map((d) =>
            `<a href="#/dashboards/${esc(d.id)}" data-dash="${esc(d.id)}" class="${d.id === activeDash ? 'active' : ''}">${esc(d.name)}</a>`
          ).join('')
        : '';
      const firstId = g.members[0]?.id || '';
      const labelCls = 'group-label' + (isActive ? ' active' : '');
      return `
        <span class="group${isActive ? ' open' : ''}">
          <button type="button" class="${labelCls}" data-dash-group="${esc(g.id)}" data-dash-group-first="${esc(firstId)}" aria-expanded="${isActive}">${esc(g.label)}</button>
          ${isActive ? `<span class="members">${members}</span>` : ''}
        </span>`;
    }).join('');

    secondary = `<nav class="nav-sub" aria-label="Dashboards">${groupHtml}</nav>`;
  }

  return `
<nav class="nav" aria-label="Product">
  <span class="brand">XERJ CONSOLE</span>
  ${primaryLinks}
  <span class="spacer"></span>
  ${EditCtrl({ active: edit })}
  ${MobileCtrl({ active: mobile })}
  ${ThemeCtrl({ active: theme })}
  ${status ? `<span class="status">${esc(status)}</span>` : ''}
</nav>
${secondary}`;
};

/**
 * SceneHeader — the 96px scene title with kicker. One per view.
 *   title:    string
 *   kicker:   string (displayed above the title in small caps)
 *   meta:     optional array of strings rendered next to the kicker
 *   editable: if true, the title becomes click-to-rename via contenteditable
 *   dashId:   the dashboard id (needed when editable=true so the rename
 *             handler knows which dashboard to persist)
 */
export const SceneHeader = ({ title, kicker = '', meta = [], editable = false, dashId = '' } = {}) => {
  const bits = [kicker, ...meta].filter(Boolean);
  const kickerLine = bits.length
    ? `<div class="kicker"><span class="key">${bits.map(esc).join(
        '</span><span class="dash">·</span><span class="key">'
      )}</span></div>`
    : '';
  const editAttrs = editable
    ? ` contenteditable="true" spellcheck="false" data-rename-dash="${esc(dashId)}" title="Click to rename · Enter to save · Esc to cancel"`
    : '';
  return `
<header class="scene">
  ${kickerLine}
  <h1 class="h-scene${editable ? ' editable' : ''}"${editAttrs}>${esc(title)}</h1>
</header>`;
};

/**
 * TimeCtrl — inline range selector. Emits `[data-time]` click events
 * handled by the app router. Adds a CUSTOM slot that reveals two
 * datetime-local inputs for an absolute from/to range — a top-15
 * corpus ask that Kibana already supports and users expect.
 */
export const TimeCtrl = ({
  ranges = ['1H','24H','7D','30D','90D'],
  active = '24H',
  custom = { from: '', to: '' },
} = {}) => {
  const buttons = ranges.map((r) =>
    `<button type="button" data-time="${esc(r)}" class="${r === active ? 'active' : ''}" aria-pressed="${r === active}">${esc(r)}</button>`
  ).join('');
  const isCustom = active === 'CUSTOM';
  const customBtn = `<button type="button" data-time="CUSTOM" class="${isCustom ? 'active' : ''}" aria-pressed="${isCustom}">CUSTOM</button>`;
  const customInputs = isCustom ? `
    <span class="custom-range">
      <input type="datetime-local" data-time-from value="${esc(custom.from || '')}" aria-label="From" />
      <span class="dash">→</span>
      <input type="datetime-local" data-time-to   value="${esc(custom.to   || '')}" aria-label="To"   />
    </span>` : '';
  return `
  <div class="time" role="group" aria-label="Time range">
    <span class="key">RANGE</span>
    ${buttons}
    ${customBtn}
    ${customInputs}
  </div>`;
};

/**
 * RefreshCtrl — auto-refresh interval picker (milliseconds). 0 = off.
 * Corpus evidence: auto-refresh is in the Kibana top-5 user asks —
 * users want live dashboards without a full page reload.
 */
export const RefreshCtrl = ({
  intervals = [
    { ms: 0,       label: 'OFF' },
    { ms: 10_000,  label: '10S' },
    { ms: 30_000,  label: '30S' },
    { ms: 60_000,  label: '1M'  },
    { ms: 300_000, label: '5M'  },
  ],
  active = 0,
} = {}) => `
<div class="refresh" role="group" aria-label="Auto-refresh">
  <span class="key">REFRESH</span>
  ${intervals.map((iv) =>
    `<button type="button" data-refresh="${iv.ms}" class="${iv.ms === active ? 'active' : ''}" aria-pressed="${iv.ms === active}">${esc(iv.label)}</button>`
  ).join('')}
</div>`;

/**
 * FilterBar — global filter pills row + KQL-lite search input.
 * Lives between the time control and the canvas on every dashboard.
 * Fixes the #1 Kibana authoring pain: filters are a dashboard-level
 * concept, not a per-panel one. Plus the GH#3693 ask for OR filters:
 * a field can hold an array of values rendered as `field : a | b`.
 *
 *   filters:  { [field]: value | value[] }  — current dashboard filters
 *   kql:      the current text in the KQL input (preserved across renders)
 *
 * Pills emit:
 *   [data-filter-remove="field:value"]   — remove one value from field
 *   [data-filter-clear]                  — clear everything
 *   [data-kql-input]                     — the search input (Enter to run)
 */
export const FilterBar = ({ filters = {}, kql = '' } = {}) => {
  const entries = Object.entries(filters);
  const pills = entries.map(([field, value]) => {
    const values = Array.isArray(value) ? value : [value];
    const chips = values.map((v, i) => `
      ${i > 0 ? '<span class="or">OR</span>' : ''}
      <button type="button" class="chip" data-filter-remove="${esc(field)}:${esc(v)}" title="Remove ${esc(field)} = ${esc(v)}">
        <span class="value">${esc(v)}</span>
        <span class="x">✕</span>
      </button>
    `).join('');
    return `
    <span class="pill active">
      <span class="field">${esc(field)}</span>
      <span class="eq">:</span>
      ${chips}
    </span>`;
  }).join('');

  const hint = entries.length
    ? ''
    : `<span class="hint mono faint">TYPE <span class="mono">field:value</span> ABOVE · OR CLICK A LABEL IN A CHART</span>`;
  const clearBtn = entries.length
    ? `<button type="button" class="clear" data-filter-clear>CLEAR ALL</button>`
    : '';

  return `
  <div class="filter-bar${entries.length ? '' : ' empty'}" aria-label="Filters">
    <div class="kql">
      <span class="key">KQL</span>
      <input type="text" data-kql-input value="${esc(kql)}" placeholder="service:auth level:error &quot;login failed&quot;" spellcheck="false" autocomplete="off" />
    </div>
    <div class="pills">
      <span class="key">FILTER</span>
      ${pills}
      ${hint}
      ${clearBtn}
    </div>
  </div>`;
};

/**
 * SavedViews — a named snapshot of (dashId, time, cluster, filters).
 * Rendered as a one-line row below the filter bar on dashboards.
 * Kibana equivalent: saved searches / dashboard links. Top-5 corpus
 * ask — users want to pin a context they can jump back to.
 */
export const SavedViews = ({ views = [], dashId = '' } = {}) => {
  const mine = views.filter((v) => v.dashId === dashId);
  const saveBtn = `<button type="button" class="save" data-view-save>+ SAVE CURRENT VIEW</button>`;
  if (!mine.length) {
    return `
    <div class="saved-views empty" aria-label="Saved views">
      <span class="key">VIEWS</span>
      <span class="hint mono faint">NO SAVED VIEWS</span>
      ${saveBtn}
    </div>`;
  }
  const links = mine.map((v) => `
    <button type="button" class="view" data-view-apply="${esc(v.id)}" title="Apply view">${esc(v.name)}</button>
    <button type="button" class="x" data-view-delete="${esc(v.id)}" title="Delete view" aria-label="Delete view">✕</button>
  `).join('<span class="sep">·</span>');
  return `
  <div class="saved-views" aria-label="Saved views">
    <span class="key">VIEWS</span>
    ${links}
    ${saveBtn}
  </div>`;
};

/**
 * ClusterCtrl — dropdown-as-text cluster selector. Shown on dashboards
 * that hit cluster-scoped data. Emits `[data-cluster-set]`.
 */
export const ClusterCtrl = ({ clusters = [], active = '' } = {}) => {
  if (!clusters.length) return '';
  return `
  <div class="cluster-ctrl" role="group" aria-label="Cluster">
    <span class="key">CLUSTER</span>
    ${clusters.map((c) =>
      `<button type="button" data-cluster-set="${esc(c.id)}" class="${c.id === active ? 'active' : ''}" aria-pressed="${c.id === active}">${esc(c.name || c.id)}</button>`
    ).join('')}
  </div>`;
};

/** Footer — one line, mute, uppercase. No border. */
export const Footer = ({ version = 'v0.1' } = {}) => {
  const now = new Date().toISOString().slice(0, 16).replace('T', ' · ');
  return `
<footer class="footer">
  <span>XERJ CONSOLE · ${esc(version)}</span>
  <span>TYPE IS THE UI</span>
  <span>${esc(now)}</span>
</footer>`;
};
