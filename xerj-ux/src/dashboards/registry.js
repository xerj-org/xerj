// ============================================================
// XERJ.ai — Dashboard + section registry
//
// Two taxonomies at play:
//
//   1. SECTIONS are the primary product surface — the top nav.
//      Order = what users look for first, per user-feedback/kibana/:
//         dashboards → discover → alerts → data → users → settings
//
//   2. DASHBOARDS live under SECTION='dashboards' and appear in
//      the secondary nav row. Other sections each have exactly
//      one view (discover, alerts, data, users, settings).
// ============================================================

import { aiOverview }     from './ai-overview.js';
import { ragQuality }     from './rag-quality.js';
import { vectorIndex }    from './vector-index.js';
import { agentMemory }    from './agent-memory.js';
import { secondBrain }    from './second-brain.js';
import { anomalyDetect }  from './anomaly-detect.js';
import { ingestPipeline } from './ingest-pipeline.js';
import { logsOverview }   from './logs-overview.js';
import { system }         from './system.js';

import { searchDiscover } from './search-discover.js';   // DISCOVER section
import { alerts }         from './alerts.js';            // ALERTS section
import { dataSection }    from './data.js';              // DATA section
import { users }          from './users.js';             // USERS section
import { settings }       from './settings.js';          // SETTINGS section

// Give every DASHBOARD-section view the same section tag. We
// deliberately DON'T add it to the section-level views — those
// already declare their own `section` in their source files.
// Each dashboard also gets a `group` so the sub-nav can bucket
// them into folders/dropdowns — handles screens that can't fit
// the full dashboard list on one line.
const DEFAULT_GROUP = {
  'ai-overview':    'ai',
  'rag-quality':    'ai',
  'vector-index':   'ai',
  'agent-memory':   'ai',
  'second-brain':   'ai',
  'logs-overview':  'logs',
  'anomaly-detect': 'logs',
  'ingest-pipeline':'logs',
  'system':         'infra',
};
for (const d of [aiOverview, ragQuality, vectorIndex, agentMemory, secondBrain, anomalyDetect, ingestPipeline, logsOverview, system]) {
  d.section = 'dashboards';
  d.group = DEFAULT_GROUP[d.id] || 'other';
}
// Second Brain only earns a NAV entry once the engine holds at least
// one brain (someone ran `xerj brain <folder>`). Generic flag: the
// shell (app.js) filters the nav list on `state.liveFeatures[key]`,
// fed by data/brains-probe.js — no dashboard id is hardcoded there.
// Routing, MANAGE, and deep links (`#/dashboards/second-brain?...`)
// always resolve; only nav presence is gated.
secondBrain.requiresLive = 'brains';
// Same gate for the data-plane dashboards: each falls back to full MOCK data
// when its backing index is empty (data/backends/xerj.js → mock.js), so a fresh
// or brain-only engine that showed them would advertise fabricated numbers. Each
// earns a nav entry only once its own data-class index exists (fed by
// data/data-probe.js → state.liveFeatures). Deep links still resolve; only nav
// presence is gated. 'system' stays always-on (cluster stats always exist).
aiOverview.requiresLive     = 'chat-events';
ragQuality.requiresLive     = 'chat-events';
vectorIndex.requiresLive    = 'vector-ops';
agentMemory.requiresLive    = 'agent-memory';
anomalyDetect.requiresLive  = 'anomalies';
ingestPipeline.requiresLive = 'logs-ingest-events';
logsOverview.requiresLive   = 'logs';
// Search-discover gets tagged as the DISCOVER section (promoted out of the dashboards list).
searchDiscover.section = 'discover';

const all = [
  // Dashboards section, ordered by group so the first member of each
  // group is the one the group tab lands on when clicked.
  //   AI:    ai-overview, rag-quality, vector-index, agent-memory, second-brain
  //   Logs:  logs-overview, anomaly-detect, ingest-pipeline
  //   Infra: system
  aiOverview, ragQuality, vectorIndex, agentMemory, secondBrain,
  logsOverview, anomalyDetect, ingestPipeline,
  system,
  // Top-level sections (one view each)
  searchDiscover,
  alerts,
  dataSection,
  users,
  settings,
];

export const defaults = all;
export const registry = Object.fromEntries(all.map((d) => [d.id, d]));

// Primary nav: one entry per section. Order matters — this is the
// order corpus priority told us users scan. The label is uppercase
// UI text; the id maps to a section. Each section knows which view
// to render when clicked (the first dashboard in that section).
export const SECTIONS = [
  { id: 'dashboards', label: 'Dashboards' },
  { id: 'discover',   label: 'Discover'   },
  { id: 'alerts',     label: 'Alerts'     },
  { id: 'data',       label: 'Data'       },
  { id: 'users',      label: 'Users'      },
  { id: 'settings',   label: 'Settings'   },
];

// Dashboard groups — folders under the 'dashboards' section. The
// order here is what users see; each group is rendered in the
// secondary nav with its members visible when the group is active,
// and collapsed to a label otherwise. Adding a new group is just a
// DEFAULT_GROUP entry above + a row in this array.
export const DASHBOARD_GROUPS = [
  { id: 'ai',    label: 'AI'    },
  { id: 'logs',  label: 'Logs'  },
  { id: 'infra', label: 'Infra' },
];

/** Return dashboards belonging to a section. Used by the nav
 *  router and the secondary nav row. */
export function dashboardsInSection(sectionId, merged) {
  return merged.filter((d) => (d.section || 'dashboards') === sectionId);
}

// Legacy alias — kept so callers that imported `dashboardList` still work.
export const dashboardList = all.map((d) => ({ id: d.id, name: d.name }));
