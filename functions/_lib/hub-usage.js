// Hub usage: privacy-safe, aggregate-only counts of what agents ask the hosted
// Corpus Hub API about each corpus. Shared by the hosted routes that RECORD a
// request (functions/api/v1/code/[slug].js, def/[slug]/[[symbol]].js, search.js)
// and the route that PUBLISHES aggregates (functions/api/hub-usage.js).
// Not a route itself: it exports no onRequest handler.
//
// ─────────────────────────────── WHAT IS STORED ─────────────────────────
// One R2 object per hosted request (binding HUB_USAGE → bucket xerj-hub-usage):
//   ev/<YYYY-MM-DD>/<corpus>/<ms>-<rand>   empty body; customMetadata
//        r = route (code | def | search)
//        t = the query TEMPLATE (templateOf): identifiers, versions, paths,
//            numbers, quoted strings, URLs, e-mails, advisory ids replaced by
//            typed placeholders, lower-cased, at most 16 words / 200 chars
//        a = user-agent FAMILY (uaFamily): "xerj-cli", "curl", "python", …
// NOT stored: the raw query, the IP address, the full user agent, cookies, any
// session or user id. The per-source rate limit lives in isolate memory only.
//   agg/<YYYY-MM-DD>.json   the day's events rolled up per corpus, written on
//                           the first read after the day closes
//   meta/count-<day>        events accepted that day (daily cap)
// Bucket lifecycle: ev/ and agg/ expire after 40 days, meta/ after 2 days, so
// nothing older than the 30-day window survives.
//
// ─────────────────────────────── WHAT IS PUBLISHED ──────────────────────
// aggregate(): per corpus, request counts over 7 and 30 days, the number of
// distinct user-agent families, and — over the 30-day window — only templates
// seen at least K times, after every word seen in fewer than K requests has
// been generalised to <term>. So every literal word on the page was asked by at
// least K requests, and no single query is ever shown. Topics are the most
// frequent of those surviving words (term frequency, stopwords removed).
//
// ─────────────────────────────── OPT-OUT ────────────────────────────────
// A request carrying `X-Xerj-Disable-Feedback: 1|true|yes`, `DNT: 1` or
// `Sec-GPC: 1` is answered normally and not recorded. A hosted client that
// honours XERJ_DISABLE_FEEDBACK / --disable-feedback must send the first one.

export const K = 3;                 // publish a template only when seen >= K times
export const WINDOW_DAYS = 30;
export const DAILY_CAP = 5000;      // ×2 Class-A writes = 300k/mo, inside R2's free 1M
export const MAX_TEMPLATE_WORDS = 16;
export const MAX_TEMPLATE_CHARS = 200;
export const SEARCH_CORPUS = '_search';   // hub-wide search has no single corpus
const SLUG_RE = /^[a-z0-9][a-z0-9._-]{0,99}$/;

// ── Query → template ────────────────────────────────────────────────────────
const PH = (name) => ` \u0000${name}\u0001 `;           // protected during tokenising
const CODE_EXT = /\.(rs|py|pyi|js|mjs|cjs|ts|tsx|jsx|go|c|h|cc|cpp|hpp|cxx|java|kt|swift|rb|php|cs|scala|md|rst|toml|json|ya?ml|txt|sh|html?|css|sql|proto|lock|cfg|ini|xml|zig|lua|ex|exs|erl)$/i;

