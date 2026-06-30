#!/usr/bin/env node
// ============================================================
// Kibana feedback collector
//
// Pulls real artifacts about Kibana from 4 public, no-auth APIs:
//   1. HN Algolia           (most data, permissive)
//   2. Reddit .json         (generous, light rate limit)
//   3. Stack Exchange API   (tag-based)
//   4. GitHub issues        (elastic/kibana, 60 req/hr unauth)
//
// Writes JSONL into user-feedback/kibana/sources/<src>/raw.jsonl,
// one line per artifact. Idempotent — re-running replaces items
// with the same id. Resumable via start-page arguments.
//
// Usage:
//    node pipeline/collect.mjs [--source hn|reddit|so|github|all]
//                              [--pages N]
// ============================================================

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const SOURCES = {
  hackernews:    path.join(ROOT, 'sources/hackernews/raw.jsonl'),
  reddit:        path.join(ROOT, 'sources/reddit/raw.jsonl'),
  stackoverflow: path.join(ROOT, 'sources/stackoverflow/raw.jsonl'),
  github:        path.join(ROOT, 'sources/github/raw.jsonl'),
  discourse:     path.join(ROOT, 'sources/discourse/raw.jsonl'),
};
const GH_TOKEN = process.env.GH_TOKEN || process.env.GITHUB_TOKEN || '';

const args = Object.fromEntries(process.argv.slice(2).reduce((a, x, i, arr) => {
  if (x.startsWith('--')) a.push([x.slice(2), arr[i + 1]]);
  return a;
}, []));
const pages = Number(args.pages || 20);
const wanted = (args.source || 'all').split(',').map((s) => s.trim());

// ---------- helpers ---------------------------------------
const nowIso = () => new Date().toISOString();

function stripHtml(s) {
  if (!s) return '';
  return String(s)
    .replace(/<br\s*\/?>/gi, '\n')
    .replace(/<[^>]+>/g, '')
    .replace(/&quot;/g, '"').replace(/&#x27;/g, "'")
    .replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>')
    .replace(/&nbsp;/g, ' ')
    .trim();
}

function sniffVersions(text) {
  if (!text) return [];
  const m = text.match(/\b([5-9]|1\d)\.\d{1,2}(?:\.\d{1,2})?\b/g) || [];
  return Array.from(new Set(m)).slice(0, 4);
}

// Append a batch of already-normalized items to a JSONL file.
// Dedupe by id against what already exists on disk.
function writeBatch(filePath, items) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  const seen = new Set();
  if (fs.existsSync(filePath)) {
    const existing = fs.readFileSync(filePath, 'utf8').split('\n').filter(Boolean);
    const kept = [];
    for (const line of existing) {
      try {
        const obj = JSON.parse(line);
        if (!seen.has(obj.id)) { seen.add(obj.id); kept.push(line); }
      } catch { /* skip corrupt */ }
    }
    fs.writeFileSync(filePath, kept.join('\n') + (kept.length ? '\n' : ''));
  }
  // Open for append-only, write new items that aren't already in the file.
  const fd = fs.openSync(filePath, 'a');
  let added = 0, replaced = 0;
  for (const item of items) {
    if (!item || !item.id) continue;
    if (seen.has(item.id)) { replaced++; continue; }
    fs.writeSync(fd, JSON.stringify(item) + '\n');
    seen.add(item.id);
    added++;
  }
  fs.closeSync(fd);
  return { added, replaced };
}

async function getJson(url, { headers = {}, timeout = 20000 } = {}) {
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), timeout);
  try {
    const res = await fetch(url, {
      headers: { 'user-agent': 'xerj-feedback-collector/1.0', ...headers },
      signal: ctrl.signal,
    });
    if (!res.ok) throw new Error(`${res.status} ${res.statusText} · ${url}`);
    return await res.json();
  } finally {
    clearTimeout(t);
  }
}

async function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

