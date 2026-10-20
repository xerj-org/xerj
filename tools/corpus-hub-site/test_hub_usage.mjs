// node --test tools/corpus-hub-site/test_hub_usage.mjs  (Node 18+; no dependencies)
// The query-template normaliser, the k-threshold, the aggregation and the
// /api/hub-usage + hosted routes against an in-memory stand-in for R2.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { templateOf, kTemplates, topicsOf, rollup, aggregate, uaFamily, recordUsage, K } from '../../functions/_lib/hub-usage.js';
import { onRequest as usage, _resetCaches } from '../../functions/api/hub-usage.js';
import { onRequest as code } from '../../functions/api/v1/code/[slug].js';
import { onRequest as def } from '../../functions/api/v1/def/[slug]/[symbol].js';

test('templateOf replaces identifiers, versions, paths, numbers and quoted strings', () => {
  const cases = [
    ['CVEs affecting tokio before 1.38.2', 'cves affecting tokio before <version>'],
    ['How does LZ4_compress_fast handle acceleration?', 'how does <ident> handle acceleration'],
    ['what does src/lib.rs do at line 42', 'what does <path> do at line <n>'],
    ['where is "no such file or directory" raised', 'where is <str> raised'],
    ["grep 'secret token value' please", 'grep <str> please'],
    ['GHSA-abcd-efgh-ijkl in serde_json 1.0.100 commit 9c28114a', '<advisory> in <ident> <version> commit <sha>'],
    ['CVE-2024-12345 fixed in v2.3?', '<advisory> fixed in <version>'],
    ['parseHeader vs HttpClient::send', '<ident> vs <ident>'],
    ['mail alice@example.com from 10.0.0.1 see https://x.example/a?b=1', 'mail <email> from <ip> see <url>'],
    ['lz4 level 9 vs 12 speed', '<corpus> level <n> vs <n> speed'],
    ['  ', ''],
  ];
  for (const [q, t] of cases) assert.equal(templateOf(q, 'lz4'), t, q);
});

test('templateOf: nothing of a raw secret-looking value survives', () => {
  const t = templateOf('token ghp_AbC123xyz password "hunter2" key=0xDEADBEEF api 4111111111111111');
  for (const bad of ['ghp_', 'hunter2', 'deadbeef', '4111']) assert.ok(!t.toLowerCase().includes(bad), t);
});

test('templateOf caps length and word count', () => {
  const t = templateOf(Array(100).fill('word').join(' '));
  assert.equal(t.split(' ').length, 16);
  assert.ok(templateOf('x'.repeat(5000)).length <= 200);
});

test('k-threshold: a template seen fewer than k times is never published', () => {
  assert.equal(K, 3);
  assert.deepEqual(kTemplates({ 'how does <ident> work': 2 }), []);
  assert.deepEqual(kTemplates({ 'how does <ident> work': 3 }), [{ template: 'how does <ident> work', count: 3 }]);
});

test('k-threshold: rare words generalise to <term>, so CVEs-for-<crate> merges', () => {
  const out = kTemplates({
    'cves affecting tokio before <version>': 1,
    'cves affecting serde before <version>': 1,
    'cves affecting rand before <version>': 1,
    'my private project codename zork': 1,
  });
  assert.deepEqual(out, [{ template: 'cves affecting <term> before <version>', count: 3 }]);
  // every literal word left was asked >= k times
  for (const w of out[0].template.split(' ').filter((w) => !w.startsWith('<'))) assert.ok(['cves', 'affecting', 'before'].includes(w));
});

test('k-threshold: k is a parameter', () => {
  assert.deepEqual(kTemplates({ 'a b': 4 }, 5), []);
  assert.equal(kTemplates({ 'a b': 5 }, 5).length, 1);
});

test('topics: term frequency over surviving templates, stopwords dropped', () => {
  const tops = topicsOf([{ template: 'how does compression ratio change with <n>', count: 5 },
                         { template: 'compression speed of <ident>', count: 3 }]);
  assert.deepEqual(tops.slice(0, 2), [{ term: 'compression', count: 8 }, { term: 'change', count: 5 }]);
  assert.ok(!tops.some((t) => ['how', 'does', 'with', 'of'].includes(t.term)));
});