function classify(tok, slug) {
  if (/^\u0000[a-z]+\u0001$/.test(tok)) return `<${tok.slice(1, -1)}>`;
  const core = tok.replace(/^[\s"'`([{<,;:!?]+|[\s"'`)\]}>,;:!?.]+$/g, '');
  if (!core) return null;
  if (slug && core.toLowerCase() === slug) return '<corpus>';
  if (/^[\^~>=<]*v?\d+(\.\d+){1,3}([-+][0-9a-z.+-]+)?$/i.test(core)) return '<version>';
  if (/^[0-9a-f]{7,64}$/i.test(core) && /\d/.test(core) && /[a-f]/i.test(core)) return '<sha>';
  if (/^[-+]?\d+([.,]\d+)*(%|[kmg]i?b?|ms|s|x)?$/i.test(core)) return '<n>';
  if (/[\\/]/.test(core) || CODE_EXT.test(core)) return '<path>';
  if (core.length > 30
      || /::|->|[_#()[\]{}=*&|$@]|\.\w/.test(core)          // snake_case, a::b, a.b, f()
      || /[a-z][A-Z]/.test(core) || /^[A-Z][a-z0-9]+[A-Z]/.test(core)   // camelCase, PascalCase
      || (/\d/.test(core) && /[a-z]/i.test(core))) return '<ident>';      // lz4, x86, sha256
  const w = core.toLowerCase().replace(/[^\p{L}\p{N}'-]+/gu, '');
  return w || null;
}

/** Normalise one query into a template. Pure; never throws. */
export function templateOf(query, slug = '') {
  let s = String(query ?? '').slice(0, 2000).replace(/[\u0000-\u001f\u007f]+/g, ' ');
  s = s.replace(/\b(?:https?|ftp|file|git|ssh):\/\/\S+/gi, PH('url'))
       .replace(/[\w.+-]+@[\w-]+(\.[\w-]+)+/g, PH('email'))
       .replace(/"[^"\n]{0,500}"|“[^”\n]{0,500}”|`[^`\n]{0,500}`/g, PH('str'))
       .replace(/(^|[\s(=:,])'[^'\n]{0,500}'(?=$|[\s),.;:?!])/g, (_, p) => p + PH('str'))
       .replace(/\b(?:CVE-\d{4}-\d{4,}|GHSA(?:-[a-z0-9]{4}){3}|RUSTSEC-\d{4}-\d{4}|PYSEC-\d{4}-\d+|GO-\d{4}-\d+|OSV-\d{4}-\d+)\b/gi, PH('advisory'))
       .replace(/\b\d{1,3}(\.\d{1,3}){3}(\/\d{1,2})?\b/g, PH('ip'))
       .replace(/\b[0-9a-f]{1,4}(:[0-9a-f]{0,4}){4,7}\b/gi, PH('ip'));
  slug = String(slug || '').toLowerCase();
  const out = [];
  for (const tok of s.split(/\s+/)) {
    const c = classify(tok, slug);
    if (!c) continue;
    if (c.startsWith('<') && out[out.length - 1] === c) continue;   // <n> <n> → <n>
    out.push(c);
    if (out.length >= MAX_TEMPLATE_WORDS) break;
  }
  let t = out.join(' ');
  while (t.length > MAX_TEMPLATE_CHARS) t = t.slice(0, t.lastIndexOf(' ') > 0 ? t.lastIndexOf(' ') : MAX_TEMPLATE_CHARS);
  return t;
}

// ── User agent → family (the full string is never stored) ──────────────────
// Coding agents first (their UA often also says Mozilla or node), then crawlers,
// then generic HTTP clients. Order matters: the first match wins.
const FAMILIES = [
  [/claude-code|claude_code/i, 'claude-code'], [/codex/i, 'codex'], [/gemini[-_ ]?cli/i, 'gemini-cli'],
  [/cursor/i, 'cursor'], [/copilot/i, 'copilot'], [/aider/i, 'aider'], [/cline|roo-?code/i, 'cline'],
  [/windsurf|codeium/i, 'windsurf'], [/opencode/i, 'opencode'], [/goose/i, 'goose'],
  [/xerj[-_ ]?mcp/i, 'xerj-mcp'], [/^xerj\b|xerj-hub|xerj\//i, 'xerj-cli'],
  [/claude-user|claude-searchbot/i, 'claude'], [/chatgpt-user|oai-searchbot/i, 'chatgpt'],
  [/perplexity-user/i, 'perplexity'],
  [/bot\b|bot\/|crawl|spider|slurp|bingpreview|facebookexternalhit|headless/i, 'crawler'],
  [/anthropic/i, 'claude'], [/openai/i, 'chatgpt'], [/gemini|google-extended|googleother/i, 'gemini'],
  [/perplexity/i, 'perplexity'],
  [/^curl\//i, 'curl'], [/^wget/i, 'wget'],
  [/python|httpx|aiohttp|urllib/i, 'python'], [/node|undici|axios|deno|bun\//i, 'node-js'],
  [/go-http-client/i, 'go'], [/reqwest|hyper|ureq/i, 'rust'],
  [/mozilla|webkit|gecko|chrome|safari/i, 'browser'],
];
export function uaFamily(ua) {
  ua = String(ua || '').trim();
  if (!ua) return 'unknown';
  for (const [re, fam] of FAMILIES) if (re.test(ua)) return fam;
  return 'other';
}

export function optedOut(request) {
  const h = (n) => (request.headers.get(n) || '').trim().toLowerCase();
  return /^(1|true|yes|on)$/.test(h('x-xerj-disable-feedback')) || h('dnt') === '1' || h('sec-gpc') === '1';
}

// ── Recording ───────────────────────────────────────────────────────────────
// In-isolate limiter: at most RATE_PER_MIN recorded requests per source per
// minute. The key is a hash of the IPv4 address / IPv6 /64, held in memory
// only, never written; the map is dropped every minute.
const RATE_PER_MIN = 30;
let rateMinute = 0, rateMap = new Map();
export async function overRate(request) {
  const ip = String(request.headers.get('CF-Connecting-IP') || '');
  const key = ip.includes(':') ? ip.split(':').slice(0, 4).join(':') : ip;
  const d = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(key));
  const h = [...new Uint8Array(d).slice(0, 8)].map((b) => b.toString(16).padStart(2, '0')).join('');
  const m = Math.floor(Date.now() / 60000);
  if (m !== rateMinute) { rateMinute = m; rateMap = new Map(); }
  const n = (rateMap.get(h) || 0) + 1;
  rateMap.set(h, n);
  return n > RATE_PER_MIN;
}

export const day = (ms = Date.now()) => new Date(ms).toISOString().slice(0, 10);

/**
 * Record one hosted request. Never throws and never blocks the answer: call it
 * through waitUntil. Returns why it did or did not record (for tests).
 */
export async function recordUsage(env, request, { corpus, route, query }) {
  try {
    const bucket = env && env.HUB_USAGE;
    if (!bucket) return 'disabled';
    if (optedOut(request)) return 'opted-out';
    corpus = String(corpus || '').toLowerCase();
    if (corpus !== SEARCH_CORPUS && !SLUG_RE.test(corpus)) return 'bad-corpus';
    if (await overRate(request)) return 'rate-limited';
    const today = day();
    const cntKey = `meta/count-${today}`;
    let n = 0;
    try { const c = await bucket.get(cntKey); if (c) n = parseInt(await c.text(), 10) || 0; } catch (_) { n = 0; }
    if (n >= DAILY_CAP) return 'capped';
    const meta = {
      r: String(route || '').slice(0, 16),
      t: route === 'def' ? '<symbol>' : templateOf(query, corpus === SEARCH_CORPUS ? '' : corpus),
      a: uaFamily(request.headers.get('user-agent')),
    };
    const rand = Math.random().toString(36).slice(2, 8);
    await bucket.put(`ev/${today}/${corpus}/${Date.now()}-${rand}`, '', { customMetadata: meta });
    await bucket.put(cntKey, String(n + 1));
    return 'recorded';
  } catch (e) {
    console.log('[hub-usage] record failed:', String(e));
    return 'error';
  }
}

// ── Aggregation (pure, tested) ──────────────────────────────────────────────
const STOP = new Set(('a an and are as at be been but by can do does for from has have how i if in into is it its '
  + 'me my no not of on or our should so that the their them then there these this those to use used using was '
  + 'we what when where which who why will with without you your about any all also between both each get got '
  + 'more most other over same some such than too very via vs just like does did make makes way ways there\'s '
  + 'what\'s how\'s it\'s don\'t can\'t isn\'t versus etc per one two before after since until onto').split(' '));

/** Generalise words seen in fewer than k templates, then count and keep templates seen >= k times. */
export function kTemplates(counts, k = K) {
  const df = new Map();
  for (const [t, n] of Object.entries(counts)) {
    for (const w of new Set(t.split(' '))) if (w && !w.startsWith('<')) df.set(w, (df.get(w) || 0) + n);
  }
  const merged = new Map();
  for (const [t, n] of Object.entries(counts)) {
    const out = [];
    for (const w of t.split(' ')) {
      if (!w) continue;
      const g = w.startsWith('<') || (df.get(w) || 0) >= k ? w : '<term>';
      if (g.startsWith('<') && out[out.length - 1] === g) continue;
      out.push(g);
    }
    const key = out.join(' ');
    if (key) merged.set(key, (merged.get(key) || 0) + n);
  }
  return [...merged].filter(([, n]) => n >= k).sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))
    .map(([template, count]) => ({ template, count }));
}

/** Topic terms: most frequent non-stopword words across the k-surviving templates. */
export function topicsOf(templates, limit = 8) {
  const tf = new Map();
  for (const { template, count } of templates)
    for (const w of new Set(template.split(' ')))
      if (w && !w.startsWith('<') && w.length >= 3 && !STOP.has(w)) tf.set(w, (tf.get(w) || 0) + count);
  return [...tf].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)).slice(0, limit)
    .map(([term, count]) => ({ term, count }));
}

/** Roll a list of events {corpus, r, t, a} up into {corpus: {n, a:{}, r:{}, t:{}}}. */
export function rollup(events) {
  const out = {};
  for (const e of events) {
    const c = (out[e.corpus] ||= { n: 0, a: {}, r: {}, t: {} });
    c.n++;
    c.a[e.a || 'unknown'] = (c.a[e.a || 'unknown'] || 0) + 1;
    c.r[e.r || '?'] = (c.r[e.r || '?'] || 0) + 1;
    if (e.t) c.t[e.t] = (c.t[e.t] || 0) + 1;
  }
  return out;
}

/**
 * Publishable stats from daily rollups ({ 'YYYY-MM-DD': rollup }) as of `today`.
 * summary: counts only. detail: k-thresholded templates and topics.
 */
export function aggregate(days, today, k = K) {
  const t0 = Date.parse(today + 'T00:00:00Z');
  const per = {};
  for (const [d, roll] of Object.entries(days)) {
    const age = Math.round((t0 - Date.parse(d + 'T00:00:00Z')) / 86400000);
    if (!(age >= 0 && age < WINDOW_DAYS)) continue;
    for (const [corpus, c] of Object.entries(roll || {})) {
      const p = (per[corpus] ||= { d7: 0, d30: 0, a: {}, r: {}, t: {} });
      p.d30 += c.n || 0;
      if (age < 7) p.d7 += c.n || 0;
      for (const f of ['a', 'r', 't']) for (const [x, n] of Object.entries(c[f] || {})) p[f][x] = (p[f][x] || 0) + n;
    }
  }
  const stats = {};
  for (const [corpus, p] of Object.entries(per)) {
    const templates = kTemplates(p.t, k);
    stats[corpus] = {
      d7: p.d7, d30: p.d30,
      agents30: Object.keys(p.a).filter((f) => f !== 'unknown').length,
      agent_families: p.a, routes: p.r,
      templates: templates.slice(0, 10), topics: topicsOf(templates),
    };
  }
  return stats;
}