// ============================================================
// HN Algolia — by far the most generous
// ============================================================
async function collectHackerNews() {
  console.log('[HN] collecting…');
  const queries = [
    { q: 'kibana', tags: 'story' },
    { q: 'kibana', tags: 'comment' },
    { q: 'kibana dashboard', tags: 'comment' },
    { q: 'kibana lens', tags: 'comment' },
    { q: 'kibana alerting', tags: 'comment' },
    { q: 'kibana upgrade', tags: 'comment' },
    { q: 'kibana slow', tags: 'comment' },
    { q: 'kibana discover', tags: 'comment' },
    { q: 'kibana kql', tags: 'comment' },
    { q: 'kibana plugin', tags: 'comment' },
    { q: 'kibana sucks', tags: 'comment' },
    { q: 'kibana hate', tags: 'comment' },
    { q: 'kibana broken', tags: 'comment' },
    { q: 'kibana migration', tags: 'comment' },
    { q: 'kibana spaces', tags: 'comment' },
    { q: 'kibana siem', tags: 'comment' },
    { q: 'kibana apm', tags: 'comment' },
    { q: 'kibana opensearch', tags: 'comment' },
    { q: 'elasticsearch kibana', tags: 'comment' },
    { q: 'grafana vs kibana', tags: 'comment' },
    { q: 'elk stack', tags: 'comment' },
    { q: 'elastic stack', tags: 'comment' },
    { q: 'opensearch dashboards', tags: 'comment' },
    { q: 'kibana alternatives', tags: '(story,comment)' },
    { q: 'kibana enterprise', tags: '(story,comment)' },
  ];
  const all = [];
  for (const { q, tags } of queries) {
    for (let page = 0; page < pages; page++) {
      const url = `https://hn.algolia.com/api/v1/search_by_date?query=${encodeURIComponent(q)}&tags=${encodeURIComponent(tags)}&hitsPerPage=1000&page=${page}`;
      try {
        const j = await getJson(url);
        const hits = j.hits || [];
        if (!hits.length) break;
        for (const h of hits) {
          const title = h.title || h.story_title || '';
          const body = stripHtml(h.comment_text || h.story_text || '');
          if (!body && !title) continue;
          const text = title + ' ' + body;
          if (!/kibana/i.test(text)) continue; // guardrail
          const kind = h._tags?.includes('comment') ? 'comment' : 'story';
          all.push({
            id: 'hn-' + h.objectID,
            source: 'hackernews',
            source_url: kind === 'comment'
              ? `https://news.ycombinator.com/item?id=${h.objectID}`
              : h.url || `https://news.ycombinator.com/item?id=${h.objectID}`,
            parent_url: h.story_id && h.story_id !== Number(h.objectID)
              ? `https://news.ycombinator.com/item?id=${h.story_id}`
              : undefined,
            posted_at: h.created_at || null,
            captured_at: nowIso(),
            author: h.author || null,
            score: h.points || 0,
            kind,
            title,
            body,
            product: ['kibana'],
            versions: sniffVersions(text),
            lang: 'en',
          });
        }
        if (hits.length < 1000) break; // no more pages for this query
      } catch (e) {
        console.log('[HN] page error', page, e.message);
        break;
      }
      await sleep(250);
    }
  }
  const { added, replaced } = writeBatch(SOURCES.hackernews, all);
  console.log(`[HN] raw hits=${all.length} · added=${added} · dedup-replaced=${replaced}`);
  return all.length;
}

