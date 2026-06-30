#!/usr/bin/env node
// ============================================================
// Kibana feedback classifier
//
// Reads every JSONL file under sources/, scores each artifact
// against the 15-category keyword rules in TAXONOMY.md, picks
// the best primary category and up to 3 secondaries, attaches
// tags, computes naive sentiment, and writes pointer files
// under categories/<slug>/pointers.jsonl.
//
// Re-running rebuilds pointer files from scratch — there is no
// drift. Scoring is deterministic for a given source file.
//
// Usage: node pipeline/classify.mjs
// ============================================================

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const SOURCES_DIR = path.join(ROOT, 'sources');
const CATEGORIES_DIR = path.join(ROOT, 'categories');

// ---------- category rules ---------------------------------
// Order is canonical (slug). Each rule has a list of weighted
// keyword patterns. A pattern can be:
//   - a string (case-insensitive substring, weight 1)
//   - [string, weight]
//   - { rx: /regex/i, w: 2 }
const CATEGORIES = [
  ['01-dashboard-authoring', 'AUTHORING', [
    ['lens', 3], ['tsvb', 3], ['canvas', 2], ['vega', 2],
    ['visualization editor', 3], ['visualize app', 2],
    ['dashboard builder', 3], ['drag and drop', 2],
    ['chart type', 2], ['custom visualization', 2],
    ['kibana dashboard', 2], ['edit dashboard', 1],
    ['agg based', 1], ['xy chart', 1],
    // soft / generic
    ['dashboard', 1], ['visualization', 1], ['visualize', 1],
    ['panel', 1], ['split chart', 2], ['y-axis', 2],
    ['x-axis', 2], ['second y axis', 2], ['second axis', 2],
    ['data table', 1], ['line chart', 1], ['bar chart', 1],
    ['area chart', 1], ['pie chart', 1], ['gauge chart', 1],
    ['metric visualization', 2], ['timeseries', 1],
    ['saved visualization', 2], ['edit visualization', 2],
  ]],
  ['02-discover-and-kql', 'QUERYING', [
    ['discover', 3], ['kql', 3], ['lucene query', 2],
    ['search bar', 2], ['saved search', 3],
    ['scroll api', 2], ['document table', 2],
    ['from + size', 1], ['deep paging', 2],
    ['no results', 1], ['kibana query language', 3],
    // soft
    ['lucene', 2], ['query', 1], ['wildcard query', 2],
    ['query syntax', 2], ['dsl query', 1],
  ]],
  ['03-visualization-quality', 'RENDERING', [
    ['pie chart', 2], ['stacked bar', 2], ['axis label', 2],
    ['legend', 2], ['tooltip', 2], ['chart bug', 3],
    ['can\'t plot', 2], ['missing chart', 2],
    ['render', 1], ['heatmap', 1], ['gauge chart', 1],
    ['chart broken', 3],
    // soft
    ['second y-axis', 2], ['scale', 1], ['bounds', 1],
    ['label overlap', 2], ['axis range', 2],
  ]],
  ['04-performance-and-load', 'SCALING', [
    ['slow', 2], ['loading forever', 3], ['spinner', 2],
    ['timeout', 2], ['timed out', 2], ['hang', 2], ['freeze', 2],
    ['out of memory', 3], ['oom', 2], ['performance', 1],
    ['too slow', 3], ['takes forever', 3],
    ['kibana slow', 4], ['dashboard slow', 4],
    ['p95', 1], ['latency', 1],
    // soft
    ['cpu usage', 1], ['memory usage', 1], ['heap', 1],
    ['too many requests', 2], ['429', 1], ['504', 1],
    ['gateway timeout', 3],
  ]],
  ['05-index-patterns-data-views', 'UX', [
    ['index pattern', 3], ['data view', 3], ['data views', 3],
    ['field refresh', 3], ['runtime field', 2],
    ['scripted field', 2], ['mapping conflict', 3],
    ['mapping explosion', 2], ['wildcard pattern', 2],
    // soft
    ['field type', 2], ['mapping', 1], ['kibana index', 2],
    ['date field', 1], ['nested field', 2],
  ]],
  ['06-sharing-and-export', 'UX', [
    ['pdf report', 3], ['csv export', 3], ['reporting', 2],
    ['embed', 2], ['iframe', 2], ['short url', 3],
    ['reporting queue', 3], ['export csv', 3],
    ['print dashboard', 2], ['share dashboard', 2],
    // soft
    ['export', 1], ['download chart', 2], ['png export', 2],
    ['export to image', 2],
  ]],
  ['07-alerting-and-watcher', 'OPS', [
    ['watcher', 3], ['alerting', 3], ['rule type', 2],
    ['connector', 1], ['slack notification', 2],
    ['opsgenie', 2], ['pagerduty', 2],
    ['notification', 1], ['alert noise', 3],
    ['noisy alert', 3], ['silent alert', 3],
    ['kibana alert', 4], ['threshold rule', 2],
    // soft
    ['alert', 1], ['email notification', 2], ['webhook', 1],
  ]],
  ['08-spaces-and-rbac', 'OPS', [
    ['kibana space', 4], ['spaces', 1], ['rbac', 3],
    ['role mapping', 3], ['feature privilege', 3],
    ['api key', 2], ['kibana permission', 4],
    ['permission denied', 2], ['403', 1],
    ['multi-tenant', 2], ['multi tenant', 2],
    // soft
    ['authentication', 1], ['sso', 1], ['oidc', 2],
    ['saml', 2], ['ldap', 1],
  ]],
  ['09-upgrades-and-migration', 'TRUST', [
    ['upgrade', 2], ['migration', 1], ['saved object', 3],
    ['broken dashboard', 4], ['version upgrade', 3],
    ['8.0 upgrade', 4], ['7.17', 2], ['breaking change', 3],
    ['can\'t upgrade', 4], ['migration error', 3],
    ['saved objects', 3],
    // soft
    ['version compatibility', 2], ['kbn-version', 2],
    ['after upgrading', 3], ['after upgrade', 2],
  ]],
  ['10-docs-and-ux', 'UX', [
    ['confusing', 2], ['dark mode', 3], ['filter bar', 3],
    ['time picker', 3], ['ui change', 2], ['kibana ui', 2],
    ['documentation', 1], ['kibana docs', 2],
    ['unclear', 1], ['inconsistent', 1],
    ['where is the', 2], ['can\'t find', 2],
    // soft
    ['rison', 2], ['url', 0.5], ['ui', 1], ['look and feel', 2],
    ['change order', 1], ['display name', 2],
  ]],
  ['11-ml-and-anomaly', 'UX', [
    ['anomaly detection', 4], ['machine learning', 2],
    ['ml job', 4], ['data frame', 3], ['transform job', 3],
    ['paywall', 2], ['platinum license', 3],
    ['kibana ml', 4],
    // soft
    ['ml', 0.5], ['outlier', 1], ['regression model', 2],
  ]],
  ['12-maps-and-geo', 'RENDERING', [
    ['kibana map', 4], ['ems tiles', 3], ['ems', 1],
    ['geo_point', 3], ['choropleth', 3], ['heat map', 1],
    ['offline tiles', 3], ['geojson', 2],
    ['region map', 3],
    // soft
    ['map', 1], ['geo', 1], ['layer', 0.5],
  ]],
  ['13-observability-apm', 'AUTHORING', [
    ['apm', 3], ['service map', 3], ['transaction trace', 3],
    ['span', 1], ['trace view', 3], ['observability', 2],
    ['uptime app', 3], ['rum agent', 2],
    ['logs ui', 2], ['metrics explorer', 2],
    // soft
    ['rum', 1], ['traces', 1], ['service inventory', 2],
  ]],
  ['14-siem-and-security', 'OPS', [
    ['siem', 3], ['detection rule', 3], ['kibana case', 3],
    ['timeline', 1], ['security app', 3], ['endpoint', 1],
    ['fleet', 1], ['osquery', 2], ['threat intel', 2],
    // soft
    ['detection', 1], ['cve', 1], ['vulnerability', 1],
  ]],
  ['15-plugin-and-ecosystem', 'TRUST', [
    ['kibana plugin', 4], ['plugin api', 3],
    ['removed feature', 3], ['deprecated', 2],
    ['breaking api', 3], ['plugin broken', 4],
    ['custom plugin', 3], ['third party plugin', 3],
    // soft
    ['plugin', 1], ['extension', 1], ['opensearch', 1],
    ['fork', 1], ['feature request', 1], ['enhancement', 0.5],
  ]],
];

