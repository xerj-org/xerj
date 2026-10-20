// node --test tools/corpus-hub-site/test_hub_traffic.mjs  (Node 18+; no dependencies)
// Traffic panel: daily-salted unique views, referrer hosts, pack-pull deltas,
// /api/hub-traffic and the /corpus/* view counter against an in-memory R2.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dailyHash, refHost, rollupViews, packSlug, snapshotFromReleases, pullsBetween, recordView } from '../../functions/_lib/hub-traffic.js';
import { onRequest as traffic, ensureSnapshot, _resetTrafficCaches } from '../../functions/api/hub-traffic.js';
import { _resetCaches } from '../../functions/api/hub-usage.js';
import { onRequest as page } from '../../functions/corpus/[[path]].js';
import { onRequest as code } from '../../functions/api/v1/code/[slug].js';

// No network in tests: the GitHub snapshot sees an empty release list.
globalThis.fetch = async () => new Response('[]');

function r2() {
  const m = new Map();
  const obj = (k) => ({ key: k, text: async () => m.get(k).body, customMetadata: m.get(k).meta });
  return {
    m,
    async get(k) { return m.has(k) ? obj(k) : null; },
    async put(k, body, o = {}) { m.set(k, { body: String(body), meta: o.customMetadata || {} }); },
    async list({ prefix }) { return { objects: [...m.keys()].filter((k) => k.startsWith(prefix)).map(obj), truncated: false }; },
  };
}
const ASSETS = { fetch: async () => new Response(JSON.stringify({ corpus: ['lz4', 'zstd'] })) };
const req = (path, headers = {}) => new Request(`https://hub.xerj.org${path}`, { headers: { 'user-agent': 'claude-code/2.0', ...headers } });
const view = (env, path, ip, headers = {}) => page({ request: req(path, { 'CF-Connecting-IP': ip, ...headers }), env, next: async () => new Response('<html>', { status: 200 }) });

test('daily hash: same source same day matches, next day does not, IPv6 /64 is one source, no IP inside', async () => {
  const a = await dailyHash('1.2.3.4', '2026-10-19', 's'), b = await dailyHash('1.2.3.4', '2026-10-19', 's');
  assert.equal(a, b); assert.equal(a.length, 16); assert.ok(!a.includes('1.2.3.4'));
  assert.notEqual(a, await dailyHash('1.2.3.4', '2026-10-20', 's'));
  assert.notEqual(a, await dailyHash('1.2.3.4', '2026-10-19', 'other-salt'));
  assert.equal(await dailyHash('2001:db8:1:2::1', 'd', 's'), await dailyHash('2001:db8:1:2:ffff::9', 'd', 's'));
});

test('refHost keeps the external host only', () => {
  assert.equal(refHost('https://www.google.com/search?q=secret'), 'google.com');
  assert.equal(refHost('https://hub.xerj.org/index.html'), '');
  assert.equal(refHost('https://abc.xerj-hub.pages.dev/'), '');
  assert.equal(refHost('http://10.1.2.3/x'), '');
  assert.equal(refHost('not a url'), '');
});

test('rollupViews counts distinct daily hashes and drops them', () => {
  const r = rollupViews([{ corpus: 'lz4', u: 'a', a: 'codex' }, { corpus: 'lz4', u: 'a', a: 'codex' }, { corpus: 'lz4', u: 'b', a: 'browser', f: 'github.com' }]);
  assert.deepEqual(r, { lz4: { v: 3, vu: 2, a: { codex: 2, browser: 1 }, f: { 'github.com': 1 } } });
});

test('pack pulls: slug from asset, growth per day, a new release counts whole', () => {
  assert.equal(packSlug('lz4-0774d055.hubpack.tar.zst'), 'lz4');
  assert.equal(packSlug('dnscrypt-proxy-ade0c1d6.hubpack.tar.zst'), 'dnscrypt-proxy');
  assert.equal(packSlug('lz4-0774d055.hubpack.tar.zst.sig'), '');
  const snap = snapshotFromReleases([
    { tag_name: 'pack-lz4-0774d055', assets: [{ name: 'lz4-0774d055.hubpack.tar.zst', download_count: 41 }, { name: 'lz4-0774d055.hubpack.tar.zst.sig', download_count: 37 }] },
    { tag_name: 'v1.0.0', assets: [{ name: 'xerj.tar.gz', download_count: 999 }] },
  ]);
  assert.deepEqual(snap, { 'pack-lz4-0774d055': ['lz4', 41] });
  assert.deepEqual(pullsBetween({ 'pack-lz4-0774d055': ['lz4', 30] }, { 'pack-lz4-0774d055': ['lz4', 41], 'pack-zstd-1': ['zstd', 3] }), { lz4: 11, zstd: 3 });
  assert.equal(pullsBetween(null, snap), null);
});