// ============================================================
// Reddit — .json endpoint, limited but accessible
// ============================================================
async function collectReddit() {
  console.log('[reddit] collecting…');
  const queries = [
    'https://www.reddit.com/r/elasticsearch/search.json?q=kibana&restrict_sr=1&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/r/sysadmin/search.json?q=kibana&restrict_sr=1&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/r/devops/search.json?q=kibana&restrict_sr=1&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/r/programming/search.json?q=kibana&restrict_sr=1&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/r/selfhosted/search.json?q=kibana&restrict_sr=1&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/r/dataengineering/search.json?q=kibana&restrict_sr=1&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/r/homelab/search.json?q=kibana&restrict_sr=1&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/search.json?q=kibana%20dashboard&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/search.json?q=kibana%20alternative&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/search.json?q=kibana%20slow&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/search.json?q=kibana%20upgrade%20broken&sort=relevance&t=all&limit=100',
    'https://www.reddit.com/search.json?q=kibana%20lens&sort=relevance&t=all&limit=100',
  ];
  const all = [];
  for (const base of queries) {
    let after = '';
    for (let page = 0; page < Math.min(pages, 10); page++) {
      const url = base + (after ? `&after=${after}` : '');
      try {
        const j = await getJson(url);
        const children = j?.data?.children || [];
        if (!children.length) break;
        for (const c of children) {
          const d = c.data || {};
          const title = d.title || '';
          const body = d.selftext || '';
          const text = title + ' ' + body;
          if (!/kibana/i.test(text)) continue;
          all.push({
            id: 'reddit-' + d.id,
            source: 'reddit',
            source_url: 'https://www.reddit.com' + (d.permalink || ''),
            posted_at: d.created_utc ? new Date(d.created_utc * 1000).toISOString() : null,
            captured_at: nowIso(),
            author: d.author || null,
            score: d.score || 0,
            kind: 'story',
            title,
            body,
            product: ['kibana'],
            versions: sniffVersions(text),
            lang: 'en',
            subreddit: d.subreddit || null,
            num_comments: d.num_comments || 0,
          });
        }
        after = j?.data?.after || '';
        if (!after) break;
      } catch (e) {
        console.log('[reddit] error', e.message);
        break;
      }
      await sleep(800); // reddit is sensitive
    }
  }
  const { added, replaced } = writeBatch(SOURCES.reddit, all);
  console.log(`[reddit] raw hits=${all.length} · added=${added} · dedup-replaced=${replaced}`);
  return all.length;
}

// ============================================================
// Stack Exchange — tagged questions + answers
// ============================================================
async function collectStackOverflow() {
  console.log('[so] collecting…');
  const all = [];
  const tags = [
    'kibana', 'kibana-4', 'kibana-5', 'kibana-6', 'kibana-7', 'kibana-8',
    'kibana-plugin', 'kibana-visualization', 'kibana-query-language',
    'lens-kibana', 'kibana-canvas', 'kibana-tsvb',
    'opensearch-dashboards',
  ];
  for (const tag of tags) {
    for (let page = 1; page <= pages; page++) {
      const url = `https://api.stackexchange.com/2.3/questions?order=desc&sort=votes&tagged=${tag}&site=stackoverflow&pagesize=100&page=${page}&filter=!9Z(-wwYGT`;
      try {
        const j = await getJson(url);
        const items = j?.items || [];
        if (!items.length) break;
        for (const q of items) {
          const title = q.title || '';
          const body = stripHtml(q.body || '');
          const text = title + ' ' + body;
          if (!/kibana/i.test(text)) continue;
          all.push({
            id: 'so-' + q.question_id,
            source: 'stackoverflow',
            source_url: q.link || `https://stackoverflow.com/questions/${q.question_id}`,
            posted_at: q.creation_date ? new Date(q.creation_date * 1000).toISOString() : null,
            captured_at: nowIso(),
            author: q.owner?.display_name || null,
            score: q.score || 0,
            kind: 'question',
            title,
            body,
            product: ['kibana'],
            versions: sniffVersions(text),
            lang: 'en',
            answer_count: q.answer_count || 0,
            view_count: q.view_count || 0,
            accepted: !!q.is_answered && q.accepted_answer_id != null,
          });
        }
        if (j?.has_more === false) break;
      } catch (e) {
        console.log('[so] page error', page, e.message);
        break;
      }
      await sleep(500);
    }
  }
  const { added, replaced } = writeBatch(SOURCES.stackoverflow, all);
  console.log(`[so] raw hits=${all.length} · added=${added} · dedup-replaced=${replaced}`);
  return all.length;
}