// Negative / positive lexicon for naive sentiment
const NEG = [
  'broken', 'bug', 'horrible', 'hate', 'awful', 'painful', 'pain',
  'slow', 'hang', 'stuck', 'crash', 'crashes', 'fail', 'failed',
  'frustrating', 'useless', 'worst', 'terrible', 'unstable',
  'oom', 'leak', 'leaks', 'broke', 'breaks', 'regression',
  'unusable', 'sucks', 'garbage', 'nightmare', 'nope', 'avoid',
];
const POS = [
  'love', 'loved', 'great', 'works well', 'nice', 'amazing',
  'thanks', 'thank you', 'excellent', 'fast', 'reliable',
  'recommend', 'helpful', 'solid',
];

function scoreText(text, rules) {
  let s = 0;
  const lower = text.toLowerCase();
  for (const rule of rules) {
    if (typeof rule === 'string') {
      if (lower.includes(rule)) s += 1;
    } else if (Array.isArray(rule)) {
      const [kw, w] = rule;
      if (lower.includes(kw)) s += w;
    } else if (rule && rule.rx) {
      if (rule.rx.test(text)) s += rule.w || 1;
    }
  }
  return s;
}

function sentimentOf(text) {
  const lower = text.toLowerCase();
  let neg = 0, pos = 0;
  for (const w of NEG) if (lower.includes(w)) neg++;
  for (const w of POS) if (lower.includes(w)) pos++;
  if (neg === 0 && pos === 0) return 'neutral';
  if (neg > pos * 1.5) return 'negative';
  if (pos > neg * 1.5) return 'positive';
  return 'neutral';
}