test('corpus page views: counted per corpus with daily uniques; crawler, opt-out, unknown corpus and non-200 are not', async () => {
  _resetCaches(); _resetTrafficCaches();
  const bucket = r2();
  const env = { HUB_USAGE: bucket, ASSETS, HUB_STARS_SALT: 'salt' };
  await view(env, '/corpus/lz4.html', '1.1.1.1', { referer: 'https://github.com/xerj-org/xerj' });
  await view(env, '/corpus/lz4', '1.1.1.1', { referer: 'https://github.com/xerj-org/xerj' });
  await view(env, '/corpus/lz4', '2.2.2.2', { referer: 'https://github.com/x', 'user-agent': 'codex_cli_rs/0.46' });
  await view(env, '/corpus/lz4', '3.3.3.3', { 'user-agent': 'Mozilla/5.0 (compatible; Googlebot/2.1)' });
  await view(env, '/corpus/lz4', '4.4.4.4', { dnt: '1' });
  await view(env, '/corpus/nope', '5.5.5.5');
  await page({ request: req('/corpus/lz4', { 'CF-Connecting-IP': '6.6.6.6' }), env, next: async () => new Response('', { status: 404 }) });
  const pv = [...bucket.m.entries()].filter(([k]) => k.startsWith('pv/'));
  assert.equal(pv.length, 3);
  assert.ok(pv.every(([k]) => k.split('/')[2] === 'lz4'));
  assert.ok(!JSON.stringify([...bucket.m.entries()]).match(/1\.1\.1\.1|2\.2\.2\.2|xerj-org\/xerj/));

  await code({ request: req('/api/v1/code/lz4?q=ratio', { 'CF-Connecting-IP': '7.7.7.7' }), env, params: { slug: 'lz4' } });
  const t = await (await traffic({ request: req('/api/hub-traffic?corpus=lz4'), env })).json();
  assert.equal(t.days.length, 14);
  const last = t.days[13];
  assert.deepEqual([last.views, last.unique_views, last.searches, last.pulls], [3, 2, 1, null]);
  assert.deepEqual(t.totals, { views: 3, unique_views: 2, pulls: null, pulls_unique: null, searches: 1 });
  assert.deepEqual(t.referrers, [{ host: 'github.com', count: 3 }]);
  assert.equal(t.recording_since, last.date); assert.equal(t.days[0].views, null);
  assert.deepEqual(t.agents.map((a) => a.family), ['claude-code', 'codex']);
});

test('referrers below k are not shown', async () => {
  _resetCaches(); _resetTrafficCaches();
  const env = { HUB_USAGE: r2(), ASSETS };
  await view(env, '/corpus/zstd', '1.1.1.1', { referer: 'https://intranet.example.com/' });
  await view(env, '/corpus/zstd', '1.1.1.2', { referer: 'https://intranet.example.com/' });
  const t = await (await traffic({ request: req('/api/hub-traffic?corpus=zstd'), env })).json();
  assert.deepEqual(t.referrers, []);
  assert.equal(t.totals.views, 2);
});

test('pulls come from two daily snapshots; one GitHub attempt per hour', async () => {
  _resetCaches(); _resetTrafficCaches();
  const bucket = r2();
  const env = { HUB_USAGE: bucket, ASSETS };
  const now = Date.now(), yday = new Date(now - 86400000).toISOString().slice(0, 10);
  bucket.m.set(`gh/${yday}.json`, { body: JSON.stringify({ 'pack-lz4-0774d055': ['lz4', 30] }), meta: {} });
  let calls = 0;
  const fake = async () => { calls++; return new Response(JSON.stringify([{ tag_name: 'pack-lz4-0774d055', assets: [{ name: 'lz4-0774d055.hubpack.tar.zst', download_count: 41 }] }])); };
  assert.equal(await ensureSnapshot(bucket, env, now, fake), 'taken');
  assert.equal(await ensureSnapshot(bucket, env, now, fake), 'have');
  assert.equal(calls, 1);
  const t = await (await traffic({ request: req('/api/hub-traffic?corpus=lz4'), env })).json();
  assert.equal(t.days[13].pulls, 11); assert.equal(t.totals.pulls, 11); assert.equal(t.totals.pulls_unique, null);
  assert.equal(t.pulls_since, t.days[13].date);
});

test('no binding: disabled, and the page is still served', async () => {
  const t = await (await traffic({ request: req('/api/hub-traffic?corpus=lz4'), env: {} })).json();
  assert.deepEqual(t, { ok: true, enabled: false });
  const r = await view({}, '/corpus/lz4', '1.1.1.1');
  assert.equal(r.status, 200);
  assert.equal(await recordView({}, req('/'), 'lz4'), 'disabled');
});