// ============================================================
// GitHub issues — multi-repo, auth-aware
// ============================================================
async function collectGithub() {
  console.log('[github] collecting…' + (GH_TOKEN ? ' (auth)' : ' (unauth — 60/hr)'));
  const all = [];
  const headers = {
    accept: 'application/vnd.github+json',
    'x-github-api-version': '2022-11-28',
  };
  if (GH_TOKEN) headers.authorization = 'Bearer ' + GH_TOKEN;

  // Repos to pull. Each entry: { owner, repo, idPrefix, productLabels }.
  const repos = [
    { owner: 'elastic', repo: 'kibana',        idPrefix: 'gh-kibana-',        product: ['kibana'] },
    { owner: 'elastic', repo: 'elasticsearch', idPrefix: 'gh-es-',            product: ['elasticsearch'] },
    { owner: 'elastic', repo: 'eui',           idPrefix: 'gh-eui-',           product: ['kibana', 'eui'] },
    { owner: 'opensearch-project', repo: 'OpenSearch-Dashboards', idPrefix: 'gh-osd-', product: ['opensearch-dashboards'] },
  ];

  // With auth: 5000 req/hr → can fetch ~50K issues if needed.
  // Without auth: 60 req/hr → pull conservatively.
  const maxPages = GH_TOKEN ? Math.min(pages, 200) : Math.min(pages, 5);

  // Flush per-repo so partial progress is preserved if the run is killed.
  const flushedSoFar = [];
  for (const r of repos) {
    const repoBatch = [];
    let pulled = 0;
    for (let page = 1; page <= maxPages; page++) {
      const url = `https://api.github.com/repos/${r.owner}/${r.repo}/issues?state=all&per_page=100&page=${page}&sort=comments&direction=desc`;
      try {
        const j = await getJson(url, { headers });
        const items = Array.isArray(j) ? j : [];
        if (!items.length) break;
        for (const i of items) {
          if (i.pull_request) continue;
          const title = i.title || '';
          const body = (i.body || '').slice(0, 4000);
          repoBatch.push({
            id: r.idPrefix + i.number,
            source: 'github',
            source_url: i.html_url,
            posted_at: i.created_at,
            captured_at: nowIso(),
            author: i.user?.login || null,
            score: i.reactions?.total_count || 0,
            kind: 'issue',
            title,
            body,
            product: r.product,
            repo: `${r.owner}/${r.repo}`,
            versions: sniffVersions(title + ' ' + body),
            lang: 'en',
            state: i.state,
            labels: (i.labels || []).map((l) => (typeof l === 'string' ? l : l.name)),
            comments: i.comments || 0,
          });
          pulled++;
        }
      } catch (e) {
        console.log(`[github ${r.repo}] page ${page} error`, e.message);
        if (/403/.test(e.message)) {
          console.log(`[github ${r.repo}] rate-limited — stopping this repo`);
          break;
        }
        if (/422/.test(e.message)) {
          // GitHub caps the Issues list at 10K results (page 100). Stop.
          console.log(`[github ${r.repo}] hit 10K-result cap at page ${page} — moving on`);
          break;
        }
      }
      // With token we can be aggressive; without, slow down.
      await sleep(GH_TOKEN ? 40 : 1200);
    }
    // Flush this repo's batch IMMEDIATELY so killing the run doesn't lose work.
    const partial = writeBatch(SOURCES.github, repoBatch);
    flushedSoFar.push(...repoBatch);
    console.log(`[github ${r.repo}] +${pulled} · flushed +${partial.added}`);
  }
  console.log(`[github] total raw hits=${flushedSoFar.length}`);
  return flushedSoFar.length;
}