// Tag extraction: a small set of always-on flags
const TAGS = [
  ['kql', /\bkql\b/i],
  ['lens', /\blens\b/i],
  ['tsvb', /\btsvb\b/i],
  ['canvas', /\bcanvas\b/i],
  ['discover', /\bdiscover\b/i],
  ['alerting', /alerting|watcher/i],
  ['siem', /\bsiem\b/i],
  ['apm', /\bapm\b/i],
  ['upgrade-break', /broken (after|on) upgrade|upgrade broke/i],
  ['saved-objects', /saved object/i],
  ['data-views', /data view/i],
  ['filter-bar', /filter bar/i],
  ['time-picker', /time picker/i],
  ['dark-mode', /dark mode/i],
  ['pdf-export', /pdf (export|report)/i],
  ['csv-export', /csv export/i],
  ['anomaly', /anomaly/i],
  ['ml', /machine learning|ml job/i],
  ['fleet', /\bfleet\b/i],
  ['security-app', /security app/i],
  ['oom', /\boom\b|out of memory/i],
  ['slow-dashboard', /slow dashboard|dashboard.*slow/i],
  ['license', /license|paywall|platinum/i],
];

function extractTags(text) {
  const out = [];
  for (const [name, rx] of TAGS) if (rx.test(text)) out.push(name);
  return out;
}

// Pre-filter for clearly-internal noise (CI test failures, etc.) — these
// are NOT user feedback and shouldn't pollute the buckets.
const NOISE_PATTERNS = [
  /^failing test:/i,
  /^skipped test:/i,
  /^flaky test:/i,
  /jest integration tests/i,
  /backport to /i,
];
function isNoise(item) {
  const t = (item.title || '') + ' ' + (item.body || '');
  return NOISE_PATTERNS.some((rx) => rx.test(t));
}

