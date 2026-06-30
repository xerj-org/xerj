// ============================================================
// Section — SETTINGS
//
// The last-resort section. If defaults are right, most users
// never visit. This page exposes:
//   • the default cluster
//   • the persistent-state inventory (what's in localStorage)
//   • the dashboards manager (rename/reorder/hide/clone/delete/new)
//   • the DANGER RESET ALL button
//
// We deliberately keep Dashboards management under Settings
// (not as its own primary section) because editing dashboards
// is a rare admin action, not a daily task.
// ============================================================

import { esc }                 from '../ux/text.js';
import { Citations }           from '../ux/charts-ops.js';
import { Markdown }            from '../ux/tables.js';
import { defaultClusterId }    from '../data/data-sources.js';

export const settings = {
  id: 'settings',
  name: 'Settings',
  section: 'settings',
  render: ({ data, time }) => {
    const dashboards = data.dashboards || [];
    const views = data.views || [];
    return {
      title: 'SETTINGS',
      kicker: 'DEFAULTS · DASHBOARDS · DANGER',
      meta: [time, 'ADMIN'],
      caption: 'Everything under SETTINGS is rare admin. Rename or reorder a dashboard here; for everything else, the defaults are right.',
      panels: [

        { id: 'defaults', eyebrow: 'DEFAULTS', cols: 12, type: 'settings',
          render: () => renderSettings(),
        },

        { id: 'mg-dashboards-head', eyebrow: '', cols: 12, type: 'markdown',
          render: () => `<div class="h-section" style="margin-top:var(--sp-3);">DASHBOARDS · ${dashboards.length}</div>
            <div class="hint" style="margin-top:6px;">Drag a row to reorder. Click RENAME to rename. CLONE, HIDE, or DELETE from the action buttons.</div>`,
        },

        { id: 'mg-dashboards', eyebrow: '', cols: 12, type: 'manage-dashboards',
          render: () => renderDashboardTable(dashboards),
        },

        { id: 'mg-new', eyebrow: '', cols: 12, type: 'manage-new',
          render: () => renderNewDashboardRow(dashboards.filter((d) => !d.isUser && d.section === 'dashboards')),
        },

        { id: 'mg-views-head', eyebrow: '', cols: 12, type: 'markdown',
          render: () => `<div class="h-section" style="margin-top:var(--sp-10);">SAVED VIEWS · ${views.length}</div>
            <div class="hint" style="margin-top:6px;">A saved view is a pinned snapshot of a dashboard with its time range, cluster, and filters. Click the name to apply.</div>`,
        },

        { id: 'mg-views', eyebrow: '', cols: 12, type: 'manage-views',
          render: () => renderViewsTable(views),
        },

        { id: 'storage', eyebrow: 'PERSISTENT STATE INVENTORY', cols: 12, type: 'markdown',
          render: () => Markdown(
`## What we store, where

Everything XERJ.ai persists lives under \`localStorage.xerj.*\`. No
cookies, no IndexedDB, no server state (yet). You can dump the entire
inventory with one browser-console command:

\`\`\`
Object.keys(localStorage).filter(k => k.startsWith('xerj.'))
\`\`\`

The current keys are:

- \`xerj.theme\` · \`day\` | \`night\`
- \`xerj.time\` · last-used time range (\`1H\`, \`24H\`, \`7D\`, ...)
- \`xerj.edit\` · \`0\` | \`1\` — is edit mode sticky on reload
- \`xerj.search\` · search-discover state (query + type + filters + sort)
- \`xerj.dashboards\` · rename / order / hidden / custom (user dashboards)
- \`xerj.layout.<dash-id>\` · per-dashboard panel layout overrides
- \`xerj.cluster\` · default cluster id

When the engine grows a \`/v1/users/me\` endpoint, this store syncs up.`
          ),
        },

        { id: 'danger', eyebrow: 'DANGER · WIPE ALL STATE', cols: 12, type: 'danger',
          render: () => `
            <div class="mg-settings">
              <div class="mg-setting">
                <span class="key">RESET EVERYTHING</span>
                <button type="button" class="mg-btn mg-btn-danger" data-mg-reset-all>RESET ALL SAVED STATE</button>
                <span class="hint">— restores defaults, deletes every user dashboard, clears layouts, themes, filters. Reloads the page. Cannot be undone.</span>
              </div>
            </div>
          `,
        },

        { id: 'citations', eyebrow: 'WHY THIS SECTION EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
          render: () => Citations({
            items: [
              { id: 'gh-56406', source: 'github', score: 41,
                title: 'Add a configuration setting for default "Rows Per Page" setting in Management',
                url: 'https://github.com/elastic/kibana/issues/56406' },
              { id: 'gh-6515',  source: 'github', score: 18,
                title: 'Kibana Globalization',
                url: 'https://github.com/elastic/kibana/issues/6515' },
              { id: 'gh-1600',  source: 'github', score: 14,
                title: 'Global timezone support',
                url: 'https://github.com/elastic/kibana/issues/1600' },
            ],
            total: 3610,
          }),
        },

      ],
    };
  },
};

// ---------- renderers -----------------------------------

function renderSettings() {
  return `
    <div class="mg-settings">
      <div class="mg-setting">
        <span class="key">DEFAULT CLUSTER</span>
        <span class="mono accent">${esc(defaultClusterId())}</span>
        <span class="hint">— change by clicking a cluster in the <a href="#/data" class="mono accent">DATA</a> section</span>
      </div>
      <div class="mg-setting">
        <span class="key">THEME</span>
        <span class="mono">toggle in the top-right nav</span>
      </div>
      <div class="mg-setting">
        <span class="key">TIME ZONE</span>
        <span class="mono">${esc(Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC')}</span>
        <span class="hint">— auto-detected from browser</span>
      </div>
    </div>`;
}