// ============================================================
// Discourse — discuss.elastic.co (Elastic forum)
// ============================================================
async function collectDiscourse() {
  console.log('[discourse] collecting…');
  const all = [];
  // Discourse exposes JSON variants of every page. We pull:
  //   - search results filtered by category
  //   - latest topics in the kibana category
  // Discourse search returns ~50 per page and supports pagination via &page=
  const queries = [
    'kibana%20slow',
    'kibana%20broken',
    'kibana%20upgrade',
    'kibana%20dashboard',
    'kibana%20lens',
    'kibana%20discover',
    'kibana%20kql',
    'kibana%20alerting',
    'kibana%20space',
    'kibana%20rbac',
    'kibana%20saved%20object',
    'kibana%20pdf',
    'kibana%20csv',
    'kibana%20performance',
    'kibana%20oom',
    'kibana%20canvas',
    'kibana%20vega',
    'kibana%20plugin',
    'kibana%20apm',
    'kibana%20siem',
  ];
  for (const q of queries) {
    for (let page = 1; page <= Math.min(pages, 10); page++) {
      const url = `https://discuss.elastic.co/search.json?q=${q}&page=${page}`;
      try {
        const j = await getJson(url);
        const topics = j?.topics || [];
        const posts  = j?.posts  || [];
        if (!topics.length && !posts.length) break;
        // Each post belongs to a topic; map them together.
        const topicById = Object.fromEntries(topics.map((t) => [t.id, t]));
        for (const p of posts) {
          const t = topicById[p.topic_id] || {};
          const title = t.title || p.topic_title || '';
          const body = stripHtml(p.blurb || p.cooked || '').slice(0, 4000);
          if (!body) continue;
          all.push({
            id: 'disc-' + p.id,
            source: 'discourse',
            source_url: `https://discuss.elastic.co/t/${t.slug || 'topic'}/${p.topic_id}/${p.post_number || 1}`,
            posted_at: p.created_at || t.created_at || null,
            captured_at: nowIso(),
            author: p.username || null,
            score: (t.like_count || 0) + (t.posts_count || 0),
            kind: 'post',
            title,
            body,
            product: ['kibana'],
            versions: sniffVersions(title + ' ' + body),
            lang: 'en',
            category: t.category_id || null,
          });
        }
        if (posts.length < 10) break; // exhausted
      } catch (e) {
        console.log('[discourse] error', e.message);
        break;
      }
      await sleep(400);
    }
  }
  const { added, replaced } = writeBatch(SOURCES.discourse, all);
  console.log(`[discourse] raw hits=${all.length} · added=${added} · dedup-replaced=${replaced}`);
  return all.length;
}

// ============================================================
// Driver
// ============================================================
const start = Date.now();
const want = (src) => wanted.includes('all') || wanted.includes(src);
const results = {};
if (want('hn'))        results.hackernews    = await collectHackerNews().catch((e) => { console.log('HN failed', e); return 0; });
if (want('reddit'))    results.reddit        = await collectReddit().catch((e) => { console.log('Reddit failed', e); return 0; });
if (want('so'))        results.stackoverflow = await collectStackOverflow().catch((e) => { console.log('SO failed', e); return 0; });
if (want('github'))    results.github        = await collectGithub().catch((e) => { console.log('GH failed', e); return 0; });
if (want('discourse')) results.discourse     = await collectDiscourse().catch((e) => { console.log('Discourse failed', e); return 0; });

const elapsed = ((Date.now() - start) / 1000).toFixed(1);
console.log('---');
console.log('DONE in', elapsed, 's');
for (const [k, v] of Object.entries(results)) console.log('  ', k.padEnd(16), v);
const grandTotal = Object.values(results).reduce((a, b) => a + b, 0);
console.log('  TOTAL raw hits (with dups):', grandTotal);
