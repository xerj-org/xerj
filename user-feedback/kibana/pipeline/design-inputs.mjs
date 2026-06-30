#!/usr/bin/env node
// ============================================================
// design-inputs.mjs
//
// Picks the top N actionable items across the corpus and writes
// `themes/design-inputs.md` — a single ranked list a PM can read
// in one sitting and use as the brief for the next dashboard
// sprint.
//
// Ranking signal:
//   • GitHub issues with `enhancement|feature` label  → reactions
//   • All other negative-sentiment items              → score · log(text length)
//
// The output groups by category and hand-maps each item to the
// XERJ.ai response (which dashboard / primitive solves it).
// ============================================================

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const SOURCES_DIR = path.join(ROOT, 'sources');
const THEMES_DIR  = path.join(ROOT, 'themes');

// Hand-mapped responses per category. Edit this when product strategy changes.
const RESPONSE = {
  '01-dashboard-authoring': 'web/src/dashboards/* + edit-mode card grid · panels are JS objects, no rewrite-induced regressions',
  '02-discover-and-kql':    'web/src/dashboards/search-discover.js · live query box, 8 query types, plan tree',
  '03-visualization-quality':'web/src/ux/charts*.js · 23 primitives, all 1px, no chart furniture',
  '04-performance-and-load':'engine/xerj-engine + xerj-storage · single binary, no GC, mmap, doc-values (G2)',
  '05-index-patterns-data-views':'engine/xerj-api · /v1/indices schema is code, not a clickable wizard',
  '06-sharing-and-export':  'TODO · CSV/PNG export from any panel · static URL is enough',
  '07-alerting-and-watcher':'TODO · rules as code, NL→rule, no separate Watcher app',
  '08-spaces-and-rbac':     'TODO · single XERJ.ai token, indices ARE the boundary',
  '09-upgrades-and-migration':'web/src/dashboards/* schema · upgrades can\'t break dashboards because dashboards are code',
  '10-docs-and-ux':         'web/UX_BOOK.md · text is the UI, fewer modals',
  '11-ml-and-anomaly':      'web/src/dashboards/anomaly-detect.js · z-score over engine metrics, no Platinum gate',
  '12-maps-and-geo':        'TODO · ASCII region grid, no offline-tile pain',
  '13-observability-apm':   'TODO · logs+metrics+traces in one index, one query plane',
  '14-siem-and-security':   'TODO · detection rules as code, dedup via AgentMemory',
  '15-plugin-and-ecosystem':'web/src/dashboards/registry.js · drop-in JS module, no plugin API to break',
  '99-noise':               '—',
};

const items = [];
for (const src of fs.readdirSync(SOURCES_DIR)) {
  const f = path.join(SOURCES_DIR, src, 'enriched.jsonl');
  if (!fs.existsSync(f)) continue;
  for (const line of fs.readFileSync(f, 'utf8').split('\n').filter(Boolean)) {
    try { items.push(JSON.parse(line)); } catch {}
  }
}

const trim = (s, n) => {
  s = String(s || '').replace(/\s+/g, ' ').trim();
  return s.length > n ? s.slice(0, n - 1) + '…' : s;
};
const safeMd = (s) => String(s || '').replace(/\|/g, '\\|');

// Quality filter: weed out posts that mention 'kibana' incidentally but
// aren't actually about it. We require the title or first 240 chars of
// body to contain 'kibana' (or 'kbn'). GitHub items are exempt because
// every issue in elastic/kibana IS Kibana feedback by construction.
function isOnTopic(i) {
  if (i.source === 'github' && (i.repo || '').includes('kibana')) return true;
  const head = ((i.title || '') + ' ' + (i.body || '').slice(0, 240)).toLowerCase();
  return /\bkibana\b/.test(head) || /\bkbn[-_]/.test(head);
}

// ---------- pool 1: GitHub feature requests --------------
const ghFeatures = items.filter((i) =>
  i.source === 'github' &&
  (i.labels || []).some((l) => /enhancement|feature|user experience/i.test(l)) &&
  (i.score || 0) > 5 &&
  isOnTopic(i)
);
ghFeatures.sort((a, b) => (b.score || 0) - (a.score || 0));

// ---------- pool 2: highest-engagement negatives ---------
// Quality-weighted: GitHub items get a 5x score multiplier (because GH
// reactions are structured feedback, not random upvotes); on-topic
// social posts only count if they have substantial body text.
const negs = items.filter((i) =>
  i.sentiment === 'negative' &&
  i.category && i.category !== '99-noise' && i.category !== '99-uncategorized' &&
  (i.score || 0) >= 2 &&
  isOnTopic(i) &&
  ((i.body || '').length >= 80 || i.source === 'github')
);
negs.sort((a, b) => {
  const aBoost = a.source === 'github' ? 5 : 1;
  const bBoost = b.source === 'github' ? 5 : 1;
  const aS = (a.score || 0) * aBoost * (1 + Math.log10(((a.body || '').length + 50) / 50));
  const bS = (b.score || 0) * bBoost * (1 + Math.log10(((b.body || '').length + 50) / 50));
  return bS - aS;
});

// ---------- merge into a single top-50 -------------------
// Take top 25 from features + top 25 from negatives, dedupe by id, then truncate to 50.
const top = [];
const seen = new Set();
for (const i of [...ghFeatures.slice(0, 28), ...negs.slice(0, 40)]) {
  if (seen.has(i.id)) continue;
  seen.add(i.id);
  top.push(i);
  if (top.length >= 50) break;
}

