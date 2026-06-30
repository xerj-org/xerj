#!/usr/bin/env node
// ============================================================
// Themes generator
//
// Pulls the top NEGATIVE items per category and writes a plain
// markdown synthesis under themes/. The output is what a PM
// would actually read: real quotes, ranked, attributed.
//
// Run after stats.mjs.
// ============================================================

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const SOURCES_DIR = path.join(ROOT, 'sources');
const THEMES_DIR = path.join(ROOT, 'themes');
fs.mkdirSync(THEMES_DIR, { recursive: true });

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

// ---------- top pains ------------------------------------
const negatives = items.filter((i) => i.sentiment === 'negative' && i.category && i.category !== '99-noise');
console.log('[themes]', negatives.length, 'negative items');

const byCat = {};
for (const it of negatives) {
  byCat[it.category] = byCat[it.category] || [];
  byCat[it.category].push(it);
}

const md = [];
md.push('# Top pains · synthesized from real Kibana user feedback\n');
md.push(`Generated ${new Date().toISOString().slice(0, 10)} · ` +
        `${negatives.length} negative-sentiment artifacts ranked across ${Object.keys(byCat).length} categories.\n`);
md.push('Quotes are real, taken verbatim from Hacker News, Reddit, Stack Overflow ' +
        'and GitHub issues. The score next to each item is the post upvotes / reactions ' +
        'on the original platform.\n');
md.push('---\n');

const cats = Object.entries(byCat).sort((a, b) => b[1].length - a[1].length);
for (const [cat, list] of cats) {
  list.sort((a, b) => (b.score || 0) - (a.score || 0));
  md.push(`## ${cat}  ·  **${list.length}** negative items\n`);
  md.push('| Score | Source | Quote | Link |');
  md.push('|------:|--------|-------|------|');
  for (const it of list.slice(0, 12)) {
    const text = (it.title || '') + ' — ' + trim(it.body, 240);
    const cleaned = text.replace(/\|/g, '\\|');
    md.push(`| ${it.score || 0} | ${it.source.slice(0, 6)} | ${trim(cleaned, 280)} | [↗](${it.source_url}) |`);
  }
  md.push('');
}
fs.writeFileSync(path.join(THEMES_DIR, 'top-pains.md'), md.join('\n'));
console.log('[themes] wrote top-pains.md');

// ---------- top asks (feature requests) ------------------
const asks = items.filter((i) =>
  i.source === 'github' && /enhancement|feature/i.test((i.labels || []).join(' '))
);
asks.sort((a, b) => (b.score || 0) - (a.score || 0));
const askMd = [];
askMd.push('# Top feature requests · from elastic/kibana GitHub issues\n');
askMd.push(`${asks.length} items labeled enhancement / feature, ranked by reactions.\n`);
askMd.push('| Score | Comments | Title | Link |');
askMd.push('|------:|---------:|-------|------|');
for (const it of asks.slice(0, 50)) {
  const t = trim((it.title || '').replace(/\|/g, '\\|'), 100);
  askMd.push(`| ${it.score || 0} | ${it.comments || 0} | ${t} | [↗](${it.source_url}) |`);
}
fs.writeFileSync(path.join(THEMES_DIR, 'top-asks.md'), askMd.join('\n') + '\n');
console.log('[themes] wrote top-asks.md');

// ---------- cross-source clusters: items that share keywords across all sources ----
const buckets = {
  'lens-rewrite-pain':       /lens.*(rewrite|redesign|broken)|tsvb.*(end of life|deprecat|gone)/i,
  'dashboard-perf-killer':   /dashboard.*slow|slow.*dashboard|loading.*forever|spinner|hangs/i,
  'upgrade-broke-everything':/upgrade.*broke|broken (after|on) upgrade|migration error|saved object.*fail/i,
  'kql-confusion':           /kql.*(confus|hate|sucks|broken|hard)/i,
  'pdf-export-broken':       /pdf.*(broken|fail|cut off|missing|wrong)/i,
  'alerting-noise':          /alert.*(noise|noisy|spam|too many)/i,
  'spaces-perm-pain':        /space.*permission|rbac.*(confus|broken)|feature privilege/i,
  'license-paywall-rage':    /paywall|platinum.*(only|required)|enterprise.*(license|tier)/i,
};
const clusters = {};
for (const [name, rx] of Object.entries(buckets)) {
  clusters[name] = items.filter((i) => rx.test((i.title || '') + ' ' + (i.body || ''))).slice(0, 30);
}
const cmd = [];
cmd.push('# Cross-source pain clusters\n');
cmd.push('Each cluster is a regex pattern that catches the same complaint across HN, ' +
        'Reddit, Stack Overflow and GitHub. The point of this view is to show the ' +
        'patterns that recur in multiple communities — not isolated rants.\n');
cmd.push('---\n');
for (const [name, list] of Object.entries(clusters)) {
  cmd.push(`## ${name}  ·  ${list.length} matches\n`);
  if (!list.length) { cmd.push('_no matches_\n'); continue; }
  for (const it of list.slice(0, 10)) {
    const t = trim((it.title || '— no title —').replace(/\|/g, '\\|'), 110);
    cmd.push(`- **${it.source}** · score ${it.score || 0} · [${t}](${it.source_url})`);
  }
  cmd.push('');
}
fs.writeFileSync(path.join(THEMES_DIR, 'cross-source-clusters.md'), cmd.join('\n'));
console.log('[themes] wrote cross-source-clusters.md');
