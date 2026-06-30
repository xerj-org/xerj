#!/usr/bin/env node
// ============================================================
// Kibana feedback stats reporter
//
// Reads all enriched JSONL + category pointer files and writes:
//   • INDEX.md            — master stats table (sources × categories,
//                           sentiment splits, top tags)
//   • categories/<slug>/README.md  — one per category, with the top
//     20 highest-score items and 1-line quotes (themes generator)
//
// Run: node pipeline/stats.mjs
// ============================================================

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const SOURCES_DIR = path.join(ROOT, 'sources');
const CATEGORIES_DIR = path.join(ROOT, 'categories');

const SOURCES = fs.readdirSync(SOURCES_DIR).filter((d) => fs.statSync(path.join(SOURCES_DIR, d)).isDirectory());

// Load every enriched item (created by classify.mjs).
const items = [];
const bySource = {};
for (const src of SOURCES) {
  const f = path.join(SOURCES_DIR, src, 'enriched.jsonl');
  if (!fs.existsSync(f)) continue;
  const lines = fs.readFileSync(f, 'utf8').split('\n').filter(Boolean);
  bySource[src] = 0;
  for (const line of lines) {
    try { const o = JSON.parse(line); items.push(o); bySource[src]++; } catch {}
  }
}
console.log('[stats]', items.length, 'enriched items across', Object.keys(bySource).length, 'sources');

// ---------- aggregate ------------------------------------
const byCategory = {};
const byCategorySource = {};
const sentimentByCategory = {};
const tagCounts = {};
let totalNeg = 0, totalPos = 0, totalNeu = 0;

for (const it of items) {
  const c = it.category || '99-uncategorized';
  byCategory[c] = (byCategory[c] || 0) + 1;
  byCategorySource[c] = byCategorySource[c] || {};
  byCategorySource[c][it.source] = (byCategorySource[c][it.source] || 0) + 1;
  sentimentByCategory[c] = sentimentByCategory[c] || { positive: 0, neutral: 0, negative: 0 };
  sentimentByCategory[c][it.sentiment] = (sentimentByCategory[c][it.sentiment] || 0) + 1;
  if (it.sentiment === 'negative') totalNeg++;
  else if (it.sentiment === 'positive') totalPos++;
  else totalNeu++;
  for (const t of it.tags || []) tagCounts[t] = (tagCounts[t] || 0) + 1;
}

const allCategories = Object.keys(byCategory).sort();

// ---------- INDEX.md -------------------------------------
const lines = [];
lines.push('# Kibana feedback · master index\n');
lines.push(`Generated ${new Date().toISOString().slice(0, 10)} · `);
lines.push(`**${items.length}** real artifacts across **${Object.keys(bySource).length}** primary sources.\n`);

lines.push('## Sources\n');
lines.push('| Source | Items |');
lines.push('|--------|------:|');
const srcSorted = Object.entries(bySource).sort((a, b) => b[1] - a[1]);
for (const [s, n] of srcSorted) lines.push(`| \`${s}\` | ${n} |`);
lines.push(`| **TOTAL** | **${items.length}** |\n`);

lines.push('## Sentiment (engine-classified, ±)\n');
lines.push('| Class | Count | Share |');
lines.push('|-------|------:|------:|');
const tot = items.length;
lines.push(`| negative | ${totalNeg} | ${(100 * totalNeg / tot).toFixed(1)}% |`);
lines.push(`| neutral  | ${totalNeu} | ${(100 * totalNeu / tot).toFixed(1)}% |`);
lines.push(`| positive | ${totalPos} | ${(100 * totalPos / tot).toFixed(1)}% |\n`);

lines.push('## Categories (primary)\n');
lines.push('| Category | Items | Negative | Positive | Top sources |');
lines.push('|----------|------:|---------:|---------:|-------------|');
const catSorted = Object.entries(byCategory).sort((a, b) => b[1] - a[1]);
for (const [c, n] of catSorted) {
  const sent = sentimentByCategory[c] || {};
  const srcMix = Object.entries(byCategorySource[c] || {})
    .sort((a, b) => b[1] - a[1]).slice(0, 3)
    .map(([s, k]) => `${s.slice(0, 4)}=${k}`).join(' ');
  lines.push(`| [\`${c}\`](categories/${c}/README.md) | ${n} | ${sent.negative || 0} | ${sent.positive || 0} | ${srcMix} |`);
}
lines.push('');

lines.push('## Top tags\n');
lines.push('| Tag | Count |');
lines.push('|-----|------:|');
for (const [t, n] of Object.entries(tagCounts).sort((a, b) => b[1] - a[1]).slice(0, 25)) {
  lines.push(`| \`${t}\` | ${n} |`);
}
lines.push('');

lines.push('## Pipeline\n');
lines.push('```');
lines.push('node pipeline/collect.mjs   # pull from APIs, write sources/<src>/raw.jsonl');
lines.push('node pipeline/classify.mjs  # rewrite enriched.jsonl + category pointers');
lines.push('node pipeline/stats.mjs     # rewrite this file + per-category READMEs');
lines.push('```');
lines.push('');
lines.push('See `SCHEMA.md` and `TAXONOMY.md` for definitions.');

fs.writeFileSync(path.join(ROOT, 'INDEX.md'), lines.join('\n'));
console.log('[stats] wrote INDEX.md');

// ---------- per-category README.md -----------------------
const trim = (s, n) => {
  s = String(s || '').replace(/\s+/g, ' ').trim();
  return s.length > n ? s.slice(0, n - 1) + '…' : s;
};

for (const cat of allCategories) {
  const dir = path.join(CATEGORIES_DIR, cat);
  fs.mkdirSync(dir, { recursive: true });
  const cItems = items
    .filter((i) => i.category === cat)
    .sort((a, b) => (b.score || 0) - (a.score || 0));
  const top = cItems.slice(0, 20);
  const sentTotals = sentimentByCategory[cat] || { positive: 0, neutral: 0, negative: 0 };
  const md = [];
  md.push(`# ${cat}\n`);
  md.push(`**${cItems.length}** items · ` +
          `negative ${sentTotals.negative} · neutral ${sentTotals.neutral} · positive ${sentTotals.positive}\n`);
  md.push(`Pointers: [\`pointers.jsonl\`](pointers.jsonl) · raw bodies in \`../../sources/<src>/enriched.jsonl\`\n`);
  md.push('## Top 20 by score\n');
  if (!top.length) {
    md.push('_no items_');
  } else {
    md.push('| # | Source | Score | Title | URL |');
    md.push('|---|--------|------:|-------|-----|');
    top.forEach((it, i) => {
      const src = (it.source || '').slice(0, 6);
      md.push(`| ${i + 1} | ${src} | ${it.score || 0} | ${trim(it.title, 80)} | [↗](${it.source_url}) |`);
    });
  }

  // Top tags inside this category
  const tagsHere = {};
  for (const it of cItems) for (const t of it.tags || []) tagsHere[t] = (tagsHere[t] || 0) + 1;
  const topTags = Object.entries(tagsHere).sort((a, b) => b[1] - a[1]).slice(0, 12);
  if (topTags.length) {
    md.push('\n## Top tags in this category\n');
    md.push('| Tag | Count |');
    md.push('|-----|------:|');
    for (const [t, n] of topTags) md.push(`| \`${t}\` | ${n} |`);
  }

  fs.writeFileSync(path.join(dir, 'README.md'), md.join('\n') + '\n');
}
console.log('[stats] wrote', allCategories.length, 'category READMEs');