// ---------- group by category for the doc ----------------
const byCat = {};
for (const it of top) {
  byCat[it.category] = byCat[it.category] || [];
  byCat[it.category].push(it);
}

// ---------- write design-inputs.md -----------------------
const md = [];
md.push('# Design inputs · top 50 actionable Kibana pains\n');
md.push(`Generated ${new Date().toISOString().slice(0, 10)} from \`themes/top-asks.md\` + ` +
        `\`themes/top-pains.md\`. Each entry maps a real, scored, attributable user pain ` +
        `to the XERJ.ai response.\n`);
md.push('Use this as the brief for the next dashboard sprint. If you ship a feature ' +
        'that is **not** in this list, you are speculating.\n');
md.push('---\n');

const cats = Object.keys(byCat).sort();
let n = 1;
for (const cat of cats) {
  const list = byCat[cat];
  list.sort((a, b) => (b.score || 0) - (a.score || 0));
  md.push(`## ${cat}\n`);
  md.push(`XERJ.ai response: \`${RESPONSE[cat] || '—'}\`\n`);
  md.push('| # | Source | Score | Title | Link |');
  md.push('|---|--------|------:|-------|------|');
  for (const it of list) {
    const title = trim(safeMd(it.title || it.body || '— no title —'), 100);
    md.push(`| ${n++} | ${(it.source || '').slice(0, 6)} | ${it.score || 0} | ${title} | [↗](${it.source_url}) |`);
  }
  md.push('');
}

fs.mkdirSync(THEMES_DIR, { recursive: true });
fs.writeFileSync(path.join(THEMES_DIR, 'design-inputs.md'), md.join('\n'));
console.log('[design-inputs] wrote design-inputs.md · ' + (n - 1) + ' items across ' + cats.length + ' categories');

// ---------- write a tiny machine-readable manifest -------
// Each existing XERJ.ai dashboard can import this and pin captions
// to specific artifact ids. Format: { dashboard_id: [{id, label, score, url}, ...] }
// Each dashboard maps to the categories from the Kibana corpus that its
// panels actually address. Vector-index, agent-memory, rag-quality have
// no Kibana counterpart (Kibana doesn't ship them) — they get the AI/ML
// gap evidence instead, sourced from the broader pool.
const DASH_TO_CATS = {
  'ai-overview':     ['04-performance-and-load', '10-docs-and-ux'],
  'rag-quality':     ['11-ml-and-anomaly'],
  'vector-index':    ['11-ml-and-anomaly', '15-plugin-and-ecosystem'],
  'agent-memory':    ['11-ml-and-anomaly'],
  'search-discover': ['02-discover-and-kql', '01-dashboard-authoring'],
  'anomaly-detect':  ['11-ml-and-anomaly'],
  'ingest-pipeline': ['09-upgrades-and-migration', '04-performance-and-load'],
  'logs-overview':   ['06-sharing-and-export', '01-dashboard-authoring'],
  'system':          ['04-performance-and-load'],
};

// Build the dashboard citation map by drawing from the FILTERED top pool.
// Always source from `negs` (quality-filtered) plus `ghFeatures`. If a
// dashboard's categories yield nothing, fall back to ALL on-topic items in
// those categories ranked by score so we never ship an empty cite block.
const qualityPool = [...ghFeatures, ...negs.filter((n) => !ghFeatures.find((g) => g.id === n.id))];
const fallbackPool = items
  .filter((i) => isOnTopic(i) && i.category && i.category !== '99-noise' && i.category !== '99-uncategorized')
  .sort((a, b) => {
    const aBoost = a.source === 'github' ? 5 : 1;
    const bBoost = b.source === 'github' ? 5 : 1;
    return (b.score || 0) * bBoost - (a.score || 0) * aBoost;
  });

const dashMap = {};
for (const [dashId, cats] of Object.entries(DASH_TO_CATS)) {
  let pick = qualityPool.filter((i) => cats.includes(i.category)).slice(0, 3);
  if (pick.length < 3) {
    const seen = new Set(pick.map((i) => i.id));
    for (const i of fallbackPool) {
      if (pick.length >= 3) break;
      if (cats.includes(i.category) && !seen.has(i.id)) {
        pick.push(i);
        seen.add(i.id);
      }
    }
  }
  dashMap[dashId] = pick.map((i) => ({
    id: i.id, source: i.source, score: i.score || 0,
    title: trim(i.title || '', 90), url: i.source_url,
  }));
}
fs.writeFileSync(
  path.join(THEMES_DIR, 'dashboard-citations.json'),
  JSON.stringify(dashMap, null, 2) + '\n'
);
console.log('[design-inputs] wrote dashboard-citations.json');

// Also emit a JS module the web/ frontend can import directly.
// This is the only place the corpus reaches into the running app.
const WEB_OUT = path.resolve(ROOT, '../../web/src/data/feedback-citations.js');
const moduleSrc = `// AUTO-GENERATED by user-feedback/kibana/pipeline/design-inputs.mjs
// Do not edit by hand. Re-run \`node pipeline/design-inputs.mjs\` after a corpus refresh.
//
// Each dashboard in src/dashboards/ that wants to attribute its panels to
// real user pain imports this module and renders a CITATIONS panel.

export const dashboardCitations = ${JSON.stringify(dashMap, null, 2)};
`;
try {
  fs.mkdirSync(path.dirname(WEB_OUT), { recursive: true });
  fs.writeFileSync(WEB_OUT, moduleSrc);
  console.log('[design-inputs] wrote', path.relative(process.cwd(), WEB_OUT));
} catch (e) {
  console.log('[design-inputs] could not write web module:', e.message);
}