function classify(item) {
  if (isNoise(item)) {
    return {
      category: '99-noise',
      secondary_categories: [],
      sentiment: 'neutral',
      tags: ['ci-noise'],
    };
  }
  const text = ((item.title || '') + ' ' + (item.body || '')).slice(0, 5000);
  const scores = CATEGORIES.map(([slug, axis, rules]) => ({
    slug,
    axis,
    score: scoreText(text, rules),
  }));
  scores.sort((a, b) => b.score - a.score);
  let primary;
  if (scores[0].score >= 1) {
    primary = scores[0].slug;
  } else if (/kibana/i.test(text)) {
    // Kibana is mentioned but no rule fired — it's general feedback.
    primary = '10-docs-and-ux';
  } else {
    primary = '99-uncategorized';
  }
  const secondaries = scores.slice(1, 4).filter((s) => s.score > 0).map((s) => s.slug);
  return {
    category: primary,
    secondary_categories: secondaries,
    sentiment: sentimentOf(text),
    tags: extractTags(text),
  };
}

// ---------- run -------------------------------------------
const start = Date.now();
const allItems = [];
const sourceFiles = fs.readdirSync(SOURCES_DIR).filter((d) => fs.statSync(path.join(SOURCES_DIR, d)).isDirectory());
for (const src of sourceFiles) {
  const f = path.join(SOURCES_DIR, src, 'raw.jsonl');
  if (!fs.existsSync(f)) continue;
  const lines = fs.readFileSync(f, 'utf8').split('\n').filter(Boolean);
  for (const line of lines) {
    try { allItems.push(JSON.parse(line)); } catch { /* skip */ }
  }
}
console.log(`[classify] ${allItems.length} items across ${sourceFiles.length} sources`);

// Build category buckets
const buckets = new Map();
const ensureBucket = (slug) => {
  if (!buckets.has(slug)) buckets.set(slug, []);
  return buckets.get(slug);
};
const counters = { sentiment: { positive: 0, neutral: 0, negative: 0 }, primary: {} };
let uncategorized = 0;

for (const item of allItems) {
  const c = classify(item);
  Object.assign(item, c);
  ensureBucket(c.category).push({
    id: item.id,
    source: item.source,
    score: item.score || 0,
    posted_at: item.posted_at,
    tags: item.tags,
    title: (item.title || '').slice(0, 160),
  });
  for (const sec of c.secondary_categories) {
    ensureBucket(sec).push({
      id: item.id, source: item.source, score: item.score || 0,
      posted_at: item.posted_at, tags: item.tags,
      title: (item.title || '').slice(0, 160),
      _secondary: true,
    });
  }
  counters.sentiment[c.sentiment] = (counters.sentiment[c.sentiment] || 0) + 1;
  counters.primary[c.category] = (counters.primary[c.category] || 0) + 1;
  if (c.category === '99-uncategorized') uncategorized++;
}

// Write pointer files
fs.mkdirSync(CATEGORIES_DIR, { recursive: true });
const allCategories = [...new Set([
  ...CATEGORIES.map(([s]) => s),
  '99-uncategorized',
  ...Array.from(buckets.keys()),
])];
for (const slug of allCategories) {
  const dir = path.join(CATEGORIES_DIR, slug);
  fs.mkdirSync(dir, { recursive: true });
  const items = buckets.get(slug) || [];
  const out = items.map((i) => JSON.stringify(i)).join('\n') + (items.length ? '\n' : '');
  fs.writeFileSync(path.join(dir, 'pointers.jsonl'), out);
}

// Write enriched master JSONL (the source files plus classifier output)
const enrichedDir = path.join(ROOT, 'sources');
for (const src of sourceFiles) {
  const items = allItems.filter((i) => i.source === src);
  if (!items.length) continue;
  fs.writeFileSync(
    path.join(enrichedDir, src, 'enriched.jsonl'),
    items.map((i) => JSON.stringify(i)).join('\n') + '\n'
  );
}

const elapsed = ((Date.now() - start) / 1000).toFixed(1);
console.log('[classify] done in', elapsed, 's');
console.log('[classify] sentiment:', counters.sentiment);
console.log('[classify] uncategorized:', uncategorized);
console.log('[classify] top categories:');
const top = Object.entries(counters.primary).sort((a, b) => b[1] - a[1]);
for (const [k, v] of top) console.log('   ', String(v).padStart(5, ' '), k);