function renderDashboardTable(dashboards) {
  if (!dashboards.length) {
    return '<div class="mono faint">No dashboards. Click + NEW to create one.</div>';
  }
  // Only show rows for dashboards in the 'dashboards' section (skip section-level dashboards like alerts/data/users/settings).
  const listable = dashboards.filter((d) => d.section === 'dashboards' || d.section == null);
  const rows = listable.map((d, i) => {
    const isHidden = d.hidden;
    const kind = d.isUser
      ? `<span class="mono accent">USER</span>`
      : `<span class="mono faint">DEFAULT</span>`;
    const src  = d.clonedFrom
      ? `<span class="mono faint">from ${esc(d.clonedFrom)}</span>`
      : '';
    const actions = [
      `<button type="button" class="mg-btn" data-mg-up="${esc(d.id)}" title="Move up">↑</button>`,
      `<button type="button" class="mg-btn" data-mg-down="${esc(d.id)}" title="Move down">↓</button>`,
      `<button type="button" class="mg-btn" data-mg-rename="${esc(d.id)}" title="Rename">RENAME</button>`,
      `<button type="button" class="mg-btn" data-mg-hide="${esc(d.id)}" title="${isHidden ? 'Show' : 'Hide'}">${isHidden ? 'SHOW' : 'HIDE'}</button>`,
      `<button type="button" class="mg-btn" data-mg-clone="${esc(d.id)}" title="Clone">CLONE</button>`,
      d.isUser
        ? `<button type="button" class="mg-btn mg-btn-danger" data-mg-delete="${esc(d.id)}" title="Delete">DELETE</button>`
        : `<span class="mg-btn mg-btn-disabled" title="Defaults can only be hidden">—</span>`,
    ].join('');
    return `
      <div class="mg-row${isHidden ? ' mg-row-hidden' : ''}" data-mg-id="${esc(d.id)}">
        <span class="mg-order mono faint">${String(i + 1).padStart(2, '0')}</span>
        <span class="mg-name">${esc(d.name)}</span>
        <span class="mg-kind">${kind}</span>
        <span class="mg-src">${src}</span>
        <span class="mg-actions">${actions}</span>
      </div>`;
  }).join('');
  return `
    <div class="mg-table">
      <div class="mg-row mg-head">
        <span class="mg-order">#</span>
        <span class="mg-name">NAME</span>
        <span class="mg-kind">KIND</span>
        <span class="mg-src">SOURCE</span>
        <span class="mg-actions">ACTIONS</span>
      </div>
      ${rows}
    </div>`;
}

function renderViewsTable(views) {
  if (!views.length) {
    return '<div class="mono faint">No saved views. Pin a view from any dashboard with the + SAVE CURRENT VIEW button.</div>';
  }
  const rows = views.map((v, i) => {
    const fs = v.filters || {};
    const fbits = Object.entries(fs).map(([k, val]) => {
      const vals = Array.isArray(val) ? val.join('|') : val;
      return `${k}:${vals}`;
    }).join(' · ') || '—';
    const when = (v.savedAt || '').slice(0, 16).replace('T', ' ');
    return `
      <div class="mg-row" data-view-id="${esc(v.id)}">
        <span class="mg-order mono faint">${String(i + 1).padStart(2, '0')}</span>
        <span class="mg-name"><button type="button" class="mg-btn mg-btn-link" data-view-apply="${esc(v.id)}" title="Apply view">${esc(v.name)}</button></span>
        <span class="mg-kind"><span class="mono faint">${esc(v.dashId)}</span></span>
        <span class="mg-src"><span class="mono faint">${esc(v.time || '24H')} · ${esc(v.cluster || '—')} · ${esc(fbits)}</span></span>
        <span class="mg-actions">
          <span class="mono faint">${esc(when)}</span>
          <button type="button" class="mg-btn" data-view-apply="${esc(v.id)}" title="Apply">APPLY</button>
          <button type="button" class="mg-btn mg-btn-danger" data-view-delete="${esc(v.id)}" title="Delete">DELETE</button>
        </span>
      </div>`;
  }).join('');
  return `
    <div class="mg-table">
      <div class="mg-row mg-head">
        <span class="mg-order">#</span>
        <span class="mg-name">NAME</span>
        <span class="mg-kind">DASHBOARD</span>
        <span class="mg-src">CONTEXT</span>
        <span class="mg-actions">SAVED / ACTIONS</span>
      </div>
      ${rows}
    </div>`;
}

function renderNewDashboardRow(templates) {
  const options = templates.map((t) =>
    `<button type="button" class="mg-btn" data-mg-new="${esc(t.id)}" title="New from ${esc(t.name)}">${esc(t.name)}</button>`
  ).join('<span class="sep">·</span>');
  return `
    <div class="mg-new-row">
      <span class="key accent">+ NEW DASHBOARD</span>
      <span class="hint" style="margin: 0 var(--sp-2);">clone a template</span>
      <span class="mg-new-templates">${options}</span>
      <span class="hint" style="margin-left:auto;">or</span>
      <button type="button" class="mg-btn" data-mg-new="" title="New blank dashboard">BLANK</button>
    </div>`;
}