test('uaFamily keeps only a coarse family', () => {
  assert.equal(uaFamily('xerj/0.9.1 (linux)'), 'xerj-cli');
  assert.equal(uaFamily('xerj-mcp/0.9'), 'xerj-mcp');
  assert.equal(uaFamily('curl/8.5.0'), 'curl');
  assert.equal(uaFamily('python-httpx/0.27'), 'python');
  assert.equal(uaFamily('Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; Claude-User/1.0; +Claude-User@anthropic.com)'), 'claude');
  assert.equal(uaFamily('claude-code/2.1.0'), 'claude-code');
  assert.equal(uaFamily('codex_cli_rs/0.46.0 (Mac OS 15.1; arm64)'), 'codex');
  assert.equal(uaFamily('GeminiCLI/0.9.0 (linux; x64)'), 'gemini-cli');
  assert.equal(uaFamily('Mozilla/5.0 (compatible; Googlebot/2.1)'), 'crawler');
  assert.equal(uaFamily('Mozilla/5.0 (compatible; ClaudeBot/1.0)'), 'crawler');
  assert.equal(uaFamily(''), 'unknown');
  assert.equal(uaFamily('SomethingElse/1'), 'other');
});

test('aggregate: 7/30-day windows, agent families, thresholded detail', () => {
  const ev = (corpus, t, a) => ({ corpus, r: 'code', t, a });
  const days = {
    '2026-10-19': rollup([ev('lz4', 'ratio of <ident>', 'curl'), ev('lz4', 'ratio of <ident>', 'xerj-cli')]),
    '2026-10-10': rollup([ev('lz4', 'ratio of <ident>', 'python'), ev('zstd', 'one off', 'curl')]),
    '2026-09-01': rollup([ev('lz4', 'ratio of <ident>', 'curl')]),   // outside 30 days
  };
  const s = aggregate(days, '2026-10-19');
  assert.equal(s.lz4.d7, 2); assert.equal(s.lz4.d30, 3); assert.equal(s.lz4.agents30, 3);
  assert.deepEqual(s.lz4.templates, [{ template: 'ratio of <ident>', count: 3 }]);
  assert.deepEqual(s.zstd.templates, []);
  assert.equal(s.zstd.d30, 1);
});

function r2() {
  const m = new Map();
  const obj = (k) => ({ key: k, text: async () => m.get(k).body, customMetadata: m.get(k).meta });
  return {
    m, writes: 0,
    async get(k) { return m.has(k) ? obj(k) : null; },
    async put(k, body, o = {}) { this.writes++; m.set(k, { body: String(body), meta: o.customMetadata || {} }); },
    async list({ prefix }) { return { objects: [...m.keys()].filter((k) => k.startsWith(prefix)).map(obj), truncated: false }; },
  };
}
const ASSETS = { fetch: async () => new Response(JSON.stringify({ corpus: ['lz4'] })) };
const req = (path, headers = {}) => new Request(`https://hub.xerj.org${path}`, { headers: { 'CF-Connecting-IP': '1.2.3.4', 'user-agent': 'curl/8', ...headers } });

test('no binding: usage says disabled; hosted route still answers', async () => {
  const r = await usage({ request: req('/api/hub-usage?corpus=lz4'), env: {} });
  assert.deepEqual(await r.json(), { ok: true, enabled: false });
  const h = await code({ request: req('/api/v1/code/lz4?q=x'), env: { ASSETS }, params: { slug: 'lz4' } });
  assert.equal(h.status, 503);
  assert.equal((await h.json()).hosted, false);
});

test('hosted route records template only; usage publishes after k; opt-out respected', async () => {
  _resetCaches();
  const bucket = r2();
  const env = { HUB_USAGE: bucket, ASSETS, HUB_USAGE_DETAIL_SIGNIN: '0' };
  const ask = (q, headers) => code({ request: req(`/api/v1/code/lz4?q=${encodeURIComponent(q)}`, headers), env, params: { slug: 'lz4' } });
  assert.equal((await ask('compression ratio of LZ4_compress_fast at level 9')).status, 503);
  const evs = [...bucket.m.entries()].filter(([k]) => k.startsWith('ev/'));
  assert.equal(evs.length, 1);
  assert.deepEqual(evs[0][1].meta, { r: 'code', t: 'compression ratio of <ident> at level <n>', a: 'curl' });
  assert.ok(![...bucket.m.values()].some((v) => JSON.stringify(v).includes('LZ4_compress_fast') || JSON.stringify(v).includes('1.2.3.4')));

  await ask('secret', { 'x-xerj-disable-feedback': 'true' });
  await ask('secret', { dnt: '1' });
  assert.equal([...bucket.m.keys()].filter((k) => k.startsWith('ev/')).length, 1);

  let u = await (await usage({ request: req('/api/hub-usage?corpus=lz4'), env })).json();
  assert.equal(u.d7, 1); assert.deepEqual(u.detail.templates, []);
  await ask('compression ratio of LZ4_compress_default at level 1');
  await ask('compression ratio of LZ4_compress_HC at level 12');
  _resetCaches();
  u = await (await usage({ request: req('/api/hub-usage?corpus=lz4'), env })).json();
  assert.equal(u.d30, 3);
  assert.deepEqual(u.detail.templates, [{ template: 'compression ratio of <ident> at level <n>', count: 3 }]);
  assert.deepEqual(u.detail.topics.map((t) => t.term), ['compression', 'level', 'ratio']);
  const top = await (await usage({ request: req('/api/hub-usage'), env })).json();
  assert.deepEqual(top.top, [{ corpus: 'lz4', d7: 3, d30: 3 }]);
});

test('unknown corpus is a 404 and is not counted; def counts the symbol as <symbol>', async () => {
  _resetCaches();
  const bucket = r2();
  const env = { HUB_USAGE: bucket, ASSETS };
  const r = await code({ request: req('/api/v1/code/nope?q=x'), env, params: { slug: 'nope' } });
  assert.equal(r.status, 404);
  await def({ request: req('/api/v1/def/lz4/LZ4_VERSION_RELEASE'), env, params: { slug: 'lz4', symbol: 'LZ4_VERSION_RELEASE' } });
  const evs = [...bucket.m.entries()].filter(([k]) => k.startsWith('ev/'));
  assert.equal(evs.length, 1);
  assert.equal(evs[0][1].meta.t, '<symbol>');
});

test('detail is behind sign-in by default; summary stays public; a session opens it', async () => {
  _resetCaches();
  const env = { HUB_USAGE: r2(), ASSETS };
  let u = await (await usage({ request: req('/api/hub-usage?corpus=lz4'), env })).json();
  assert.equal(u.detail, null); assert.equal(u.detail_requires_signin, true); assert.equal(u.signin_enabled, false); assert.equal(u.d30, 0);
  const secret = 's'.repeat(40);
  const { makeSessionCookie } = await import('../../functions/_lib/session.js');
  const set = await makeSessionCookie(secret, { provider: 'github', id: '1', login: 'ann', name: 'Ann', avatar: '' });
  const cookie = set.split(';')[0];
  const env2 = { ...env, HUB_SESSION_SECRET: secret, GITHUB_OAUTH_CLIENT_ID: 'i', GITHUB_OAUTH_CLIENT_SECRET: 's' };
  u = await (await usage({ request: req('/api/hub-usage?corpus=lz4', { cookie }), env: env2 })).json();
  assert.deepEqual(u.detail, { templates: [], topics: [] }); assert.equal(u.signin_enabled, true);
  u = await (await usage({ request: req('/api/hub-usage?corpus=lz4'), env: env2 })).json();
  assert.equal(u.detail, null);
});

test('record: daily cap and missing binding never throw', async () => {
  assert.equal(await recordUsage({}, req('/'), { corpus: 'lz4', route: 'code', query: 'x' }), 'disabled');
  const bucket = r2();
  bucket.m.set(`meta/count-${new Date().toISOString().slice(0, 10)}`, { body: '5000', meta: {} });
  assert.equal(await recordUsage({ HUB_USAGE: bucket }, req('/', { 'CF-Connecting-IP': '9.9.9.9' }), { corpus: 'lz4', route: 'code', query: 'x' }), 'capped');
});
